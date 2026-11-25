#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::gts::{FactTier, GovernedOperationV1};

use super::{FactError, FactSchema, FactWriter};

/// A derived Governed Operation type, as a Consuming Gear declares one.
#[toolkit_gts::gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~cf.test.widget.create.v2~"),
    description = "test operation",
    properties = "name,size,owner_count",
    traits = serde_json::json!({
        "facts": { "owner_count": { "tier": "costly", "binding": false } },
        "replaces": { "title": "name" }
    })
)]
#[derive(Default)]
struct CreateWidgetV2 {
    name: String,
    size: u64,
    owner_count: u64,
}

fn schema() -> FactSchema {
    FactSchema::of::<CreateWidgetV2>().expect("schema")
}

#[test]
fn reads_declared_facts_and_traits() {
    let s = schema();
    assert_eq!(
        s.declared().into_iter().collect::<Vec<_>>(),
        ["name", "owner_count", "size"]
    );
    assert_eq!(s.traits("owner_count").unwrap().tier, FactTier::Costly);
    assert!(!s.traits("owner_count").unwrap().binding);
    assert_eq!(s.traits("size").unwrap().tier, FactTier::Request);
}

#[test]
fn renamed_fact_maps_to_its_current_id() {
    let s = schema();
    assert_eq!(s.current_id("title"), Some("name"));
    assert_eq!(s.current_id("name"), Some("name"));
    assert_eq!(s.current_id("nope"), None);
}

#[test]
fn writer_rejects_undeclared_and_malformed_facts() {
    let s = schema();
    let mut w = FactWriter::new(&s);
    assert_eq!(
        w.set("color", "red"),
        Err(FactError::Undeclared("color".to_owned()))
    );
    assert!(matches!(
        w.set("size", "big"),
        Err(FactError::Invalid { .. })
    ));
    w.set("size", 10_u64).unwrap();
    assert!(w.has("size"));
}

#[test]
fn snapshot_carries_renamed_fact_under_old_id() {
    let s = schema();
    let mut w = FactWriter::new(&s);
    w.set("name", "a").unwrap();
    w.unavailable("owner_count", "db down");
    let (values, unavailable) = w.snapshot();
    assert_eq!(values["name"], "a");
    assert_eq!(values["title"], "a");
    assert_eq!(unavailable["owner_count"], "db down");
}

#[test]
fn tiers_are_ordered_cheapest_first() {
    let s = schema();
    let tiers = s.by_tier(&s.declared());
    assert_eq!(tiers.first().unwrap().0, FactTier::Request);
    assert_eq!(tiers.last().unwrap().0, FactTier::Costly);
}
