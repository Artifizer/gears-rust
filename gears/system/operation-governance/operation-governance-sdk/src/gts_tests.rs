#![allow(clippy::unwrap_used, clippy::expect_used)]

use gts::GtsSchema;

use super::{GovernedOperationV1, OperationGovernancePluginSpecV1};

#[test]
fn type_ids_are_canonical() {
    assert_eq!(
        GovernedOperationV1::<()>::TYPE_ID,
        toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~")
    );
    assert_eq!(
        OperationGovernancePluginSpecV1::TYPE_ID,
        toolkit_gts::gts_id!("cf.toolkit.plugins.plugin.v1~cf.core.og.plugin.v1~")
    );
}

#[test]
fn inventory_registers_sdk_types() {
    let ids: Vec<&str> = toolkit_gts::inventory::iter::<toolkit_gts::InventoryTypeSchema>
        .into_iter()
        .map(|e| e.type_id)
        .collect();
    for id in [
        GovernedOperationV1::<()>::TYPE_ID,
        OperationGovernancePluginSpecV1::TYPE_ID,
    ] {
        assert!(ids.contains(&id), "{id} not registered: {ids:?}");
    }
    toolkit_gts::all_inventory_type_schemas().expect("every schema renders");
}

#[test]
fn base_declares_trait_defaults() {
    let schema = GovernedOperationV1::<()>::gts_schema_with_refs();
    let text = schema.to_string();
    assert!(text.contains("x-gts-traits-schema"), "{schema:#}");
    assert!(text.contains("\"replaces\""), "{schema:#}");
}
