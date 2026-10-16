mod empty;
mod level_meter;
pub mod oscilloscope;
mod pcm_view;
mod spectrogram;
pub mod spectrum;
mod stereometer;
#[cfg(test)]
pub(crate) mod test_support;
mod waveform;

use crate::extensions::{Registry, RegistryError};

/// Tilt new Spectrum and Spectrogram instances apply around 1 kHz. Music falls
/// roughly 3–8 dB per octave per FFT bin; 4.5 dB/oct is the common analyzer
/// default (FabFilter Pro-Q) and looks balanced without hiding the bass.
pub(crate) const DEFAULT_TILT_DB_PER_OCTAVE: f32 = 4.5;

pub use empty::EMPTY_MODULE_ID;
pub use level_meter::LEVEL_METER_MODULE_ID;
pub use oscilloscope::{
    LINE_OSCILLOSCOPE_STYLE_ID, OSCILLOSCOPE_MODULE_ID, PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
};
pub use spectrogram::SPECTROGRAM_MODULE_ID;
pub use spectrum::{
    BAR_SPECTRUM_STYLE_ID, DEFAULT_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID, SPECTRUM_MODULE_ID,
};
pub use stereometer::STEREOMETER_MODULE_ID;
pub use waveform::WAVEFORM_MODULE_ID;

pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    spectrum::register(registry)?;
    spectrogram::register(registry)?;
    oscilloscope::register(registry)?;
    waveform::register(registry)?;
    stereometer::register(registry)?;
    level_meter::register(registry)?;
    empty::register(registry)
}
