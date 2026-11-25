//! The Fact contract a Consuming Gear fulfils on demand.
//!
//! A gear implements [`FactProvider`] once per Governed Operation type. The
//! Enforcer calls it with only the Facts the applicable policies read, one
//! call per [`FactTier`], so a cheap refusal never pays for an expensive Fact.
//! The [`FactWriter`] accepts only Facts the type declares, and checks every
//! value against the Fact's schema, so a gear bug fails in the gear, with the
//! Fact's name, instead of reaching a policy.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use crate::gts::{FactTier, FactTraits};
use crate::models::FactSet;

/// Why a Fact could not be written or provided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactError {
    /// The Governed Operation type does not declare this Fact.
    #[error("fact `{0}` is not declared by the governed operation")]
    Undeclared(String),
    /// The value does not match the Fact's schema.
    #[error("fact `{fact}` does not match its schema: {message}")]
    Invalid { fact: String, message: String },
    /// The provider failed as a whole, for example a database outage.
    #[error("fact provider failed: {0}")]
    Provider(String),
    /// The Governed Operation type's schema is not usable.
    #[error("governed operation schema is not usable: {0}")]
    Schema(String),
}

/// Facts of one Governed Operation for one check, filled on demand.
#[async_trait]
pub trait FactProvider: Send + Sync {
    /// The Governed Operation type (derived from
    /// [`GovernedOperationV1`](crate::GovernedOperationV1)) whose Facts this provides.
    type Facts: gts::GtsSchema;

    /// Write every Fact in `need`. A Fact that cannot be loaded is reported
    /// with [`FactWriter::unavailable`], never silently skipped.
    async fn provide(&self, need: &FactSet, out: &mut FactWriter<'_>) -> Result<(), FactError>;
}

/// The Facts a Governed Operation type declares, read from its GTS schema.
#[derive(Debug)]
pub struct FactSchema {
    type_id: &'static str,
    facts: BTreeMap<String, FactSpec>,
    replaces: BTreeMap<String, String>,
}

#[derive(Debug)]
struct FactSpec {
    traits: FactTraits,
    validator: jsonschema::Validator,
}

#[derive(serde::Deserialize, Default)]
struct TraitValues {
    #[serde(default)]
    facts: BTreeMap<String, FactTraits>,
    #[serde(default)]
    replaces: BTreeMap<String, String>,
}

impl FactSchema {
    /// Read the declared Facts and their traits from a derived Governed
    /// Operation type.
    ///
    /// # Errors
    /// [`FactError::Schema`] when the type has no `facts` properties, a Fact's
    /// schema does not compile, or a trait names an undeclared Fact.
    pub fn of<T: gts::GtsSchema>() -> Result<Self, FactError> {
        let schema = T::gts_schema_with_refs();
        let properties = find_facts_properties(&schema).ok_or_else(|| {
            FactError::Schema(format!("`{}` declares no `facts` properties", T::TYPE_ID))
        })?;
        let traits: TraitValues = find_key(&schema, gts::schema_traits::X_GTS_TRAITS)
            .map(|v| serde_json::from_value(v.clone()))
            .transpose()
            .map_err(|e| FactError::Schema(format!("`{}` traits: {e}", T::TYPE_ID)))?
            .unwrap_or_default();

        for name in traits.facts.keys() {
            if !properties.contains_key(name) {
                return Err(FactError::Schema(format!(
                    "`{}` has traits for undeclared fact `{name}`",
                    T::TYPE_ID
                )));
            }
        }

        let mut facts = BTreeMap::new();
        for (name, subschema) in properties {
            let validator = jsonschema::validator_for(subschema)
                .map_err(|e| FactError::Schema(format!("fact `{name}`: {e}")))?;
            let traits = traits.facts.get(name).cloned().unwrap_or_default();
            facts.insert(name.clone(), FactSpec { traits, validator });
        }

        Ok(Self {
            type_id: T::TYPE_ID,
            facts,
            replaces: traits.replaces,
        })
    }

    /// The Governed Operation type ID.
    #[must_use]
    pub const fn type_id(&self) -> &'static str {
        self.type_id
    }

    /// Every declared Fact ID.
    #[must_use]
    pub fn declared(&self) -> FactSet {
        self.facts.keys().cloned().collect()
    }

    /// Traits of a declared Fact.
    #[must_use]
    pub fn traits(&self, fact: &str) -> Option<&FactTraits> {
        self.facts.get(fact).map(|s| &s.traits)
    }

    /// The current ID of a Fact a policy reads: itself when declared, or its
    /// new ID when this version renamed it.
    #[must_use]
    pub fn current_id<'a>(&'a self, fact: &'a str) -> Option<&'a str> {
        if self.facts.contains_key(fact) {
            return Some(fact);
        }
        self.replaces
            .get(fact)
            .map(String::as_str)
            .filter(|new| self.facts.contains_key(*new))
    }

    /// Old IDs of a current Fact, so the check also carries it under them.
    pub fn old_ids<'a>(&'a self, fact: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.replaces
            .iter()
            .filter(move |(_, new)| new.as_str() == fact)
            .map(|(old, _)| old.as_str())
    }

    /// Split Facts by tier, cheapest first.
    #[must_use]
    pub fn by_tier(&self, facts: &FactSet) -> Vec<(FactTier, FactSet)> {
        let mut tiers: BTreeMap<FactTier, FactSet> = BTreeMap::new();
        for fact in facts {
            let tier = self.traits(fact).map_or(FactTier::Request, |t| t.tier);
            tiers.entry(tier).or_default().insert(fact.clone());
        }
        tiers.into_iter().collect()
    }
}

/// Collects Fact values for one check.
#[derive(Debug)]
pub struct FactWriter<'a> {
    schema: &'a FactSchema,
    values: serde_json::Map<String, Value>,
    unavailable: BTreeMap<String, String>,
}

impl<'a> FactWriter<'a> {
    #[must_use]
    pub fn new(schema: &'a FactSchema) -> Self {
        Self {
            schema,
            values: serde_json::Map::new(),
            unavailable: BTreeMap::new(),
        }
    }

    /// Write one Fact.
    ///
    /// # Errors
    /// [`FactError::Undeclared`] for a Fact the type does not declare, and
    /// [`FactError::Invalid`] for a value that does not match its schema.
    pub fn set(&mut self, fact: &str, value: impl Serialize) -> Result<(), FactError> {
        let spec = self
            .schema
            .facts
            .get(fact)
            .ok_or_else(|| FactError::Undeclared(fact.to_owned()))?;
        let value = serde_json::to_value(value).map_err(|e| FactError::Invalid {
            fact: fact.to_owned(),
            message: e.to_string(),
        })?;
        if let Err(e) = spec.validator.validate(&value) {
            return Err(FactError::Invalid {
                fact: fact.to_owned(),
                message: e.to_string(),
            });
        }
        self.unavailable.remove(fact);
        self.values.insert(fact.to_owned(), value);
        Ok(())
    }

    /// Report that a declared Fact could not be loaded. A policy that reads it
    /// then makes the check Unavailable, never Allowed.
    pub fn unavailable(&mut self, fact: &str, reason: impl Into<String>) {
        if self.schema.facts.contains_key(fact) && !self.values.contains_key(fact) {
            self.unavailable.insert(fact.to_owned(), reason.into());
        }
    }

    /// Whether the Fact has a value or was reported unavailable.
    #[must_use]
    pub fn has(&self, fact: &str) -> bool {
        self.values.contains_key(fact) || self.unavailable.contains_key(fact)
    }

    /// The values written so far, with each renamed Fact also under its old
    /// IDs, and the Facts reported unavailable.
    #[must_use]
    pub fn snapshot(&self) -> (serde_json::Map<String, Value>, BTreeMap<String, String>) {
        let mut values = self.values.clone();
        for (fact, value) in &self.values {
            for old in self.schema.old_ids(fact) {
                values.insert(old.to_owned(), value.clone());
            }
        }
        (values, self.unavailable.clone())
    }
}

/// The `properties` of the `facts` object refined by a derived type.
fn find_facts_properties(schema: &Value) -> Option<&serde_json::Map<String, Value>> {
    match schema {
        Value::Object(map) => {
            if let Some(Value::Object(facts)) = map
                .get("properties")
                .and_then(|p| p.get("facts"))
                .and_then(|f| f.get("properties"))
            {
                return Some(facts);
            }
            map.values().find_map(find_facts_properties)
        }
        Value::Array(items) => items.iter().find_map(find_facts_properties),
        _ => None,
    }
}

fn find_key<'v>(schema: &'v Value, key: &str) -> Option<&'v Value> {
    match schema {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|v| find_key(v, key))),
        Value::Array(items) => items.iter().find_map(|v| find_key(v, key)),
        _ => None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "facts_tests.rs"]
mod facts_tests;
