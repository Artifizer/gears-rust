//! Runtime support for `#[derive(GovernedFacts)]`.
//!
//! - [`FactValue`] — what a typed Fact method returns: the value, or why it
//!   could not be loaded.
//! - [`LazySource`] — loads a source (for example a database row) at most
//!   once per check, shared by every costly Fact built from it.
//! - [`FactResultExt::or_unavailable`] — turns any `Result` into a
//!   [`FactValue`].

use std::collections::BTreeSet;
use std::fmt::Display;
use std::future::Future;

use serde::Serialize;
use tokio::sync::OnceCell;

use crate::facts::{FactError, FactSchema, FactWriter};
use crate::gts::GovernedOperationTraits;

#[doc(hidden)]
pub use async_trait::async_trait;

/// Why a Fact could not be loaded. A policy that reads it makes the check
/// Unavailable, never Allowed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactUnavailable(pub String);

impl FactUnavailable {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// The result of one typed Fact method.
pub type FactValue<T> = Result<T, FactUnavailable>;

/// Turn any `Result` into a [`FactValue`].
pub trait FactResultExt<T> {
    /// The value, or the error's message as the unavailable reason.
    ///
    /// # Errors
    /// [`FactUnavailable`] carrying the original error's message.
    fn or_unavailable(self) -> FactValue<T>;
}

impl<T, E: Display> FactResultExt<T> for Result<T, E> {
    fn or_unavailable(self) -> FactValue<T> {
        self.map_err(|e| FactUnavailable(e.to_string()))
    }
}

/// Loads a source at most once and shares it between costly Facts.
///
/// `None` from the loader means "the source does not exist": every Fact that
/// needs it is unavailable, and [`LazySource::is_missing`] tells the caller,
/// so it can answer "not found" instead of a governance refusal.
pub struct LazySource<T> {
    cell: OnceCell<Result<Option<T>, String>>,
}

impl<T> Default for LazySource<T> {
    fn default() -> Self {
        Self {
            cell: OnceCell::new(),
        }
    }
}

impl<T> LazySource<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The source, loading it on the first call.
    ///
    /// # Errors
    /// [`FactUnavailable`] when the loader failed or found nothing.
    pub async fn get<F, Fut, E>(&self, load: F) -> FactValue<&T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, E>>,
        E: Display,
    {
        let loaded = self
            .cell
            .get_or_init(|| async { load().await.map_err(|e| e.to_string()) })
            .await;
        match loaded {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(FactUnavailable::new("source not found")),
            Err(e) => Err(FactUnavailable(e.clone())),
        }
    }

    /// Whether the source was looked up and does not exist.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        matches!(self.cell.get(), Some(Ok(None)))
    }

    /// Whether the source was looked up at all (a policy read one of its Facts).
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        self.cell.get().is_some()
    }
}

/// Write a typed Fact value. Used by the generated provider.
///
/// # Errors
/// [`FactError`] when the value does not match the Fact's schema.
#[doc(hidden)]
pub fn write_fact<T: Serialize>(
    out: &mut FactWriter<'_>,
    fact: &str,
    value: FactValue<T>,
) -> Result<(), FactError> {
    match value {
        Ok(value) => out.set(fact, value),
        Err(FactUnavailable(reason)) => {
            out.unavailable(fact, reason);
            Ok(())
        }
    }
}

/// Check that the type's GTS schema declares exactly the derive's Facts, with
/// exactly its traits. Called by the test the derive generates.
///
/// # Panics
/// When the schema's Facts differ from the struct's fields, or the `traits`
/// passed to `gts_type_schema` are not `og_traits()`.
#[doc(hidden)]
#[allow(clippy::panic)]
pub fn assert_schema_matches<T: gts::GtsSchema>(
    rust_type: &str,
    facts: &[&str],
    traits: &GovernedOperationTraits,
) {
    let schema = match FactSchema::of::<T>() {
        Ok(schema) => schema,
        Err(e) => panic!("{}: {e}", T::TYPE_ID),
    };
    let declared: BTreeSet<String> = schema.declared();
    let fields: BTreeSet<String> = facts.iter().map(|f| (*f).to_owned()).collect();
    assert_eq!(
        declared,
        fields,
        "{}: the GTS schema's facts differ from the struct's fields",
        T::TYPE_ID
    );
    for (fact, expected) in &traits.facts {
        assert_eq!(
            schema.traits(fact),
            Some(expected),
            "{}: traits of `{fact}` differ from #[fact(...)]; pass `traits = {rust_type}::og_traits()`",
            T::TYPE_ID
        );
    }
    for (old, new) in &traits.replaces {
        assert_eq!(
            schema.current_id(old),
            Some(new.as_str()),
            "{}: `replaces` entry `{old}` is not in the GTS traits",
            T::TYPE_ID
        );
    }
}
