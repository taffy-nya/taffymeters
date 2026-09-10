mod descriptor;
mod status;
mod system;
mod transport;

pub use descriptor::{AudioDescriptor, ChannelLayout, DescriptorError, Generation, SourceId};
pub(crate) use status::SourceTelemetry;
pub use status::{SourceState, SourceStatus};
pub(crate) use system::{OpenSystemInput, SystemCapture};
pub(crate) use transport::{
    AudioBlock, CaptureEndpoint, ProcessingEndpoint, create_block_transport,
};
