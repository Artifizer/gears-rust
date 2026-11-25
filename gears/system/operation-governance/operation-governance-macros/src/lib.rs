//! `#[derive(GovernedFacts)]` — one declaration per Fact drives everything.
//!
//! On a Governed Operation struct (derived from `GovernedOperationV1`) every
//! field is a Fact and must say its tier:
//!
//! ```ignore
//! #[derive(Default, GovernedFacts)]
//! #[governed_facts(replaces(title = "name"))]          // optional: renamed Facts
//! pub struct CreateWidgetV2 {
//!     #[fact(request)]                                  pub name: String,
//!     #[fact(computed, approver_visible)]               pub size: u64,
//!     #[fact(costly, approver_visible, binding = false)] pub owner_count: u64,
//! }
//! ```
//!
//! It generates, for a struct `X`:
//!
//! - `X::OG_FACTS` and `X::og_traits()` — pass `traits = X::og_traits()` to
//!   `gts_type_schema`, so Fact metadata is never written as JSON by hand.
//! - `trait XFacts` — one method per Fact, typed by the field type, and
//!   **shaped by the tier**:
//!   - `request` / `computed`: `fn fact(input: &Self::Input) -> FactValue<T>`.
//!     Synchronous (no `.await` possible) and given only `Self::Input`, the
//!     operation's data, never `&self`, so a repository or connection the
//!     provider holds is out of reach.
//!   - `costly`: `async fn fact(&self) -> FactValue<T>`, free to do I/O.
//! - `struct XProvider<P>(pub P)` — implements `FactProvider` for any
//!   `P: XFacts`: the name dispatch, value writing, and "undeclared" handling
//!   are generated, never hand-written.
//! - A `#[cfg(test)]` test that the GTS schema's Facts and traits are exactly
//!   these (it catches `traits = json!(…)` written by hand instead of
//!   `og_traits()`). A `properties = "…"` name that is not a field is already a
//!   compile error in `gts_type_schema`, and the schema always holds every field.

use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, LitBool, LitStr, parse_macro_input};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tier {
    Request,
    Computed,
    Costly,
}

impl Tier {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Computed => "computed",
            Self::Costly => "costly",
        }
    }
    const fn is_costly(self) -> bool {
        matches!(self, Self::Costly)
    }
}

/// One field: its flags mirror the GTS trait JSON one to one.
#[allow(clippy::struct_excessive_bools)]
struct Fact {
    name: String,
    ty: syn::Type,
    tier: Tier,
    sensitive: bool,
    approver_visible: bool,
    binding: bool,
    doc: Vec<syn::Attribute>,
    ident: syn::Ident,
}

/// Derive a typed, tier-checked Fact contract for a Governed Operation type.
#[proc_macro_derive(GovernedFacts, attributes(fact, governed_facts))]
pub fn derive_governed_facts(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new(
            input.span(),
            "GovernedFacts can only be derived for a struct",
        ));
    };
    let Fields::Named(named_fields) = &data.fields else {
        return Err(syn::Error::new(
            input.span(),
            "GovernedFacts needs a struct with named fields: each field is one Fact",
        ));
    };
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "a Governed Operation type cannot be generic",
        ));
    }

    let mut facts = Vec::new();
    for field in &named_fields.named {
        facts.push(parse_fact(field)?);
    }
    if facts.is_empty() {
        return Err(syn::Error::new(
            input.span(),
            "a Governed Operation declares at least one Fact",
        ));
    }
    let replaces = parse_replaces(input, &facts)?;

    let og = quote!(::operation_governance_sdk);
    let vis = &input.vis;
    let ty = &input.ident;
    let trait_ident = format_ident!("{}Facts", ty);
    let provider_ident = format_ident!("{}Provider", ty);
    let has_input = facts.iter().any(|f| !f.tier.is_costly());

    // --- traits -----------------------------------------------------------
    let fact_names: Vec<&String> = facts.iter().map(|f| &f.name).collect();
    let trait_entries = facts.iter().map(|f| {
        let name = &f.name;
        let tier = format_ident!("{}", f.tier.as_str().to_upper_camel_case());
        let (sensitive, approver_visible, binding) = (f.sensitive, f.approver_visible, f.binding);
        quote! {
            facts.insert(
                ::std::string::String::from(#name),
                #og::FactTraits {
                    tier: #og::FactTier::#tier,
                    sensitive: #sensitive,
                    approver_visible: #approver_visible,
                    binding: #binding,
                },
            );
        }
    });
    let replace_entries = replaces.iter().map(|(old, new)| {
        quote! {
            replaces.insert(::std::string::String::from(#old), ::std::string::String::from(#new));
        }
    });

    // --- provider trait ---------------------------------------------------
    let input_items = has_input.then(|| {
        quote! {
            /// Data the request and computed Facts are built from: the
            /// operation's input or an object already in memory. It is all a
            /// cheap Fact can see.
            type Input: ::core::marker::Sync;
            /// The data request and computed Facts are built from.
            fn input(&self) -> &Self::Input;
        }
    });
    let methods = facts.iter().map(|f| {
        let (ident, fty, doc) = (&f.ident, &f.ty, &f.doc);
        if f.tier.is_costly() {
            quote! {
                #(#doc)*
                async fn #ident(&self) -> #og::FactValue<#fty>;
            }
        } else {
            quote! {
                #(#doc)*
                fn #ident(input: &Self::Input) -> #og::FactValue<#fty>;
            }
        }
    });

    // --- dispatch ---------------------------------------------------------
    let arms = facts.iter().map(|f| {
        let (ident, name) = (&f.ident, &f.name);
        let value = if f.tier.is_costly() {
            quote!(self.0.#ident().await)
        } else {
            quote!(<P as #trait_ident>::#ident(<P as #trait_ident>::input(&self.0)))
        };
        quote!(#name => #og::typed::write_fact(out, fact, #value)?,)
    });

    let ty_name = ty.to_string();
    let test_mod = format_ident!("__og_governed_facts_{}", ty.to_string().to_snake_case());
    let trait_doc = format!(
        "The Facts of [`{ty}`], one typed method each. Request and computed \
         Facts are synchronous and see only `Self::Input`; costly Facts are \
         async. Wrap an implementation in [`{provider_ident}`] to pass it to \
         the Governance Enforcer."
    );
    let provider_doc = format!(
        "Adapts any [`{trait_ident}`] to `FactProvider`; the dispatch by Fact ID is generated."
    );

    Ok(quote! {
        impl #ty {
            /// Fact IDs this Governed Operation declares, in declaration order.
            pub const OG_FACTS: &'static [&'static str] = &[#(#fact_names),*];

            /// Traits of this type, for `gts_type_schema(traits = …)`.
            #[must_use]
            pub fn og_traits() -> #og::GovernedOperationTraits {
                let mut facts = ::std::collections::BTreeMap::new();
                #(#trait_entries)*
                #[allow(unused_mut)]
                let mut replaces = ::std::collections::BTreeMap::new();
                #(#replace_entries)*
                #og::GovernedOperationTraits { facts, replaces }
            }
        }

        #[doc = #trait_doc]
        #[#og::typed::async_trait]
        #vis trait #trait_ident: ::core::marker::Send + ::core::marker::Sync {
            #input_items
            #(#methods)*
        }

        #[doc = #provider_doc]
        #vis struct #provider_ident<P>(pub P);

        #[#og::typed::async_trait]
        impl<P: #trait_ident> #og::FactProvider for #provider_ident<P> {
            type Facts = #ty;

            async fn provide(
                &self,
                need: &#og::FactSet,
                out: &mut #og::FactWriter<'_>,
            ) -> ::core::result::Result<(), #og::FactError> {
                for fact in need {
                    match fact.as_str() {
                        #(#arms)*
                        other => {
                            return ::core::result::Result::Err(
                                #og::FactError::Undeclared(::std::string::ToString::to_string(other)),
                            );
                        }
                    }
                }
                ::core::result::Result::Ok(())
            }
        }

        #[cfg(test)]
        #[allow(non_snake_case, clippy::unwrap_used, clippy::expect_used, clippy::panic)]
        mod #test_mod {
            #[test]
            fn declared_facts_match_the_gts_schema() {
                #og::typed::assert_schema_matches::<super::#ty>(
                    #ty_name,
                    super::#ty::OG_FACTS,
                    &super::#ty::og_traits(),
                );
            }
        }
    })
}

fn parse_fact(field: &syn::Field) -> syn::Result<Fact> {
    let ident = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new(field.span(), "a Fact needs a name"))?;
    let mut tier: Option<Tier> = None;
    let mut sensitive = false;
    let mut approver_visible = false;
    let mut binding = true;
    let mut seen = false;

    for attr in field.attrs.iter().filter(|a| a.path().is_ident("fact")) {
        if seen {
            return Err(syn::Error::new(
                attr.span(),
                "duplicate #[fact(...)] attribute",
            ));
        }
        seen = true;
        attr.parse_nested_meta(|meta| {
            let set_tier = |tier_slot: &mut Option<Tier>, t: Tier| {
                if tier_slot.replace(t).is_some() {
                    Err(meta.error("a Fact has exactly one tier: request, computed, or costly"))
                } else {
                    Ok(())
                }
            };
            if meta.path.is_ident("request") {
                set_tier(&mut tier, Tier::Request)
            } else if meta.path.is_ident("computed") {
                set_tier(&mut tier, Tier::Computed)
            } else if meta.path.is_ident("costly") {
                set_tier(&mut tier, Tier::Costly)
            } else if meta.path.is_ident("sensitive") {
                sensitive = true;
                Ok(())
            } else if meta.path.is_ident("approver_visible") {
                approver_visible = true;
                Ok(())
            } else if meta.path.is_ident("binding") {
                let value: LitBool = meta.value()?.parse()?;
                binding = value.value;
                Ok(())
            } else {
                Err(meta.error(
                    "unknown #[fact] key; expected request, computed, costly, sensitive, \
                     approver_visible, or binding = <bool>",
                ))
            }
        })?;
    }

    let tier = tier.ok_or_else(|| {
        syn::Error::new(
            ident.span(),
            format!(
                "Fact `{ident}` must declare its tier: #[fact(request)], #[fact(computed)], \
                 or #[fact(costly)]"
            ),
        )
    })?;
    let doc = field
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .cloned()
        .collect();
    Ok(Fact {
        name: ident.to_string(),
        ty: field.ty.clone(),
        tier,
        sensitive,
        approver_visible,
        binding,
        doc,
        ident,
    })
}

/// `#[governed_facts(replaces(old_name = "new_name", ...))]`
fn parse_replaces(input: &DeriveInput, facts: &[Fact]) -> syn::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for attr in input
        .attrs
        .iter()
        .filter(|a| a.path().is_ident("governed_facts"))
    {
        attr.parse_nested_meta(|meta| {
            if !meta.path.is_ident("replaces") {
                return Err(meta.error("unknown #[governed_facts] key; expected replaces(...)"));
            }
            meta.parse_nested_meta(|entry| {
                let old = entry
                    .path
                    .get_ident()
                    .ok_or_else(|| entry.error("expected `old_fact = \"new_fact\"`"))?
                    .to_string();
                let new: LitStr = entry.value()?.parse()?;
                let new_name = new.value();
                if !facts.iter().any(|f| f.name == new_name) {
                    return Err(syn::Error::new(
                        new.span(),
                        format!("`{new_name}` is not a Fact of this type"),
                    ));
                }
                if facts.iter().any(|f| f.name == old) {
                    return Err(entry.error(format!(
                        "`{old}` is still a Fact of this type; a replaced name must be gone"
                    )));
                }
                out.push((old, new_name));
                Ok(())
            })
        })?;
    }
    Ok(out)
}
