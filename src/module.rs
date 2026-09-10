use std::{collections::HashMap, error::Error, fmt, sync::Arc, time::Duration};

use crate::{
    render::{Point, Rect, RenderTarget},
    snapshot::{SpectrumReader, SpectrumSnapshot},
};

pub struct ModuleServices {
    pub spectrum: SpectrumReader,
}

pub struct ModuleFrame<'a> {
    pub rect: Rect,
    pub monotonic_time: Duration,
    pub renderer: &'a mut dyn RenderTarget,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleResponse {
    pub repaint_after: Option<Duration>,
}

pub trait Module: Send {
    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse;
}

pub trait ModuleFactory: Send + Sync {
    fn type_id(&self) -> &'static str;

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError>;
}

pub struct SpectrumStyleContext<'a> {
    pub snapshot: &'a SpectrumSnapshot,
    pub coordinates: &'a SpectrumCoordinates,
    pub rect: Rect,
    pub monotonic_time: Duration,
}

pub trait SpectrumStyle: Send {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget);
}

pub trait SpectrumStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn create(&self) -> Box<dyn SpectrumStyle>;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpectrumCoordinates {
    pub min_frequency_hz: f32,
    pub max_frequency_hz: f32,
    pub min_dbfs: f32,
    pub max_dbfs: f32,
}

impl SpectrumCoordinates {
    pub fn point(&self, frequency_hz: f32, dbfs: f32, rect: Rect) -> Point {
        let frequency = frequency_hz.clamp(self.min_frequency_hz, self.max_frequency_hz);
        let x_fraction = (frequency / self.min_frequency_hz).ln()
            / (self.max_frequency_hz / self.min_frequency_hz).ln();
        let y_fraction = ((dbfs - self.min_dbfs) / (self.max_dbfs - self.min_dbfs)).clamp(0.0, 1.0);
        Point {
            x: rect.min.x + x_fraction * rect.width(),
            y: rect.max.y - y_fraction * rect.height(),
        }
    }

    pub fn frequency_at_x(&self, x: f32, rect: Rect) -> f32 {
        let fraction = ((x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        self.min_frequency_hz * (self.max_frequency_hz / self.min_frequency_hz).powf(fraction)
    }
}

#[derive(Default)]
pub struct Registry {
    modules: HashMap<String, Arc<dyn ModuleFactory>>,
    spectrum_styles: HashMap<String, Arc<dyn SpectrumStyleFactory>>,
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

    pub fn create_spectrum_style(&self, id: &str) -> Result<Box<dyn SpectrumStyle>, RegistryError> {
        Ok(self
            .spectrum_styles
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
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateModule(id) => write!(f, "module type is already registered: {id}"),
            Self::DuplicateStyle(id) => write!(f, "spectrum style is already registered: {id}"),
            Self::UnknownModule(id) => write!(f, "module type is not registered: {id}"),
            Self::UnknownStyle(id) => write!(f, "spectrum style is not registered: {id}"),
        }
    }
}

impl Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logarithmic_mapping_round_trips_frequency() {
        let coordinates = SpectrumCoordinates {
            min_frequency_hz: 20.0,
            max_frequency_hz: 20_000.0,
            min_dbfs: -100.0,
            max_dbfs: 0.0,
        };
        let rect = Rect {
            min: Point { x: 10.0, y: 20.0 },
            max: Point {
                x: 1010.0,
                y: 520.0,
            },
        };
        for frequency in [20.0, 100.0, 1_000.0, 10_000.0, 20_000.0] {
            let x = coordinates.point(frequency, -40.0, rect).x;
            let round_trip = coordinates.frequency_at_x(x, rect);
            assert!((round_trip / frequency - 1.0).abs() < 0.000_01);
        }
    }
}
