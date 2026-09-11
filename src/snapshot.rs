use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

use arc_swap::ArcSwapOption;

use crate::audio::{AudioDescriptor, SourceStatus};
use crate::dataflow::{ChannelError, ChannelSelection, FftMetadata};

#[derive(Clone, Debug)]
pub struct SpectrumHistoryFrame {
    pub processed_through_frame: u64,
    pub discontinuity_before: bool,
    pub bins_dbfs: Arc<[f32]>,
}

#[derive(Clone, Debug)]
pub struct SpectrumSnapshotSet {
    pub descriptor: Arc<AudioDescriptor>,
    pub channel: ChannelSelection,
    pub config_revision: u64,
    pub result_version: u64,
    pub processed_through_frame: u64,
    pub source_sequence: u64,
    pub discontinuity_count: u64,
    pub fft: FftMetadata,
    pub spectrum_dbfs: Arc<[f32]>,
    pub peak_dbfs: Option<Arc<[f32]>>,
    pub history: Arc<[SpectrumHistoryFrame]>,
}

#[derive(Clone)]
pub struct SpectrumReader {
    analysis_error: Arc<ArcSwapOption<ChannelError>>,
    slot: Arc<ArcSwapOption<SpectrumSnapshotSet>>,
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<crate::audio::SourceTelemetry>,
    requested_channel: Arc<AtomicU32>,
    requested_revision: Arc<AtomicU64>,
}

impl SpectrumReader {
    pub(crate) fn new(
        slot: Arc<ArcSwapOption<SpectrumSnapshotSet>>,
        descriptor: Arc<AudioDescriptor>,
        telemetry: Arc<crate::audio::SourceTelemetry>,
        analysis_error: Arc<ArcSwapOption<ChannelError>>,
        requested_channel: Arc<AtomicU32>,
        requested_revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            slot,
            analysis_error,
            descriptor,
            telemetry,
            requested_channel,
            requested_revision,
        }
    }

    pub fn latest(&self) -> Option<Arc<SpectrumSnapshotSet>> {
        self.slot.load_full()
    }

    pub fn descriptor(&self) -> &Arc<AudioDescriptor> {
        &self.descriptor
    }

    pub fn requested_channel(&self) -> ChannelSelection {
        ChannelSelection::decode(self.requested_channel.load(Ordering::Acquire))
    }

    pub fn requested_revision(&self) -> u64 {
        self.requested_revision.load(Ordering::Acquire)
    }

    pub fn analysis_error(&self) -> Option<Arc<ChannelError>> {
        self.analysis_error.load_full()
    }

    pub fn source_status(&self) -> SourceStatus {
        self.telemetry.snapshot()
    }
}
