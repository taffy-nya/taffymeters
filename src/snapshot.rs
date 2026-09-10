use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use arc_swap::ArcSwapOption;

use crate::audio::{AudioDescriptor, SourceStatus};
use crate::dataflow::{ChannelError, ChannelSelection, FftMetadata};

#[derive(Clone, Debug)]
pub struct SpectrumSnapshot {
    pub descriptor: Arc<AudioDescriptor>,
    pub channel: ChannelSelection,
    pub config_revision: u64,
    pub result_version: u64,
    pub processed_through_frame: u64,
    pub source_sequence: u64,
    pub discontinuity_count: u64,
    pub fft: FftMetadata,
    pub bins_dbfs: Box<[f32]>,
}

#[derive(Clone)]
pub struct SpectrumReader {
    analysis_error: Arc<ArcSwapOption<ChannelError>>,
    slot: Arc<ArcSwapOption<SpectrumSnapshot>>,
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<crate::audio::SourceTelemetry>,
    requested_channel: Arc<AtomicU32>,
}

impl SpectrumReader {
    pub(crate) fn new(
        slot: Arc<ArcSwapOption<SpectrumSnapshot>>,
        descriptor: Arc<AudioDescriptor>,
        telemetry: Arc<crate::audio::SourceTelemetry>,
        analysis_error: Arc<ArcSwapOption<ChannelError>>,
        requested_channel: Arc<AtomicU32>,
    ) -> Self {
        Self {
            slot,
            analysis_error,
            descriptor,
            telemetry,
            requested_channel,
        }
    }

    pub fn latest(&self) -> Option<Arc<SpectrumSnapshot>> {
        self.slot.load_full()
    }

    pub fn descriptor(&self) -> &Arc<AudioDescriptor> {
        &self.descriptor
    }

    pub fn requested_channel(&self) -> ChannelSelection {
        ChannelSelection::decode(self.requested_channel.load(Ordering::Acquire))
    }

    pub fn analysis_error(&self) -> Option<Arc<ChannelError>> {
        self.analysis_error.load_full()
    }

    pub fn source_status(&self) -> SourceStatus {
        self.telemetry.snapshot()
    }
}
