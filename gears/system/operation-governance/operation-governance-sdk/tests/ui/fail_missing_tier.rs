use operation_governance_sdk::{FactValue, GovernedFacts, GovernedOperationV1};
use toolkit_gts::gts_type_schema;

#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.ui.widget.create.v1~"),
    description = "ui test",
    properties = "name",
    traits = CreateWidgetV1::og_traits()
)]
#[derive(Default, GovernedFacts)]
pub struct CreateWidgetV1 {
    pub name: String,
}

fn main() {}
