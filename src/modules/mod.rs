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

pub use empty::EMPTY_MODULE_ID;
pub use level_meter::{LEVEL_METER_MODULE_ID, LOUDNESS_METER_MODULE_ID};
pub use oscilloscope::{LINE_OSCILLOSCOPE_STYLE_ID, OSCILLOSCOPE_MODULE_ID};
pub use spectrogram::SPECTROGRAM_MODULE_ID;
pub use spectrum::{
    BARS_SPECTRUM_STYLE_ID, LINE_SPECTRUM_STYLE_ID, PIXEL_SPECTRUM_STYLE_ID, SPECTRUM_MODULE_ID,
};
pub use stereometer::STEREOMETER_MODULE_ID;
pub use waveform::WAVEFORM_MODULE_ID;

/// Tilt new Spectrum and Spectrogram instances apply around 1 kHz. Music falls
/// roughly 3–8 dB per octave per FFT bin; 4.5 dB/oct is the common analyzer
/// default (FabFilter Pro-Q) and looks balanced without hiding the bass.
pub(crate) const DEFAULT_TILT_DB_PER_OCTAVE: f32 = 4.5;

pub fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    spectrum::register(registry)?;
    spectrogram::register(registry)?;
    oscilloscope::register(registry)?;
    waveform::register(registry)?;
    stereometer::register(registry)?;
    level_meter::register(registry)?;
    empty::register(registry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{modules::test_support::PcmFixture, theme::ColorRole};

    #[test]
    fn type_color_roles_are_those_of_its_instances_across_styles() {
        let fixture = PcmFixture::new(2);
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        for (type_id, _) in registry.module_types() {
            let mut module = registry
                .create_module(type_id, &fixture.services, None)
                .unwrap();
            let mut offered: Vec<ColorRole> = module.color_roles().to_vec();
            for option in module.style_options() {
                module.set_style(option.id, &registry).unwrap();
                offered.extend_from_slice(module.color_roles());
            }
            offered.sort_unstable();
            offered.dedup();
            let mut declared = registry.color_roles(type_id).to_vec();
            declared.sort_unstable();
            assert_eq!(offered, declared, "{type_id}");
        }
    }
}
