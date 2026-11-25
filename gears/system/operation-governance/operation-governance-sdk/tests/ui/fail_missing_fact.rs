use operation_governance_sdk::{FactValue, GovernedFacts, GovernedOperationV1};
use toolkit_gts::gts_type_schema;
#[gts_type_schema(
    dir_path = "schemas",
    base = GovernedOperationV1,
    type_id = toolkit_gts::gts_id!("cf.core.og.governed_operation.v1~x.ui.widget.create.v1~"),
    description = "ui test",
    properties = "name,owner_count",
    traits = CreateWidgetV1::og_traits()
)]
#[derive(Default, GovernedFacts)]
pub struct CreateWidgetV1 {
    #[fact(request)]
    pub name: String,
    #[fact(costly, binding = false)]
    pub owner_count: u64,
}

pub struct Input {
    pub name: String,
}

pub struct Provider {
    input: Input,
}

#[operation_governance_sdk::typed::async_trait]
impl CreateWidgetV1Facts for Provider {
    type Input = Input;
    fn input(&self) -> &Input {
        &self.input
    }
    fn name(input: &Input) -> FactValue<String> {
        Ok(input.name.clone())
    }
    // owner_count is not provided.
}

fn main() {}
