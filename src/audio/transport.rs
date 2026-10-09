use std::{error::Error, fmt, sync::Arc, thread::Thread};

use cpal::Sample;
use ringbuf::{
    HeapCons, HeapProd, HeapRb,
    traits::{Consumer, Producer, Split},
};

use super::SourceTelemetry;

pub(crate) struct AudioBlock {
    pub(crate) sequence: u64,
    pub(crate) start_frame: u64,
    pub(crate) continuity: u64,
    pub(crate) frame_count: usize,
    samples: Box<[f32]>,
}

impl AudioBlock {
    fn new(sample_capacity: usize) -> Self {
        Self {
            sequence: 0,
            start_frame: 0,
            continuity: 0,
            frame_count: 0,
            samples: vec![0.0; sample_capacity].into_boxed_slice(),
        }
    }

    pub(crate) fn samples(&self, channels: usize) -> &[f32] {
        &self.samples[..self.frame_count * channels]
    }
}

pub(crate) struct CaptureEndpoint {
    free: HeapCons<AudioBlock>,
    ready: HeapProd<AudioBlock>,
    channels: usize,
    max_block_frames: usize,
    next_sequence: u64,
    next_frame: u64,
    telemetry: Arc<SourceTelemetry>,
    worker: Option<Thread>,
}

pub(crate) struct ProcessingEndpoint {
    ready: HeapCons<AudioBlock>,
    free: HeapProd<AudioBlock>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TransportError {
    ZeroBlocks,
    ZeroBlockFrames,
    SampleCapacityOverflow,
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroBlocks => f.write_str("transport must contain at least one block"),
            Self::ZeroBlockFrames => f.write_str("transport block must contain at least one frame"),
            Self::SampleCapacityOverflow => f.write_str("transport sample capacity overflow"),
        }
    }
}

impl Error for TransportError {}

pub(crate) fn create_block_transport(
    channels: usize,
    block_count: usize,
    max_block_frames: usize,
    telemetry: Arc<SourceTelemetry>,
) -> Result<(CaptureEndpoint, ProcessingEndpoint), TransportError> {
    if block_count == 0 {
        return Err(TransportError::ZeroBlocks);
    }
    if max_block_frames == 0 {
        return Err(TransportError::ZeroBlockFrames);
    }
    let sample_capacity = channels
        .checked_mul(max_block_frames)
        .ok_or(TransportError::SampleCapacityOverflow)?;

    let free_ring = HeapRb::new(block_count);
    let (mut free_producer, free_consumer) = free_ring.split();
    for _ in 0..block_count {
        let pushed = free_producer.try_push(AudioBlock::new(sample_capacity));
        debug_assert!(pushed.is_ok());
    }

    let ready_ring = HeapRb::new(block_count);
    let (ready_producer, ready_consumer) = ready_ring.split();

    Ok((
        CaptureEndpoint {
            free: free_consumer,
            ready: ready_producer,
            channels,
            max_block_frames,
            next_sequence: 0,
            next_frame: 0,
            telemetry,
            worker: None,
        },
        ProcessingEndpoint {
            ready: ready_consumer,
            free: free_producer,
        },
    ))
}

impl CaptureEndpoint {
    pub(crate) fn set_worker(&mut self, worker: Thread) {
        self.worker = Some(worker);
    }

    pub(crate) fn write<T>(&mut self, data: &[T])
    where
        T: Sample,
        f32: cpal::FromSample<T>,
    {
        let complete_frames = data.len() / self.channels;
        let remainder = data.len() % self.channels;
        if remainder != 0 {
            self.telemetry.add_invalid_samples(remainder as u64);
        }

        let mut frame_offset = 0;
        while frame_offset < complete_frames {
            let frame_count = (complete_frames - frame_offset).min(self.max_block_frames);
            let sequence = self.next_sequence;
            let start_frame = self.next_frame;
            self.next_sequence = self.next_sequence.wrapping_add(1);
            self.next_frame = self.next_frame.saturating_add(frame_count as u64);

            let Some(mut block) = self.free.try_pop() else {
                self.telemetry.add_dropped_block();
                frame_offset += frame_count;
                continue;
            };

            block.sequence = sequence;
            block.start_frame = start_frame;
            block.continuity = self.telemetry.continuity();
            block.frame_count = frame_count;
            let sample_offset = frame_offset * self.channels;
            let sample_count = frame_count * self.channels;
            let source = &data[sample_offset..sample_offset + sample_count];
            let destination = &mut block.samples[..sample_count];
            let mut invalid = 0_u64;
            for (output, input) in destination.iter_mut().zip(source.iter().copied()) {
                let sample = f32::from_sample(input);
                if sample.is_finite() {
                    *output = sample;
                } else {
                    *output = 0.0;
                    invalid += 1;
                }
            }
            if invalid != 0 {
                self.telemetry.add_invalid_samples(invalid);
            }

            if self.ready.try_push(block).is_err() {
                // A block came from the paired free queue, so the ready queue must have room.
                // Count and discard rather than ever waiting in the audio callback if that
                // invariant is violated.
                self.telemetry.add_dropped_block();
            }
            frame_offset += frame_count;
        }

        if complete_frames != 0
            && let Some(worker) = &self.worker
        {
            worker.unpark();
        }
    }
}

impl ProcessingEndpoint {
    pub(crate) fn try_pop(&mut self) -> Option<AudioBlock> {
        self.ready.try_pop()
    }

    pub(crate) fn recycle(&mut self, block: AudioBlock) {
        let recycled = self.free.try_push(block);
        debug_assert!(recycled.is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocations_during(action: impl FnOnce()) -> u64 {
        crate::probe::allocations_during(action)
    }

    #[test]
    fn callback_write_is_allocation_free_and_preserves_complete_blocks() {
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, mut processing) =
            create_block_transport(2, 4, 64, Arc::clone(&telemetry)).unwrap();
        let input = [0.25_f32; 128];

        let _ = allocations_during(|| {});
        let allocations = allocations_during(|| capture.write(&input));

        assert_eq!(allocations, 0);
        let block = processing.try_pop().unwrap();
        assert_eq!(block.frame_count, 64);
        assert_eq!(block.samples(2), input);
        assert!(processing.try_pop().is_none());
    }

    #[test]
    fn a_full_transport_drops_whole_blocks_and_keeps_source_gaps() {
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, mut processing) =
            create_block_transport(2, 1, 2, Arc::clone(&telemetry)).unwrap();

        capture.write(&[1.0_f32, 2.0, 3.0, 4.0]);
        capture.write(&[5.0_f32, 6.0, 7.0, 8.0]);
        let first = processing.try_pop().unwrap();
        assert_eq!((first.sequence, first.start_frame), (0, 0));
        processing.recycle(first);
        capture.write(&[9.0_f32, 10.0, 11.0, 12.0]);
        let after_gap = processing.try_pop().unwrap();
        assert_eq!((after_gap.sequence, after_gap.start_frame), (2, 4));
        assert_eq!(telemetry.snapshot().dropped_blocks, 1);
    }
}
