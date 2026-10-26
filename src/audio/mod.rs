mod descriptor;
pub(crate) mod inputs;
mod source;
mod status;
mod transport;

pub use descriptor::{AudioDescriptor, ChannelLayout, DescriptorError, Generation, SourceId};
pub use source::SourceReader;
pub(crate) use status::SourceTelemetry;
pub use status::{SourceState, SourceStatus};
pub(crate) use transport::{
    AudioBlock, CaptureEndpoint, ProcessingEndpoint, create_block_transport,
};
