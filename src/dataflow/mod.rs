mod channel;
mod engine;
mod fft;
mod kernels;
mod runtime;
mod silence;
mod spec;
pub(crate) mod store;
#[cfg(test)]
mod tests;
mod time;

pub use channel::{ChannelError, ChannelSelection};
pub use engine::RuntimeMetrics;
pub use fft::{FftConfig, FftMetadata, WindowFunction};
pub(crate) use runtime::Dataflow;
pub use runtime::{
    ControlError, DataController, DataSubscription, DataflowHandle, PublicationWaker,
};
pub use spec::{
    DataSpec, HISTORY_BUDGET_BYTES, Keep, NodeRef, NodeSpec, OutputId, OutputSpec, PeakConfig,
    SOURCE, SharedAnalysisId, SmoothConfig, SpecError,
};
