pub mod traits;
pub mod flow;
pub mod components;
pub mod oscilloscope;
pub mod waveform;
pub mod spectrum;
pub mod spectrogram;
pub mod stereometer;
pub mod levelmeter;

pub use traits::View;

macro_rules! register_views {
    ($($mod:ident::$struct:ident => $label:literal),* $(,)?) => {
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[allow(clippy::enum_variant_names)]
        pub enum ViewType {
            $($struct,)*
        }

        impl ViewType {
            pub const ALL: &'static [ViewType] = &[$(ViewType::$struct,)*];

            pub fn label(self) -> &'static str {
                match self {
                    $(ViewType::$struct => $label,)*
                }
            }

            pub fn create(self) -> Box<dyn View> {
                match self {
                    $(ViewType::$struct => Box::new($mod::$struct::new()),)*
                }
            }
        }
    };
}

register_views! {
    oscilloscope::OscilloscopeView => "Oscilloscope",
    waveform::WaveformView         => "Waveform",
    spectrum::SpectrumView         => "Spectrum",
    spectrogram::SpectrogramView   => "Spectrogram",
    stereometer::StereometerView   => "Stereometer",
    levelmeter::LevelMeterView     => "Level Meter",
}
