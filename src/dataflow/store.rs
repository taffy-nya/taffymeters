//! All outputs of one processed block are published under one lock; route changes are atomic too.
//! The shared lock protects pointer operations, never analysis.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

use crate::snapshot::Frame;

#[derive(Default)]
pub(crate) struct Store {
    /// Output rings, indexed by ring id; `None` slots are free.
    pub(crate) rings: Vec<Option<Ring>>,
    pub(crate) routes: HashMap<u64, Route>,
}

#[derive(Default)]
pub(crate) struct Ring {
    /// Oldest first; result versions increase.
    pub(crate) frames: VecDeque<Arc<Frame>>,
}

pub(crate) struct Route {
    pub(crate) revision: u64,
    pub(crate) paused: bool,
    /// Indexed by `OutputId`.
    pub(crate) outputs: Vec<RouteOutput>,
    /// While paused: the latest visible result of each output at the pause point.
    pub(crate) frozen: Vec<Option<Arc<Frame>>>,
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
