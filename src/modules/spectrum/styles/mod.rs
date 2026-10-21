mod bars;
mod line;
mod pixel;

use crate::extensions::{Registry, RegistryError};

pub const LINE_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.line";
pub const BARS_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.bars";
pub const PIXEL_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.pixel";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_spectrum_style(line::LineSpectrumStyleFactory)?;
    registry.register_spectrum_style(bars::BarSpectrumStyleFactory)?;
    registry.register_spectrum_style(pixel::PixelSpectrumStyleFactory)
}
