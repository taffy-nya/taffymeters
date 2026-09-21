mod spectrogram;
mod spectrum;
mod time_domain;

use crate::module::{Registry, RegistryError};

pub use spectrogram::SPECTROGRAM_MODULE_ID;
pub use spectrum::{
    BAR_SPECTRUM_STYLE_ID, DEFAULT_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID, SPECTRUM_MODULE_ID,
};
pub use time_domain::{
    EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, LINE_OSCILLOSCOPE_STYLE_ID, OSCILLOSCOPE_MODULE_ID,
    PERSISTENCE_OSCILLOSCOPE_STYLE_ID, STEREOMETER_MODULE_ID, WAVEFORM_MODULE_ID,
};

pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    spectrum::register(registry)?;
    spectrogram::register(registry)?;
    time_domain::register(registry)
}
