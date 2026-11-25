use operation_governance_sdk::{FactValue, GovernedFacts, GovernedOperationV1};
use toolkit_gts::gts_type_schema;

#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.ui.widget.create.v2~"),
    description = "ui test",
    properties = "name",
    traits = CreateWidgetV2::og_traits()
)]
#[derive(Default, GovernedFacts)]
#[governed_facts(replaces(title = "label"))]
pub struct CreateWidgetV2 {
    #[fact(request)]
    pub name: String,
}

fn main() {}
