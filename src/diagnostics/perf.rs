//! Bounded measurement hooks for the performance harness in `tools/perf/`.
//! Timestamps are raw QueryPerformanceCounter ticks, the clock Python's `perf_counter`
//! reads, so the harness can place its own events on the same axis.

use std::{
    cell::Cell,
    fs,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use serde::Serialize;

use crate::{audio::AudioBlock, dataflow::RuntimeMetrics, diagnostics::probe::WorkerSample};

/// Sized for a run of about two minutes: 60 passes/s and the 100 blocks/s of a 10 ms
/// WASAPI period. Memory is committed up front, so larger tables would inflate private bytes.
const ROW_CAPACITY: usize = 8_192;
const POP_CAPACITY: usize = 8_192;
/// Four counter samples per second for the same two minutes.
const COUNTER_CAPACITY: usize = 512;
/// The largest built-in measurement layouts hold eight plots; more count as overflow.
const PLOTS_PER_ROW: usize = 8;
const POP_COLUMNS: [&str; 4] = ["start_frame", "frames", "callback", "pop"];
const COUNTER_COLUMNS: [&str; 16] = [
    "qpc",
    "worker_cpu_ns",
    "worker_allocations",
    "fft_evaluations",
    "spectrum_evaluations",
    "publications",
    "pcm_frames",
    "level_frames",
    "callbacks",
    "callback_allocations",
    "dropped_blocks",
    "subscriptions",
    "active_channel_nodes",
    "active_fft_nodes",
    "active_spectrum_nodes",
    "active_level_nodes",
];

static ENABLED: AtomicBool = AtomicBool::new(false);
static CALLBACKS: AtomicU64 = AtomicU64::new(0);
static CALLBACK_FRAMES: AtomicU64 = AtomicU64::new(0);
pub(crate) static CALLBACK_ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static CALLBACK_MAX_TICKS: AtomicU64 = AtomicU64::new(0);
static DROPPED_BLOCKS: AtomicU64 = AtomicU64::new(0);
static PUBLISHED_THROUGH: AtomicU64 = AtomicU64::new(0);
static PLOTS_OVERFLOW: AtomicU64 = AtomicU64::new(0);
/// Both tables assume one worker and one source timeline. A replacement source spawns
/// a second worker (which may stay silent and never pop) and restarts at frame 0, so
/// either a second spawn or a block starting before the last popped end stops recording
/// and marks the run unusable.
static WORKERS_STARTED: AtomicU64 = AtomicU64::new(0);
static POPPED_THROUGH: AtomicU64 = AtomicU64::new(0);
static SOURCE_REPLACED: AtomicBool = AtomicBool::new(false);
static COUNTERS_SAMPLED_AT: AtomicU64 = AtomicU64::new(0);
static POPS: OnceLock<Table<4>> = OnceLock::new();
static COUNTERS: OnceLock<Table<16>> = OnceLock::new();

thread_local! {
    static PLOTS: Cell<([[f32; 2]; PLOTS_PER_ROW], usize)> =
        const { Cell::new(([[0.0; 2]; PLOTS_PER_ROW], 0)) };
    /// UI thread: exclusive end of the frames already shown by an earlier pass.
    static SHOWN_THROUGH: Cell<u64> = const { Cell::new(0) };
    /// This pass: newest frontier readers handed out and when, and when a reader first
    /// handed out a result past `SHOWN_THROUGH` (the oldest new frame's first read).
    static READ: Cell<Read> = const { Cell::new(Read { frontier: 0, at: 0, first_new_at: 0 }) };
}

#[derive(Clone, Copy)]
struct Read {
    frontier: u64,
    at: u64,
    first_new_at: u64,
}

/// Append-only rows written by the analysis worker of one source; `len` publishes
/// complete rows to readers on other threads.
struct Table<const N: usize> {
    entries: Box<[[AtomicU64; N]]>,
    len: AtomicUsize,
    overflow: AtomicU64,
}

impl<const N: usize> Table<N> {
    fn new(capacity: usize) -> Self {
        Self {
            entries: (0..capacity)
                .map(|_| std::array::from_fn(|_| AtomicU64::new(0)))
                .collect(),
            len: AtomicUsize::new(0),
            overflow: AtomicU64::new(0),
        }
    }

    fn push(&self, values: [u64; N]) {
        let index = self.len.load(Ordering::Relaxed);
        let Some(entry) = self.entries.get(index) else {
            self.overflow.fetch_add(1, Ordering::Relaxed);
            return;
        };
        for (slot, value) in entry.iter().zip(values) {
            slot.store(value, Ordering::Relaxed);
        }
        self.len.store(index + 1, Ordering::Release);
    }

    fn rows(&self) -> &[[AtomicU64; N]] {
        &self.entries[..self.len.load(Ordering::Acquire)]
    }

    fn snapshot(&self) -> Vec<[u64; N]> {
        self.rows()
            .iter()
            .map(|entry| std::array::from_fn(|i| entry[i].load(Ordering::Relaxed)))
            .collect()
    }

    fn bytes(&self) -> usize {
        self.entries.len() * size_of::<[AtomicU64; N]>()
    }
}

#[cfg(windows)]
pub(crate) fn now() -> u64 {
    let mut ticks = 0_i64;
    // SAFETY: the pointer is valid for the call; QPC cannot fail on Windows XP or later.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut ticks) };
    ticks as u64
}

#[cfg(windows)]
fn frequency() -> u64 {
    let mut frequency = 0_i64;
    // SAFETY: as in `now`.
    unsafe { windows_sys::Win32::System::Performance::QueryPerformanceFrequency(&mut frequency) };
    frequency as u64
}

#[cfg(not(windows))]
pub(crate) fn now() -> u64 {
    static EPOCH: std::sync::LazyLock<std::time::Instant> =
        std::sync::LazyLock::new(std::time::Instant::now);
    EPOCH.elapsed().as_nanos() as u64 + 1
}

#[cfg(not(windows))]
fn frequency() -> u64 {
    1_000_000_000
}

fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Removes `--perf-trace <dir>` from the arguments.
pub(crate) fn take_arg(args: &mut Vec<String>) -> Result<Option<PathBuf>, String> {
    let Some(index) = args.iter().position(|arg| arg == "--perf-trace") else {
        return Ok(None);
    };
    if index + 1 >= args.len() || args[index + 1].starts_with("--") {
        return Err("--perf-trace requires a directory".into());
    }
    let dir = PathBuf::from(args.remove(index + 1));
    args.remove(index);
    Ok(Some(dir))
}

/// Must run before the source opens so every callback is stamped.
pub(crate) fn enable() {
    let _ = POPS.set(Table::new(POP_CAPACITY));
    let _ = COUNTERS.set(Table::new(COUNTER_CAPACITY));
    ENABLED.store(true, Ordering::Relaxed);
}

/// Real-time callback: atomics only. Returns the entry stamp (0 while disabled).
pub(crate) fn callback_begin(frames: usize) -> u64 {
    if !enabled() {
        return 0;
    }
    CALLBACKS.fetch_add(1, Ordering::Relaxed);
    CALLBACK_FRAMES.fetch_add(frames as u64, Ordering::Relaxed);
    now()
}

pub(crate) fn callback_end(started: u64) {
    if started != 0 {
        CALLBACK_MAX_TICKS.fetch_max(now().saturating_sub(started), Ordering::Relaxed);
    }
}

pub(crate) fn dropped_block() {
    DROPPED_BLOCKS.fetch_add(1, Ordering::Relaxed);
}

/// Before a Dataflow worker thread starts.
pub(crate) fn worker_started() {
    if WORKERS_STARTED.fetch_add(1, Ordering::Relaxed) > 0 {
        SOURCE_REPLACED.store(true, Ordering::Relaxed);
    }
}

/// Worker, after the silence fill moved the block onto the engine timeline.
/// Inserted silence has no callback stamp and makes its frames unavailable.
pub(crate) fn popped(block: &AudioBlock) {
    let Some(pops) = POPS.get() else {
        return;
    };
    if SOURCE_REPLACED.load(Ordering::Relaxed) {
        return;
    }
    let end = block.start_frame + block.frame_count as u64;
    if block.start_frame < POPPED_THROUGH.fetch_max(end, Ordering::Relaxed) {
        SOURCE_REPLACED.store(true, Ordering::Relaxed);
        return;
    }
    pops.push([
        block.start_frame,
        block.frame_count as u64,
        block.callback_ticks,
        now(),
    ]);
}

/// Worker, once per loop: about four counter samples per second, independent of
/// whether any window draws. `sample` runs only when one is due.
pub(crate) fn worker_counters(sample: impl FnOnce() -> (RuntimeMetrics, WorkerSample)) {
    let Some(counters) = COUNTERS.get() else {
        return;
    };
    if SOURCE_REPLACED.load(Ordering::Relaxed) {
        return;
    }
    let at = now();
    if at.saturating_sub(COUNTERS_SAMPLED_AT.load(Ordering::Relaxed)) < frequency() / 4 {
        return;
    }
    COUNTERS_SAMPLED_AT.store(at, Ordering::Relaxed);
    let (metrics, worker) = sample();
    counters.push([
        at,
        worker.cpu.map_or(u64::MAX, |cpu| cpu.as_nanos() as u64),
        worker.allocations,
        metrics.fft_evaluations,
        metrics.spectrum_evaluations,
        metrics.publications,
        metrics.pcm_frames,
        metrics.level_frames,
        CALLBACKS.load(Ordering::Relaxed),
        CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
        DROPPED_BLOCKS.load(Ordering::Relaxed),
        metrics.subscriptions,
        metrics.active_channel_nodes,
        metrics.active_fft_nodes,
        metrics.active_spectrum_nodes,
        metrics.active_level_nodes,
    ]);
}

pub(crate) fn published(processed_through_frame: u64) {
    PUBLISHED_THROUGH.fetch_max(processed_through_frame, Ordering::Relaxed);
}

/// UI thread, whenever a reader hands out a result.
pub(crate) fn read(processed_through_frame: u64) {
    if !enabled() {
        return;
    }
    READ.with(|read| {
        let mut current = read.get();
        if processed_through_frame <= current.frontier {
            return;
        }
        let at = now();
        if current.first_new_at == 0 && processed_through_frame > SHOWN_THROUGH.get() {
            current.first_new_at = at;
        }
        current.frontier = processed_through_frame;
        current.at = at;
        read.set(current);
    });
}

/// Called once per drawn plot area, in logical points.
pub(crate) fn plot(width: f32, height: f32) {
    PLOTS.with(|plots| {
        let (mut values, count) = plots.get();
        match values.get_mut(count) {
            Some(slot) => *slot = [width, height],
            None => {
                PLOTS_OVERFLOW.fetch_add(1, Ordering::Relaxed);
            }
        }
        plots.set((values, count + 1));
    });
}

/// Callback entry stamp of the block holding `frame`, if the worker recorded one.
fn callback_of(frame: u64) -> Option<u64> {
    let entries = POPS.get()?.rows();
    let start = |index: usize| entries[index][0].load(Ordering::Relaxed);
    // Blocks arrive in timeline order, so the last block starting at or before `frame`.
    let after = entries.partition_point(|entry| entry[0].load(Ordering::Relaxed) <= frame);
    let index = after.checked_sub(1)?;
    let frames = entries[index][1].load(Ordering::Relaxed);
    let callback = entries[index][2].load(Ordering::Relaxed);
    (frame < start(index) + frames && callback != 0).then_some(callback)
}

#[derive(Clone, Copy, Default, Serialize)]
struct Row {
    /// `logic` entry, the start of the host pass.
    start: u64,
    logic_ticks: u64,
    /// Window and settings pass bodies, without the host's egui run, tessellation and painting.
    ui_ticks: u64,
    /// The rest of each window's pass: egui run, tessellation, submission and present.
    render_ticks: u64,
    windows_drawn: u32,
    content_points: [f32; 2],
    pixels_per_point: f32,
    plots_points: [[f32; 2]; PLOTS_PER_ROW],
    plots: u32,
    /// Published frontier at the start of `ui` (diagnostic: publication versus reading).
    published: u64,
    /// Newest result frontier the readers handed out in this pass and when (D8).
    frontier: u64,
    read_at: u64,
    /// When a reader first handed out a result covering the oldest newly shown frame.
    oldest_read_at: u64,
    /// Callback entry of the first and last frame newly shown by this pass.
    oldest_callback: u64,
    newest_callback: u64,
    newly_shown_frames: u64,
}

/// UI-thread state for one host pass (a batch of window passes).
pub(crate) struct UiPass {
    started: u64,
    published: u64,
    body_ticks: Cell<u64>,
    render_ticks: Cell<u64>,
    windows_drawn: Cell<u32>,
    content: Cell<([f32; 2], f32)>,
}

impl UiPass {
    pub(crate) fn begin() -> Self {
        PLOTS.with(|plots| plots.set(([[0.0; 2]; PLOTS_PER_ROW], 0)));
        READ.with(|read| {
            read.set(Read {
                frontier: 0,
                at: 0,
                first_new_at: 0,
            });
        });
        let started = now();
        Self {
            started,
            published: PUBLISHED_THROUGH.load(Ordering::Relaxed),
            body_ticks: Cell::new(0),
            render_ticks: Cell::new(0),
            windows_drawn: Cell::new(0),
            content: Cell::new(([0.0; 2], 0.0)),
        }
    }

    /// Body of one window's pass; `drawn` marks a Workspace window that painted.
    pub(crate) fn body(&self, started: u64, drawn: bool, ui: &egui::Ui) {
        self.body_ticks
            .set(self.body_ticks.get() + now().saturating_sub(started));
        if drawn {
            if self.windows_drawn.get() == 0 {
                let size = ui.max_rect().size();
                self.content
                    .set(([size.x, size.y], ui.ctx().pixels_per_point()));
            }
            self.windows_drawn.set(self.windows_drawn.get() + 1);
        }
    }

    /// After a window's pass and paint: everything but the body was host work.
    pub(crate) fn viewport(&self, call_started: u64, body_before: u64) {
        let call = now().saturating_sub(call_started);
        let body = self.body_ticks.get() - body_before;
        self.render_ticks
            .set(self.render_ticks.get() + call.saturating_sub(body));
    }

    pub(crate) fn body_so_far(&self) -> u64 {
        self.body_ticks.get()
    }
}

pub(crate) struct PerfTrace {
    dir: PathBuf,
    rows: Vec<Row>,
    rows_overflow: u64,
    logic: (u64, u64),
}

impl PerfTrace {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            rows: Vec::with_capacity(ROW_CAPACITY),
            rows_overflow: 0,
            logic: (0, 0),
        }
    }

    pub(crate) fn logic(&mut self, started: u64) {
        self.logic = (started, now().saturating_sub(started));
    }

    /// `ended` is taken before this bookkeeping, which the UI wall excludes.
    pub(crate) fn end(&mut self, pass: UiPass, ended: u64) {
        let ui_total = ended.saturating_sub(pass.started);
        let render_ticks = pass.render_ticks.get();
        let (content, pixels_per_point) = pass.content.get();
        let (plots_points, plots) = PLOTS.with(Cell::get);
        let read = READ.with(Cell::get);
        let mut row = Row {
            start: self.logic.0,
            logic_ticks: self.logic.1,
            ui_ticks: ui_total.saturating_sub(render_ticks),
            render_ticks,
            windows_drawn: pass.windows_drawn.get(),
            content_points: content,
            pixels_per_point,
            plots_points,
            plots: plots as u32,
            published: pass.published,
            frontier: read.frontier,
            read_at: read.at,
            ..Row::default()
        };
        let shown_through = SHOWN_THROUGH.get();
        if read.frontier > shown_through {
            row.newly_shown_frames = read.frontier - shown_through;
            row.oldest_read_at = read.first_new_at;
            row.oldest_callback = callback_of(shown_through).unwrap_or(0);
            row.newest_callback = callback_of(read.frontier - 1).unwrap_or(0);
            SHOWN_THROUGH.set(read.frontier);
        }
        if self.rows.len() < ROW_CAPACITY {
            self.rows.push(row);
        } else {
            self.rows_overflow += 1;
        }
    }

    pub(crate) fn write(&self) -> Result<PathBuf, String> {
        #[derive(Serialize)]
        struct Report<'a> {
            schema_version: u32,
            clock: &'static str,
            ticks_per_second: u64,
            written_at: u64,
            row_capacity: usize,
            rows_overflow: u64,
            pops_overflow: u64,
            counters_overflow: u64,
            plots_overflow: u64,
            source_replaced: bool,
            preallocated_bytes: usize,
            callbacks: u64,
            callback_frames: u64,
            callback_allocations: u64,
            callback_max_ticks: u64,
            dropped_blocks: u64,
            row_columns: &'static str,
            rows: &'a [Row],
            pop_columns: [&'static str; 4],
            pops: Vec<[u64; 4]>,
            /// Cumulative except the trailing gauges; `worker_cpu_ns` is `u64::MAX` when unknown.
            counter_columns: [&'static str; 16],
            counters: Vec<[u64; 16]>,
        }
        let (pops, pops_overflow, pops_bytes) = POPS.get().map_or((Vec::new(), 0, 0), |t| {
            (t.snapshot(), t.overflow.load(Ordering::Relaxed), t.bytes())
        });
        let (counters, counters_overflow, counters_bytes) =
            COUNTERS.get().map_or((Vec::new(), 0, 0), |t| {
                (t.snapshot(), t.overflow.load(Ordering::Relaxed), t.bytes())
            });
        let report = Report {
            schema_version: 4,
            clock: "QueryPerformanceCounter ticks",
            ticks_per_second: frequency(),
            written_at: now(),
            row_capacity: ROW_CAPACITY,
            rows_overflow: self.rows_overflow,
            pops_overflow,
            counters_overflow,
            plots_overflow: PLOTS_OVERFLOW.load(Ordering::Relaxed),
            source_replaced: SOURCE_REPLACED.load(Ordering::Relaxed),
            preallocated_bytes: ROW_CAPACITY * size_of::<Row>() + pops_bytes + counters_bytes,
            callbacks: CALLBACKS.load(Ordering::Relaxed),
            callback_frames: CALLBACK_FRAMES.load(Ordering::Relaxed),
            callback_allocations: CALLBACK_ALLOCATIONS.load(Ordering::Relaxed),
            callback_max_ticks: CALLBACK_MAX_TICKS.load(Ordering::Relaxed),
            dropped_blocks: DROPPED_BLOCKS.load(Ordering::Relaxed),
            row_columns: "see Row in src/perf.rs; ticks are QPC",
            rows: &self.rows,
            pop_columns: POP_COLUMNS,
            pops,
            counter_columns: COUNTER_COLUMNS,
            counters,
        };
        let path = self.dir.join("perf.json");
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let text = serde_json::to_string(&report).map_err(|e| e.to_string())?;
        fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(path)
    }
}
