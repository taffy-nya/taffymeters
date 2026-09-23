use std::{
    collections::{HashMap, VecDeque},
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use arc_swap::ArcSwapOption;

use crate::{
    audio::AudioDescriptor,
    snapshot::{AudioChunk, LevelsSnapshot, TimeDomainReader, TimeDomainSnapshot},
};

use super::{runtime::TimeMetrics, time::LevelAnalyzer};

const TIME_HISTORY_SECONDS: usize = 10;
const TIME_SNAPSHOT_HZ: usize = 30;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TimeDomainRequest {
    Latest,
    History { seconds: f32 },
    Levels,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeDomainRequestError {
    InvalidHistoryDuration,
    HistoryCapacityOverflow,
}

impl fmt::Display for TimeDomainRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHistoryDuration => {
                f.write_str("time-domain history must be finite and between 0 and 10 seconds")
            }
            Self::HistoryCapacityOverflow => {
                f.write_str("time-domain history capacity overflows addressable memory")
            }
        }
    }
}

impl Error for TimeDomainRequestError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ValidatedTimeDomainRequest {
    Latest,
    History { max_frames: usize },
    Levels,
}

impl TimeDomainRequest {
    pub(super) fn validate(
        self,
        sample_rate_hz: u32,
        channel_count: usize,
    ) -> Result<ValidatedTimeDomainRequest, TimeDomainRequestError> {
        match self {
            Self::Latest => Ok(ValidatedTimeDomainRequest::Latest),
            Self::Levels => Ok(ValidatedTimeDomainRequest::Levels),
            Self::History { seconds }
                if seconds.is_finite()
                    && seconds > 0.0
                    && seconds <= TIME_HISTORY_SECONDS as f32 =>
            {
                let frames = (seconds as f64 * sample_rate_hz as f64).ceil();
                if !frames.is_finite() || frames > usize::MAX as f64 {
                    return Err(TimeDomainRequestError::HistoryCapacityOverflow);
                }
                let max_frames = (frames as usize).max(1);
                max_frames
                    .checked_mul(channel_count)
                    .ok_or(TimeDomainRequestError::HistoryCapacityOverflow)?;
                Ok(ValidatedTimeDomainRequest::History { max_frames })
            }
            Self::History { .. } => Err(TimeDomainRequestError::InvalidHistoryDuration),
        }
    }
}

pub struct TimeDomainSubscription {
    reader: TimeDomainReader,
    remove: Option<Box<dyn FnOnce() + Send>>,
}

impl TimeDomainSubscription {
    pub(super) fn new(reader: TimeDomainReader, remove: impl FnOnce() + Send + 'static) -> Self {
        Self {
            reader,
            remove: Some(Box::new(remove)),
        }
    }

    pub fn reader(&self) -> TimeDomainReader {
        self.reader.clone()
    }
}

impl Drop for TimeDomainSubscription {
    fn drop(&mut self) {
        if let Some(remove) = self.remove.take() {
            remove();
        }
    }
}

struct TimeSubscriptionState {
    request: ValidatedTimeDomainRequest,
    slot: Arc<ArcSwapOption<TimeDomainSnapshot>>,
    join_frontier: u64,
    result_version: u64,
    history: VecDeque<Arc<AudioChunk>>,
    history_frames: usize,
    level_analyzer: Option<LevelAnalyzer>,
    momentary_lufs: Option<Vec<f32>>,
    interval_peaks: Option<Vec<f32>>,
    block_peaks: Option<Vec<f32>>,
}

impl TimeSubscriptionState {
    fn new(
        request: ValidatedTimeDomainRequest,
        slot: Arc<ArcSwapOption<TimeDomainSnapshot>>,
        join_frontier: u64,
        sample_rate_hz: u32,
        channel_count: usize,
    ) -> Self {
        let is_levels = matches!(request, ValidatedTimeDomainRequest::Levels);
        Self {
            request,
            slot,
            join_frontier,
            result_version: 0,
            history: VecDeque::new(),
            history_frames: 0,
            level_analyzer: is_levels.then(|| LevelAnalyzer::new(sample_rate_hz, channel_count)),
            momentary_lufs: is_levels.then(|| vec![-60.0; channel_count]),
            interval_peaks: is_levels.then(|| vec![0.0; channel_count]),
            block_peaks: is_levels.then(|| vec![0.0; channel_count]),
        }
    }
}

pub(super) struct TimeDomainProcessor {
    descriptor: Arc<AudioDescriptor>,
    metrics: Arc<TimeMetrics>,
    subscriptions: HashMap<u64, TimeSubscriptionState>,
    publish_interval_frames: usize,
    interval_frames: usize,
    pending_samples: Vec<f32>,
    pending_start_frame: Option<u64>,
    pending_source_sequence: u64,
    pending_frames: usize,
    gap_pending: bool,
}

impl TimeDomainProcessor {
    pub(super) fn new(descriptor: Arc<AudioDescriptor>, metrics: Arc<TimeMetrics>) -> Self {
        let publish_interval_frames =
            (descriptor.sample_rate_hz as usize).div_ceil(TIME_SNAPSHOT_HZ);
        Self {
            descriptor,
            metrics,
            subscriptions: HashMap::new(),
            publish_interval_frames,
            interval_frames: 0,
            pending_samples: Vec::new(),
            pending_start_frame: None,
            pending_source_sequence: 0,
            pending_frames: 0,
            gap_pending: false,
        }
    }

    pub(super) fn add_subscription(
        &mut self,
        id: u64,
        request: ValidatedTimeDomainRequest,
        slot: Arc<ArcSwapOption<TimeDomainSnapshot>>,
        join_frontier: u64,
    ) {
        let channels = self.descriptor.channel_count as usize;
        let subscription = TimeSubscriptionState::new(
            request,
            slot,
            join_frontier,
            self.descriptor.sample_rate_hz,
            channels,
        );
        self.subscriptions.insert(id, subscription);
        if self.has_pcm_demand() && self.pending_samples.capacity() == 0 {
            let capacity = self
                .publish_interval_frames
                .checked_mul(channels)
                .unwrap_or(0);
            self.pending_samples = Vec::with_capacity(capacity);
        }
    }

    pub(super) fn remove_subscription(&mut self, id: u64) {
        self.subscriptions.remove(&id);
        if !self.has_pcm_demand() {
            self.pending_samples = Vec::new();
            self.pending_start_frame = None;
            self.pending_frames = 0;
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }

    pub(super) fn mark_gap(&mut self) {
        self.interval_frames = 0;
        self.pending_samples.clear();
        self.pending_start_frame = None;
        self.pending_frames = 0;
        self.gap_pending = true;
        for subscription in self.subscriptions.values_mut() {
            if let Some(analyzer) = &mut subscription.level_analyzer {
                analyzer.reset();
                subscription
                    .interval_peaks
                    .as_mut()
                    .expect("level request owns interval peaks")
                    .fill(0.0);
                subscription
                    .momentary_lufs
                    .as_mut()
                    .expect("level request owns loudness state")
                    .fill(-60.0);
            }
        }
    }

    pub(super) fn process_block(
        &mut self,
        start_frame: u64,
        source_sequence: u64,
        frame_count: usize,
        interleaved: &[f32],
        discontinuity_count: u64,
    ) {
        if self.subscriptions.is_empty() || frame_count == 0 {
            return;
        }
        let channels = self.descriptor.channel_count as usize;
        let end_frame = start_frame.saturating_add(frame_count as u64);
        let samples = &interleaved[..frame_count * channels];

        let mut level_frames = 0_u64;
        for subscription in self.subscriptions.values_mut() {
            let Some(analyzer) = &mut subscription.level_analyzer else {
                continue;
            };
            if end_frame <= subscription.join_frontier {
                continue;
            }
            let first_frame = start_frame.max(subscription.join_frontier);
            let first_offset = (first_frame - start_frame) as usize;
            let level_samples = &samples[first_offset * channels..];
            let processed_frames = frame_count - first_offset;
            let momentary_lufs = subscription
                .momentary_lufs
                .as_mut()
                .expect("level request owns loudness state");
            let block_peaks = subscription
                .block_peaks
                .as_mut()
                .expect("level request owns block peaks");
            analyzer.process(level_samples, momentary_lufs, block_peaks);
            for (interval_peak, block_peak) in subscription
                .interval_peaks
                .as_mut()
                .expect("level request owns interval peaks")
                .iter_mut()
                .zip(block_peaks.iter().copied())
            {
                *interval_peak = interval_peak.max(block_peak);
            }
            level_frames = level_frames.saturating_add(processed_frames as u64);
        }
        if level_frames != 0 {
            self.metrics
                .level_frames
                .fetch_add(level_frames, Ordering::Relaxed);
        }

        if self.has_pcm_demand() {
            let first_join = self
                .subscriptions
                .values()
                .filter(|subscription| is_pcm_request(subscription.request))
                .map(|subscription| subscription.join_frontier)
                .min()
                .unwrap_or(end_frame);
            if end_frame > first_join {
                let first_frame = start_frame.max(first_join);
                let first_offset = (first_frame - start_frame) as usize;
                let pcm_samples = &samples[first_offset * channels..];
                if self.pending_frames == 0 {
                    self.pending_start_frame = Some(first_frame);
                    self.pending_source_sequence = source_sequence;
                }
                self.pending_samples.extend_from_slice(pcm_samples);
                self.pending_frames = self
                    .pending_frames
                    .saturating_add(frame_count - first_offset);
                self.metrics
                    .pcm_frames
                    .fetch_add((frame_count - first_offset) as u64, Ordering::Relaxed);
            }
        }

        self.interval_frames = self.interval_frames.saturating_add(frame_count);
        if self.interval_frames >= self.publish_interval_frames {
            self.publish(end_frame, discontinuity_count);
            self.interval_frames = 0;
        }
    }

    fn publish(&mut self, processed_through_frame: u64, discontinuity_count: u64) {
        let shared_chunk = if self.pending_frames == 0 {
            None
        } else {
            let channels = self.descriptor.channel_count as usize;
            let frame_count = self.pending_frames;
            let start_frame = self
                .pending_start_frame
                .take()
                .expect("pending PCM samples have a start frame");
            let interleaved = Arc::<[f32]>::from(std::mem::take(&mut self.pending_samples));
            self.pending_samples = Vec::with_capacity(
                self.publish_interval_frames
                    .checked_mul(channels)
                    .unwrap_or(0),
            );
            self.pending_frames = 0;
            Some(Arc::new(AudioChunk {
                start_frame,
                frame_count,
                source_sequence: self.pending_source_sequence,
                discontinuity_before: std::mem::take(&mut self.gap_pending),
                interleaved,
            }))
        };

        let descriptor = Arc::clone(&self.descriptor);
        let mut publications = 0_u64;
        for subscription in self.subscriptions.values_mut() {
            let channels = descriptor.channel_count as usize;
            let chunks = match subscription.request {
                ValidatedTimeDomainRequest::Latest => {
                    latest_chunks(shared_chunk.as_ref(), subscription.join_frontier, channels)
                }
                ValidatedTimeDomainRequest::History { max_frames } => {
                    if let Some(chunk) = subscription_chunk(
                        shared_chunk.as_ref(),
                        subscription.join_frontier,
                        channels,
                    ) {
                        subscription.history_frames = subscription
                            .history_frames
                            .saturating_add(chunk.frame_count);
                        subscription.history.push_back(chunk);
                    }
                    while subscription.history_frames > max_frames {
                        let excess = subscription.history_frames - max_frames;
                        let Some(oldest) = subscription.history.pop_front() else {
                            subscription.history_frames = 0;
                            break;
                        };
                        if oldest.frame_count <= excess {
                            subscription.history_frames = subscription
                                .history_frames
                                .saturating_sub(oldest.frame_count);
                        } else {
                            let start_frame = oldest.start_frame.saturating_add(excess as u64);
                            let trimmed = subscription_chunk(Some(&oldest), start_frame, channels)
                                .expect("history trim lies within the oldest chunk");
                            subscription.history_frames =
                                subscription.history_frames.saturating_sub(excess);
                            subscription.history.push_front(trimmed);
                            break;
                        }
                    }
                    subscription.history.iter().cloned().collect()
                }
                ValidatedTimeDomainRequest::Levels => Vec::new(),
            };
            let levels = if let (Some(momentary_lufs), Some(sample_peaks)) = (
                subscription.momentary_lufs.as_ref(),
                subscription.interval_peaks.as_mut(),
            ) {
                let snapshot = LevelsSnapshot {
                    momentary_lufs: Arc::from(momentary_lufs.clone()),
                    sample_peaks: Arc::from(sample_peaks.clone()),
                };
                sample_peaks.fill(0.0);
                Some(snapshot)
            } else {
                None
            };
            subscription.result_version = subscription.result_version.wrapping_add(1);
            subscription.slot.store(Some(Arc::new(TimeDomainSnapshot {
                descriptor: Arc::clone(&descriptor),
                result_version: subscription.result_version,
                processed_through_frame,
                discontinuity_count,
                chunks: Arc::from(chunks),
                levels,
            })));
            publications = publications.saturating_add(1);
        }
        self.metrics
            .publications
            .fetch_add(publications, Ordering::Relaxed);
    }

    fn has_pcm_demand(&self) -> bool {
        self.subscriptions
            .values()
            .any(|subscription| is_pcm_request(subscription.request))
    }
}

fn is_pcm_request(request: ValidatedTimeDomainRequest) -> bool {
    matches!(
        request,
        ValidatedTimeDomainRequest::Latest | ValidatedTimeDomainRequest::History { .. }
    )
}

fn latest_chunks(
    chunk: Option<&Arc<AudioChunk>>,
    join_frontier: u64,
    channels: usize,
) -> Vec<Arc<AudioChunk>> {
    subscription_chunk(chunk, join_frontier, channels)
        .into_iter()
        .collect()
}

fn subscription_chunk(
    chunk: Option<&Arc<AudioChunk>>,
    join_frontier: u64,
    channels: usize,
) -> Option<Arc<AudioChunk>> {
    let chunk = chunk?;
    let chunk_end = chunk.start_frame.saturating_add(chunk.frame_count as u64);
    if chunk_end <= join_frontier {
        return None;
    }
    let trim_frames = join_frontier.saturating_sub(chunk.start_frame) as usize;
    if trim_frames == 0 {
        return Some(Arc::clone(chunk));
    }
    let start_sample = trim_frames.checked_mul(channels)?;
    let interleaved = Arc::<[f32]>::from(&chunk.interleaved[start_sample..]);
    Some(Arc::new(AudioChunk {
        start_frame: chunk.start_frame + trim_frames as u64,
        frame_count: chunk.frame_count - trim_frames,
        source_sequence: chunk.source_sequence,
        discontinuity_before: chunk.discontinuity_before,
        interleaved,
    }))
}

pub(super) fn make_reader(
    slot: Arc<ArcSwapOption<TimeDomainSnapshot>>,
    descriptor: Arc<AudioDescriptor>,
    telemetry: Arc<crate::audio::SourceTelemetry>,
) -> TimeDomainReader {
    TimeDomainReader::new(slot, descriptor, telemetry)
}

pub(super) fn new_slot() -> Arc<ArcSwapOption<TimeDomainSnapshot>> {
    Arc::new(ArcSwapOption::empty())
}

pub(super) fn next_id(next: &AtomicU64) -> u64 {
    next.fetch_add(1, Ordering::Relaxed)
}
