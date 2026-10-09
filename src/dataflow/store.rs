//! Published results shared between the worker and readers. The worker takes the
//! lock once per processed block to push that block's results (so outputs of one
//! block appear together) and on control changes; readers take it once per read.
//! Only pointer operations happen under the lock, never analysis.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use crate::snapshot::Frame;

#[derive(Default)]
pub(crate) struct Store {
    /// Output rings, indexed by ring id; `None` slots are free.
    pub(crate) rings: Vec<Option<Ring>>,
    /// What each live subscription reads, by subscription id.
    pub(crate) routes: HashMap<u64, Route>,
}

#[derive(Default)]
pub(crate) struct Ring {
    /// Oldest first; result versions increase.
    pub(crate) frames: VecDeque<Arc<Frame>>,
}

pub(crate) struct Route {
    /// Revision of the applied spec.
    pub(crate) revision: u64,
    /// Readers keep their last results while the subscription's analysis is paused.
    pub(crate) paused: bool,
    /// Indexed by `OutputId`.
    pub(crate) outputs: Vec<RouteOutput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RouteOutput {
    pub(crate) ring: usize,
    /// Only results that end after this source frame are visible (join, reset,
    /// resume and reconfiguration boundaries).
    pub(crate) start_frame: u64,
    /// History length in source frames; `None` for a Latest output.
    pub(crate) keep_frames: Option<u64>,
    /// Bumped when a reader must drop what it accumulated (reset).
    pub(crate) reset_epoch: u64,
    /// Bumped when a reader keeps its history but must mark a gap (resume).
    pub(crate) gap_epoch: u64,
}

pub(crate) type SharedStore = Arc<Mutex<Store>>;

pub(crate) fn lock(store: &Mutex<Store>) -> MutexGuard<'_, Store> {
    store.lock().unwrap_or_else(PoisonError::into_inner)
}
