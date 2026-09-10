use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle, Thread},
    time::Duration,
};

use arc_swap::ArcSwapOption;

use crate::{
    audio::{AudioBlock, AudioDescriptor, ProcessingEndpoint, SourceTelemetry},
    snapshot::{SpectrumReader, SpectrumSnapshot},
};

use super::{ChannelError, ChannelSelection, FftConfig, fft::StreamingFft};

#[derive(Clone)]
pub struct SpectrumController {
    commands: SyncSender<Command>,
    worker: Thread,
    requested_channel: Arc<AtomicU32>,
}

pub(crate) struct SpectrumDataflow {
    controller: SpectrumController,
    reader: SpectrumReader,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy, Debug)]
enum Command {
    SetChannel(ChannelSelection),
    ResumeBoundary(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlError {
    Busy,
    Stopped,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => f.write_str("dataflow control queue is full"),
            Self::Stopped => f.write_str("dataflow worker has stopped"),
        }
    }
}

impl Error for ControlError {}

impl SpectrumController {
    pub fn set_channel(&self, channel: ChannelSelection) -> Result<(), ControlError> {
        self.send(Command::SetChannel(channel))?;
        self.requested_channel
            .store(channel.encode(), Ordering::Release);
        Ok(())
    }

    pub(crate) fn resume_at(&self, continuity: u64) -> Result<(), ControlError> {
        self.send(Command::ResumeBoundary(continuity))
    }

    fn send(&self, command: Command) -> Result<(), ControlError> {
        match self.commands.try_send(command) {
            Ok(()) => {
                self.worker.unpark();
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(ControlError::Busy),
            Err(TrySendError::Disconnected(_)) => Err(ControlError::Stopped),
        }
    }
}

struct Processor {
    descriptor: Arc<AudioDescriptor>,
    channel: ChannelSelection,
    resolved_channel: Result<super::channel::ResolvedChannel, ChannelError>,
    fft: StreamingFft,
    slot: Arc<ArcSwapOption<SpectrumSnapshot>>,
    analysis_error: Arc<ArcSwapOption<ChannelError>>,
    config_revision: u64,
    result_version: u64,
    discontinuity_count: u64,
    expected_sequence: Option<u64>,
    expected_start_frame: Option<u64>,
    minimum_continuity: u64,
    active_continuity: u64,
}

impl SpectrumDataflow {
    pub(crate) fn spawn(
        descriptor: Arc<AudioDescriptor>,
        mut endpoint: ProcessingEndpoint,
        telemetry: Arc<SourceTelemetry>,
        fft_config: FftConfig,
        initial_channel: ChannelSelection,
    ) -> Self {
        let slot = Arc::new(ArcSwapOption::<SpectrumSnapshot>::empty());
        let analysis_error = Arc::new(ArcSwapOption::<ChannelError>::empty());
        let requested_channel = Arc::new(AtomicU32::new(initial_channel.encode()));
        let reader = SpectrumReader::new(
            Arc::clone(&slot),
            Arc::clone(&descriptor),
            telemetry,
            Arc::clone(&analysis_error),
            Arc::clone(&requested_channel),
        );
        let (command_sender, command_receiver) = sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("taffymeters-dataflow".to_owned())
            .spawn(move || {
                let mut processor = Processor::new(
                    descriptor,
                    fft_config,
                    initial_channel,
                    slot,
                    analysis_error,
                );
                run_worker(&mut processor, &mut endpoint, command_receiver, worker_stop);
            })
            .expect("failed to create dataflow worker");
        let controller = SpectrumController {
            commands: command_sender,
            worker: worker.thread().clone(),
            requested_channel,
        };
        Self {
            controller,
            reader,
            stop,
            worker: Some(worker),
        }
    }

    pub(crate) fn reader(&self) -> SpectrumReader {
        self.reader.clone()
    }

    pub(crate) fn controller(&self) -> SpectrumController {
        self.controller.clone()
    }

    pub(crate) fn worker_thread(&self) -> Thread {
        self.worker
            .as_ref()
            .expect("worker exists while dataflow is live")
            .thread()
            .clone()
    }
}

impl Drop for SpectrumDataflow {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.controller.worker.unpark();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Processor {
    fn new(
        descriptor: Arc<AudioDescriptor>,
        fft_config: FftConfig,
        channel: ChannelSelection,
        slot: Arc<ArcSwapOption<SpectrumSnapshot>>,
        analysis_error: Arc<ArcSwapOption<ChannelError>>,
    ) -> Self {
        let resolved_channel = channel.resolve(&descriptor);
        if let Err(error) = &resolved_channel {
            analysis_error.store(Some(Arc::new(error.clone())));
        }
        Self {
            descriptor,
            channel,
            resolved_channel,
            fft: StreamingFft::new(fft_config),
            slot,
            analysis_error,
            config_revision: 1,
            result_version: 0,
            discontinuity_count: 0,
            expected_sequence: None,
            expected_start_frame: None,
            minimum_continuity: 0,
            active_continuity: 0,
        }
    }

    fn set_channel(&mut self, channel: ChannelSelection) {
        if self.channel == channel {
            return;
        }
        self.channel = channel;
        self.config_revision = self.config_revision.wrapping_add(1);
        self.resolved_channel = channel.resolve(&self.descriptor);
        match &self.resolved_channel {
            Ok(_) => self.analysis_error.store(None),
            Err(error) => self.analysis_error.store(Some(Arc::new(error.clone()))),
        }
        self.slot.store(None);
        self.reset_continuity();
    }

    fn reset_continuity(&mut self) {
        self.fft.reset();
        self.expected_sequence = None;
        self.expected_start_frame = None;
        self.discontinuity_count = self.discontinuity_count.wrapping_add(1);
    }

    fn resume_at(&mut self, continuity: u64) {
        self.minimum_continuity = continuity;
        self.active_continuity = continuity;
        self.slot.store(None);
        self.reset_continuity();
    }

    fn process_block(&mut self, block: &AudioBlock) {
        if block.continuity < self.minimum_continuity {
            return;
        }
        if block.continuity != self.active_continuity {
            self.minimum_continuity = block.continuity;
            self.active_continuity = block.continuity;
            self.slot.store(None);
            self.reset_continuity();
        }

        let sequence_continuous = self
            .expected_sequence
            .is_none_or(|expected| expected == block.sequence);
        let frame_continuous = self
            .expected_start_frame
            .is_none_or(|expected| expected == block.start_frame);
        if !sequence_continuous || !frame_continuous {
            self.reset_continuity();
        }
        self.expected_sequence = Some(block.sequence.wrapping_add(1));
        self.expected_start_frame =
            Some(block.start_frame.saturating_add(block.frame_count as u64));

        let Ok(channel) = self.resolved_channel else {
            return;
        };
        let channels = self.descriptor.channel_count as usize;
        for (frame_index, frame) in block.samples(channels).chunks_exact(channels).enumerate() {
            if self.fft.push(channel.sample(frame)) {
                self.result_version = self.result_version.wrapping_add(1);
                self.slot.store(Some(Arc::new(SpectrumSnapshot {
                    descriptor: Arc::clone(&self.descriptor),
                    channel: self.channel,
                    config_revision: self.config_revision,
                    result_version: self.result_version,
                    processed_through_frame: block.start_frame + frame_index as u64 + 1,
                    source_sequence: block.sequence,
                    discontinuity_count: self.discontinuity_count,
                    fft: self.fft.metadata(self.descriptor.sample_rate_hz),
                    bins_dbfs: self.fft.bins_dbfs().into(),
                })));
            }
        }
    }
}

fn run_worker(
    processor: &mut Processor,
    endpoint: &mut ProcessingEndpoint,
    commands: Receiver<Command>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Acquire) {
        loop {
            match commands.try_recv() {
                Ok(Command::SetChannel(channel)) => processor.set_channel(channel),
                Ok(Command::ResumeBoundary(continuity)) => processor.resume_at(continuity),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }

        if let Some(block) = endpoint.try_pop() {
            processor.process_block(&block);
            endpoint.recycle(block);
        } else {
            thread::park_timeout(Duration::from_millis(5));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{f32::consts::PI, time::Instant};

    use super::*;
    use crate::audio::{
        ChannelLayout, Generation, SourceId, SourceTelemetry, create_block_transport,
    };

    fn wait_for(
        reader: &SpectrumReader,
        revision: u64,
        processed_through_frame: u64,
        deadline: Instant,
    ) -> Arc<SpectrumSnapshot> {
        loop {
            if let Some(snapshot) = reader.latest()
                && snapshot.config_revision == revision
                && snapshot.processed_through_frame == processed_through_frame
            {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for revision {revision} through frame {processed_through_frame}"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn peak_frequency(snapshot: &SpectrumSnapshot) -> f32 {
        let (index, _) = snapshot
            .bins_dbfs
            .iter()
            .enumerate()
            .skip(1)
            .max_by(|left, right| left.1.total_cmp(right.1))
            .unwrap();
        index as f32 * snapshot.fft.bin_hz
    }

    #[test]
    fn stereo_channel_switch_selects_the_expected_signal_and_reads_do_not_advance() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, processing) =
            create_block_transport(2, 4, 4096, Arc::clone(&telemetry)).unwrap();
        let dataflow = SpectrumDataflow::spawn(
            Arc::clone(&descriptor),
            processing,
            telemetry,
            FftConfig::default(),
            ChannelSelection::Left,
        );
        capture.set_worker(dataflow.worker_thread());
        let reader = dataflow.reader();
        let controller = dataflow.controller();
        let mut samples = Vec::with_capacity(4096 * 2);
        for frame in 0..4096 {
            samples.push(0.25 * (2.0 * PI * 440.0 * frame as f32 / 48_000.0).sin());
            samples.push(0.25 * (2.0 * PI * 880.0 * frame as f32 / 48_000.0).sin());
        }

        capture.write(&samples);
        let left = wait_for(&reader, 1, 4096, Instant::now() + Duration::from_secs(1));
        assert!((peak_frequency(&left) - 440.0).abs() <= left.fft.bin_hz);
        let stable_version = left.result_version;
        for _ in 0..100 {
            assert_eq!(reader.latest().unwrap().result_version, stable_version);
        }

        controller.set_channel(ChannelSelection::Right).unwrap();
        capture.write(&samples);
        let right = wait_for(&reader, 2, 8192, Instant::now() + Duration::from_secs(1));
        assert!((peak_frequency(&right) - 880.0).abs() <= right.fft.bin_hz);
    }

    #[test]
    fn resume_boundary_discards_queued_audio_from_before_pause() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 1, ChannelLayout::Mono)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, mut processing) =
            create_block_transport(1, 4, 1024, Arc::clone(&telemetry)).unwrap();
        capture.write(&[1.0_f32; 1024]);
        let before_pause = processing.try_pop().unwrap();

        let resumed_continuity = telemetry.advance_continuity();
        capture.write(&[0.0_f32; 1024]);
        let after_resume_first_half = processing.try_pop().unwrap();

        let slot = Arc::new(ArcSwapOption::<SpectrumSnapshot>::empty());
        let analysis_error = Arc::new(ArcSwapOption::<ChannelError>::empty());
        let mut processor = Processor::new(
            descriptor,
            FftConfig::new(2048, 2048, -160.0).unwrap(),
            ChannelSelection::MonoAverage,
            Arc::clone(&slot),
            analysis_error,
        );
        processor.resume_at(resumed_continuity);
        processor.process_block(&before_pause);
        processing.recycle(before_pause);
        processor.process_block(&after_resume_first_half);
        processing.recycle(after_resume_first_half);

        assert!(
            slot.load_full().is_none(),
            "a resumed FFT must not combine queued pre-pause audio with new audio"
        );

        capture.write(&[0.0_f32; 1024]);
        let after_resume_second_half = processing.try_pop().unwrap();
        processor.process_block(&after_resume_second_half);
        processing.recycle(after_resume_second_half);
        let snapshot = slot
            .load_full()
            .expect("two resumed halves fill one window");
        assert_eq!(snapshot.processed_through_frame, 3072);
        assert!(snapshot.bins_dbfs.iter().all(|value| *value == -160.0));
    }
}
