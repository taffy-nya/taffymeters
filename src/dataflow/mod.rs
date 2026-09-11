mod channel;
mod fft;
mod processing;
mod runtime;

pub use channel::{ChannelError, ChannelSelection};
pub use fft::{FftConfig, FftMetadata, WindowFunction};
pub use processing::{
    HistoryConfig, PeakConfig, RequestError, SharedAnalysisId, SmoothConfig, SpectrumRequest,
};
pub(crate) use runtime::SpectrumDataflow;
pub use runtime::{ControlError, RuntimeMetrics, SpectrumController, SpectrumSubscription};
