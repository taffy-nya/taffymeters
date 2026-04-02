pub mod traits;
pub mod oscilloscope;
pub mod waveform;
pub mod spectrum;
pub mod spectrogram;
pub mod stereometer;
pub mod levelmeter;

use traits::View;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewType {
    Oscilloscope,
    Waveform,
    Spectrum,
    Spectrogram,
    Stereometer,
    LevelMeter,
}
 
impl ViewType {
    pub const ALL: &'static [ViewType] = &[
        ViewType::Oscilloscope,
        ViewType::Waveform,
        ViewType::Spectrum,
        ViewType::Spectrogram,
        ViewType::Stereometer,
        ViewType::LevelMeter,
    ];
 
    pub fn label(self) -> &'static str {
        match self {
            ViewType::Oscilloscope => "Oscilloscope",
            ViewType::Waveform => "Waveform",
            ViewType::Spectrum => "Spectrum",
            ViewType::Spectrogram => "Spectrogram",
            ViewType::Stereometer => "Stereometer",
            ViewType::LevelMeter => "Level Meter",
        }
    }
 
    pub fn create(self) -> Box<dyn View> {
        match self {
            ViewType::Oscilloscope => Box::new(oscilloscope::OscilloscopeView::new()),
            ViewType::Waveform => Box::new(waveform::WaveformView::new()),
            ViewType::Spectrum => Box::new(spectrum::SpectrumView::new()),
            ViewType::Spectrogram => Box::new(spectrogram::SpectrogramView::new()),
            ViewType::Stereometer => Box::new(stereometer::StereometerView::new()),
            ViewType::LevelMeter => Box::new(levelmeter::LevelMeterView::new()),
        }
    }
}
