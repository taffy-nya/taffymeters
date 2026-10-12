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

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{
        module::{ModuleFrame, ModuleResponse},
        modules::test_support::PcmFixture,
        modules::{
            oscilloscope::style::OscilloscopeStyleContext, spectrum::style::SpectrumStyleContext,
        },
        render::RenderTarget,
    };

    /// Identifies itself through `active_style`, so a test can tell which factory built it.
    struct TaggedModule(&'static str);

    impl Module for TaggedModule {
        fn draw(&mut self, _frame: ModuleFrame<'_>) -> ModuleResponse {
            ModuleResponse::default()
        }

        fn active_style(&self) -> Option<&'static str> {
            Some(self.0)
        }
    }

    /// A Module that keeps every default of the trait.
    struct PlainModule;

    impl Module for PlainModule {
        fn draw(&mut self, _frame: ModuleFrame<'_>) -> ModuleResponse {
            ModuleResponse::default()
        }
    }

    struct TaggedModuleFactory {
        id: &'static str,
        name: &'static str,
        tag: &'static str,
    }

    impl ModuleFactory for TaggedModuleFactory {
        fn type_id(&self) -> &'static str {
            self.id
        }

        fn display_name(&self) -> &'static str {
            self.name
        }

        fn create(
            &self,
            _services: &ModuleServices,
            _registry: &Registry,
            _data: Option<&DataSpec>,
        ) -> Result<Box<dyn Module>, RegistryError> {
            Ok(Box::new(TaggedModule(self.tag)))
        }
    }

    fn module_factory(
        id: &'static str,
        name: &'static str,
        tag: &'static str,
    ) -> TaggedModuleFactory {
        TaggedModuleFactory { id, name, tag }
    }

    /// Styles built by a `TaggedStyleFactory` append their tag to the shared log when
    /// cleared, which shows which factory produced them.
    struct TaggedStyle {
        tag: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl SpectrumStyle for TaggedStyle {
        fn draw(&mut self, _context: SpectrumStyleContext<'_>, _renderer: &mut dyn RenderTarget) {}

        fn clear(&mut self) {
            self.log.lock().unwrap().push(self.tag);
        }
    }

    impl OscilloscopeStyle for TaggedStyle {
        fn draw(
            &mut self,
            _context: OscilloscopeStyleContext<'_>,
            _renderer: &mut dyn RenderTarget,
        ) -> ModuleResponse {
            ModuleResponse::default()
        }

        fn clear(&mut self) {
            self.log.lock().unwrap().push(self.tag);
        }
    }

    struct TaggedStyleFactory {
        id: &'static str,
        tag: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }

    impl TaggedStyleFactory {
        fn new(id: &'static str, tag: &'static str, log: &Arc<Mutex<Vec<&'static str>>>) -> Self {
            Self {
                id,
                tag,
                log: Arc::clone(log),
            }
        }

        fn style(&self) -> TaggedStyle {
            TaggedStyle {
                tag: self.tag,
                log: Arc::clone(&self.log),
            }
        }
    }

    impl SpectrumStyleFactory for TaggedStyleFactory {
        fn style_id(&self) -> &'static str {
            self.id
        }

        fn display_name(&self) -> &'static str {
            self.tag
        }

        fn create(&self) -> Box<dyn SpectrumStyle> {
            Box::new(self.style())
        }
    }

    impl OscilloscopeStyleFactory for TaggedStyleFactory {
        fn style_id(&self) -> &'static str {
            self.id
        }

        fn display_name(&self) -> &'static str {
            self.tag
        }

        fn create(&self) -> Box<dyn OscilloscopeStyle> {
            Box::new(self.style())
        }
    }

    #[test]
    fn duplicate_module_registration_is_rejected_and_keeps_the_first_factory() {
        let fixture = PcmFixture::new(2);
        let mut registry = Registry::default();
        registry
            .register_module(module_factory("test.module", "First", "first"))
            .unwrap();

        let result = registry.register_module(module_factory("test.module", "Second", "second"));

        assert_eq!(
            result,
            Err(RegistryError::DuplicateModule("test.module".to_owned()))
        );
        assert_eq!(
            registry.module_type("test.module"),
            Some(("test.module", "First"))
        );
        assert_eq!(registry.module_types().len(), 1);
        let module = registry
            .create_module("test.module", &fixture.services, None)
            .unwrap_or_else(|error| panic!("the first factory still creates the module: {error}"));
        assert_eq!(module.active_style(), Some("first"));
    }

    #[test]
    fn unknown_module_type_is_an_error_not_a_fallback() {
        let fixture = PcmFixture::new(2);
        let mut registry = Registry::default();
        registry
            .register_module(module_factory("test.module", "First", "first"))
            .unwrap();

        let error = registry
            .create_module("test.missing", &fixture.services, None)
            .err();

        assert_eq!(
            error,
            Some(RegistryError::UnknownModule("test.missing".to_owned()))
        );
        assert_eq!(registry.module_type("test.missing"), None);
    }

    #[test]
    fn module_types_are_listed_by_display_name_regardless_of_registration_order() {
        let mut registry = Registry::default();
        // Id order (a..e) is the reverse of label order, so sorting by id fails too.
        for (id, label) in [
            ("test.a", "Echo"),
            ("test.b", "Delta"),
            ("test.c", "Charlie"),
            ("test.d", "Bravo"),
            ("test.e", "Alpha"),
        ] {
            registry
                .register_module(module_factory(id, label, "tag"))
                .unwrap();
        }

        assert_eq!(
            registry.module_types(),
            vec![
                ("test.e", "Alpha"),
                ("test.d", "Bravo"),
                ("test.c", "Charlie"),
                ("test.b", "Delta"),
                ("test.a", "Echo"),
            ]
        );
    }

    #[test]
    fn duplicate_spectrum_style_registration_is_rejected_and_keeps_the_first_factory() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = Registry::default();
        registry
            .register_spectrum_style(TaggedStyleFactory::new("test.style", "first", &log))
            .unwrap();

        let result =
            registry.register_spectrum_style(TaggedStyleFactory::new("test.style", "second", &log));

        assert_eq!(
            result,
            Err(RegistryError::DuplicateStyle("test.style".to_owned()))
        );
        let mut style = registry
            .create_spectrum_style("test.style")
            .unwrap_or_else(|error| panic!("the first factory still creates the style: {error}"));
        style.clear();
        assert_eq!(*log.lock().unwrap(), ["first"]);
    }

    #[test]
    fn duplicate_oscilloscope_style_registration_is_rejected_and_keeps_the_first_factory() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = Registry::default();
        registry
            .register_oscilloscope_style(TaggedStyleFactory::new("test.style", "first", &log))
            .unwrap();

        let result = registry.register_oscilloscope_style(TaggedStyleFactory::new(
            "test.style",
            "second",
            &log,
        ));

        assert_eq!(
            result,
            Err(RegistryError::DuplicateStyle("test.style".to_owned()))
        );
        let mut style = registry
            .create_oscilloscope_style("test.style")
            .unwrap_or_else(|error| panic!("the first factory still creates the style: {error}"));
        style.clear();
        assert_eq!(*log.lock().unwrap(), ["first"]);
    }

    #[test]
    fn unknown_style_ids_are_errors_for_both_style_families() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut registry = Registry::default();
        registry
            .register_spectrum_style(TaggedStyleFactory::new("test.spectrum", "s", &log))
            .unwrap();
        registry
            .register_oscilloscope_style(TaggedStyleFactory::new("test.scope", "o", &log))
            .unwrap();

        assert_eq!(
            registry.create_spectrum_style("test.missing").err(),
            Some(RegistryError::UnknownStyle("test.missing".to_owned()))
        );
        assert_eq!(
            registry.create_oscilloscope_style("test.missing").err(),
            Some(RegistryError::UnknownStyle("test.missing".to_owned()))
        );
    }

    #[test]
    fn module_without_style_support_reports_styles_unsupported() {
        let mut module = PlainModule;

        let result = module.set_style("anything", &Registry::default());

        assert_eq!(result, Err(RegistryError::StylesUnsupported));
        assert!(module.style_options().is_empty());
        assert_eq!(module.active_style(), None);
    }
}
