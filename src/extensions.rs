use std::{collections::HashMap, error::Error, fmt, sync::Arc};

use crate::{
    dataflow::DataSpec,
    module::{Module, ModuleError, ModuleFactory, ModuleServices},
    modules::{
        oscilloscope::style::{OscilloscopeStyle, OscilloscopeStyleFactory},
        spectrum::style::{SpectrumStyle, SpectrumStyleFactory},
    },
};

#[derive(Default)]
pub struct Registry {
    modules: HashMap<String, Arc<dyn ModuleFactory>>,
    spectrum_styles: HashMap<String, Arc<dyn SpectrumStyleFactory>>,
    oscilloscope_styles: HashMap<String, Arc<dyn OscilloscopeStyleFactory>>,
}

impl Registry {
    pub fn register_module(
        &mut self,
        factory: impl ModuleFactory + 'static,
    ) -> Result<(), RegistryError> {
        let id = factory.type_id();
        if self.modules.contains_key(id) {
            return Err(RegistryError::DuplicateModule(id.to_owned()));
        }
        self.modules.insert(id.to_owned(), Arc::new(factory));
        Ok(())
    }

    pub fn register_spectrum_style(
        &mut self,
        factory: impl SpectrumStyleFactory + 'static,
    ) -> Result<(), RegistryError> {
        let id = factory.style_id();
        if self.spectrum_styles.contains_key(id) {
            return Err(RegistryError::DuplicateStyle(id.to_owned()));
        }
        self.spectrum_styles
            .insert(id.to_owned(), Arc::new(factory));
        Ok(())
    }

    pub fn register_oscilloscope_style(
        &mut self,
        factory: impl OscilloscopeStyleFactory + 'static,
    ) -> Result<(), RegistryError> {
        let id = factory.style_id();
        if self.oscilloscope_styles.contains_key(id) {
            return Err(RegistryError::DuplicateStyle(id.to_owned()));
        }
        self.oscilloscope_styles
            .insert(id.to_owned(), Arc::new(factory));
        Ok(())
    }

    /// Create an instance of `id`, with its default analysis or the saved `data`.
    pub fn create_module(
        &self,
        id: &str,
        services: &ModuleServices,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        self.modules
            .get(id)
            .ok_or_else(|| RegistryError::UnknownModule(id.to_owned()))?
            .create(services, self, data)
    }

    /// The registered type `id` as `(type_id, display_name)`.
    pub fn module_type(&self, id: &str) -> Option<(&'static str, &'static str)> {
        self.modules
            .get(id)
            .map(|factory| (factory.type_id(), factory.display_name()))
    }

    pub fn module_types(&self) -> Vec<(&'static str, &'static str)> {
        let mut modules: Vec<_> = self
            .modules
            .values()
            .map(|factory| (factory.type_id(), factory.display_name()))
            .collect();
        modules.sort_unstable_by_key(|(_, label)| *label);
        modules
    }

    pub fn create_spectrum_style(&self, id: &str) -> Result<Box<dyn SpectrumStyle>, RegistryError> {
        Ok(self
            .spectrum_styles
            .get(id)
            .ok_or_else(|| RegistryError::UnknownStyle(id.to_owned()))?
            .create())
    }

    pub fn create_oscilloscope_style(
        &self,
        id: &str,
    ) -> Result<Box<dyn OscilloscopeStyle>, RegistryError> {
        Ok(self
            .oscilloscope_styles
            .get(id)
            .ok_or_else(|| RegistryError::UnknownStyle(id.to_owned()))?
            .create())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegistryError {
    DuplicateModule(String),
    DuplicateStyle(String),
    UnknownModule(String),
    UnknownStyle(String),
    StylesUnsupported,
    Module(ModuleError),
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModule(id) => write!(f, "module type is already registered: {id}"),
            Self::DuplicateStyle(id) => write!(f, "visual style is already registered: {id}"),
            Self::UnknownModule(id) => write!(f, "module type is not registered: {id}"),
            Self::UnknownStyle(id) => write!(f, "visual style is not registered: {id}"),
            Self::StylesUnsupported => f.write_str("module does not support replaceable styles"),
            Self::Module(error) => write!(f, "module creation failed: {error}"),
        }
    }
}

impl Error for RegistryError {}

impl From<ModuleError> for RegistryError {
    fn from(error: ModuleError) -> Self {
        Self::Module(error)
    }
}
