pub mod oscilloscope_style;
mod spectrogram;
mod spectrum;
pub mod spectrum_style;
mod time_domain;

use crate::extensions::{Registry, RegistryError};

pub use oscilloscope_style::{LINE_OSCILLOSCOPE_STYLE_ID, PERSISTENCE_OSCILLOSCOPE_STYLE_ID};
pub use spectrogram::SPECTROGRAM_MODULE_ID;
pub use spectrum::SPECTRUM_MODULE_ID;
pub use spectrum_style::{
    BAR_SPECTRUM_STYLE_ID, DEFAULT_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID,
};
pub use time_domain::{
    EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, OSCILLOSCOPE_MODULE_ID, STEREOMETER_MODULE_ID,
    WAVEFORM_MODULE_ID,
};

pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    spectrum::register(registry)?;
    spectrogram::register(registry)?;
    time_domain::register(registry)
}
