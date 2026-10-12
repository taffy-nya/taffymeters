use crate::{
    dataflow::DataSpec,
    extensions::{Registry, RegistryError},
    module::{Module, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices, spec_from_saved},
};

pub const EMPTY_MODULE_ID: &str = "taffymeters.empty";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_module(EmptyFactory)
}

struct EmptyFactory;

impl ModuleFactory for EmptyFactory {
    fn type_id(&self) -> &'static str {
        EMPTY_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Empty"
    }

    fn create(
        &self,
        _services: &ModuleServices,
        _registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        // Empty has no analysis: saved analysis it cannot hold makes the leaf a
        // stand-in that keeps the original, instead of being dropped on save.
        spec_from_saved(DataSpec::new(), data)?;
        Ok(Box::new(EmptyModule))
    }
}

struct EmptyModule;

impl Module for EmptyModule {
    fn draw(&mut self, _frame: ModuleFrame<'_>) -> ModuleResponse {
        ModuleResponse::default()
    }
}
