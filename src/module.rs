use std::{collections::HashMap, error::Error, fmt, sync::Arc, time::Duration};

use crate::{
    dataflow::ChannelSelection,
    plotting::SpectrumCoordinates,
    render::{Point, Rect, RenderTarget},
    snapshot::{SpectrumReader, SpectrumSnapshotSet, TimeDomainReader, TimeDomainSnapshot},
};

pub struct ModuleServices {
    pub spectrum: SpectrumReader,
    pub time_domain: TimeDomainReader,
}

pub struct ModuleFrame<'a> {
    pub rect: Rect,
    pub monotonic_time: Duration,
    pub pointer: Option<Point>,
    pub scroll_delta_y: f32,
    pub zoom_delta: f32,
    pub renderer: &'a mut dyn RenderTarget,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleResponse {
    pub repaint_after: Option<Duration>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleOption {
    pub id: &'static str,
    pub label: &'static str,
}

pub trait Module: Send {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse;

    fn set_channel(&mut self, _channel: ChannelSelection) {}

    fn style_options(&self) -> &'static [StyleOption] {
        &[]
    }

    fn active_style(&self) -> Option<&'static str> {
        None
    }

    fn set_style(&mut self, _style_id: &str, _registry: &Registry) -> Result<(), RegistryError> {
        Err(RegistryError::StylesUnsupported)
    }

    fn clear_visual_state(&mut self) {}
}

pub trait ModuleFactory: Send + Sync {
    fn type_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError>;
}

pub struct SpectrumStyleContext<'a> {
    pub snapshot: &'a SpectrumSnapshotSet,
    pub coordinates: &'a SpectrumCoordinates,
    pub rect: Rect,
    pub monotonic_time: Duration,
}

pub trait SpectrumStyle: Send {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget);
    fn clear(&mut self) {}
}

pub trait SpectrumStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn create(&self) -> Box<dyn SpectrumStyle>;
}

pub struct OscilloscopeStyleContext<'a> {
    pub snapshot: &'a TimeDomainSnapshot,
    pub channel: ChannelSelection,
    pub rect: Rect,
    pub y_scale: f32,
    pub monotonic_time: Duration,
}

pub trait OscilloscopeStyle: Send {
    fn draw(
        &mut self,
        context: OscilloscopeStyleContext<'_>,
        renderer: &mut dyn RenderTarget,
    ) -> ModuleResponse;
    fn clear(&mut self) {}
}

pub trait OscilloscopeStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn create(&self) -> Box<dyn OscilloscopeStyle>;
}

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

    pub fn create_module(
        &self,
        id: &str,
        services: &ModuleServices,
    ) -> Result<Box<dyn Module>, RegistryError> {
        self.modules
            .get(id)
            .ok_or_else(|| RegistryError::UnknownModule(id.to_owned()))?
            .create(services, self)
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
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModule(id) => write!(f, "module type is already registered: {id}"),
            Self::DuplicateStyle(id) => write!(f, "visual style is already registered: {id}"),
            Self::UnknownModule(id) => write!(f, "module type is not registered: {id}"),
            Self::UnknownStyle(id) => write!(f, "visual style is not registered: {id}"),
            Self::StylesUnsupported => f.write_str("module does not support replaceable styles"),
        }
    }
}

impl Error for RegistryError {}
