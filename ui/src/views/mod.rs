pub mod traits;
pub mod oscilloscope;
pub mod spectrum;
pub mod spectrogram;
pub mod stereometer;

use traits::View;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewType {
    Oscilloscope,
    Spectrum,
    Spectrogram,
    Stereometer,
}
 
impl ViewType {
    pub const ALL: &'static [ViewType] = &[
        ViewType::Oscilloscope,
        ViewType::Spectrum,
        ViewType::Spectrogram,
        ViewType::Stereometer,
    ];
 
    pub fn label(self) -> &'static str {
        match self {
            ViewType::Oscilloscope => "Oscilloscope",
            ViewType::Spectrum => "Spectrum",
            ViewType::Spectrogram => "Spectrogram",
            ViewType::Stereometer => "Stereometer",
        }
    }
 
    pub fn create(self) -> Box<dyn View> {
        match self {
            ViewType::Oscilloscope => Box::new(oscilloscope::OscilloscopeView::new()),
            ViewType::Spectrum => Box::new(spectrum::SpectrumView::new()),
            ViewType::Spectrogram => Box::new(spectrogram::SpectrogramView::new()),
            ViewType::Stereometer => Box::new(stereometer::StereometerView::new()),
        }
    }
}
