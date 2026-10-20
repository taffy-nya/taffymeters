use crate::{
    dataflow::FftMetadata,
    plotting::SpectrumCoordinates,
    render::{Rect, RenderTarget},
    theme::Palette,
};

/// One Spectrum result: the main curve and, when available, the peak curve of the
/// same analysis hop.
#[derive(Clone, Copy, Debug)]
pub struct SpectrumPlot<'a> {
    pub fft: FftMetadata,
    pub spectrum_dbfs: &'a [f32],
    pub peak_dbfs: Option<&'a [f32]>,
}

pub struct SpectrumStyleContext<'a> {
    pub snapshot: &'a SpectrumPlot<'a>,
    pub coordinates: &'a SpectrumCoordinates,
    pub rect: Rect,
    pub monotonic_time: std::time::Duration,
    pub palette: &'a Palette,
}

pub trait SpectrumStyle: Send {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget);
}

pub trait SpectrumStyleFactory: Send + Sync {
    fn style_id(&self) -> &'static str;
    fn display_name(&self) -> &'static str;
    fn create(&self) -> Box<dyn SpectrumStyle>;
}
