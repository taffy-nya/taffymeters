use std::sync::Arc;

use super::{AudioDescriptor, SourceStatus, SourceTelemetry};

#[derive(Clone)]
pub struct SourceReader {
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<SourceTelemetry>,
}

impl SourceReader {
    pub(crate) fn new(descriptor: Arc<AudioDescriptor>, telemetry: Arc<SourceTelemetry>) -> Self {
        Self {
            descriptor,
            telemetry,
        }
    }

    pub fn descriptor(&self) -> &Arc<AudioDescriptor> {
        &self.descriptor
    }

    pub fn source_status(&self) -> SourceStatus {
        self.telemetry.snapshot()
    }
}
