use std::sync::{
    Mutex,
    atomic::{AtomicU8, AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SourceState {
    Starting = 0,
    Running = 1,
    Paused = 2,
    DeviceLost = 3,
    Stopped = 4,
}

impl SourceState {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Running,
            2 => Self::Paused,
            3 => Self::DeviceLost,
            4 => Self::Stopped,
            _ => Self::Starting,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStatus {
    pub state: SourceState,
    pub dropped_blocks: u64,
    pub invalid_samples: u64,
    pub detail: Option<String>,
}

pub(crate) struct SourceTelemetry {
    state: AtomicU8,
    continuity: AtomicU64,
    dropped_blocks: AtomicU64,
    invalid_samples: AtomicU64,
    detail: Mutex<Option<String>>,
}

impl SourceTelemetry {
    pub(crate) fn new() -> Self {
        Self {
            state: AtomicU8::new(SourceState::Starting as u8),
            continuity: AtomicU64::new(0),
            dropped_blocks: AtomicU64::new(0),
            invalid_samples: AtomicU64::new(0),
            detail: Mutex::new(None),
        }
    }

    pub(crate) fn set_state(&self, state: SourceState) {
        self.state.store(state as u8, Ordering::Release);
    }

    pub(crate) fn continuity(&self) -> u64 {
        self.continuity.load(Ordering::Acquire)
    }

    pub(crate) fn advance_continuity(&self) -> u64 {
        self.continuity.fetch_add(1, Ordering::AcqRel) + 1
    }

    pub(crate) fn add_dropped_block(&self) {
        self.dropped_blocks.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn add_invalid_samples(&self, count: u64) {
        self.invalid_samples.fetch_add(count, Ordering::Relaxed);
    }

    pub(crate) fn device_lost(&self, detail: String) {
        if let Ok(mut slot) = self.detail.lock() {
            *slot = Some(detail);
        }
        self.set_state(SourceState::DeviceLost);
    }

    pub(crate) fn snapshot(&self) -> SourceStatus {
        SourceStatus {
            state: SourceState::from_u8(self.state.load(Ordering::Acquire)),
            dropped_blocks: self.dropped_blocks.load(Ordering::Relaxed),
            invalid_samples: self.invalid_samples.load(Ordering::Relaxed),
            detail: self.detail.lock().ok().and_then(|detail| detail.clone()),
        }
    }
}
