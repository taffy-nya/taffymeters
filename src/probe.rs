//! Test / automation-only measurement of the analysis worker: heap allocations made
//! on that thread and its CPU time. Never compiled into the product build.

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

struct CountingAllocator;

thread_local! {
    /// Counter of the probe that owns the current thread, or null when uncounted.
    static COUNTER: Cell<*const AtomicU64> = const { Cell::new(std::ptr::null()) };
}

fn count_allocation() {
    let _ = COUNTER.try_with(|counter| {
        let counter = counter.get();
        if !counter.is_null() {
            // SAFETY: `WorkerProbe::count_this_thread` keeps the probe alive for as
            // long as the pointer is installed and clears it before releasing it.
            unsafe { (*counter).fetch_add(1, Ordering::Relaxed) };
        }
    });
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count_allocation();
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

/// Allocation count and CPU time of one worker thread.
#[derive(Default)]
pub(crate) struct WorkerProbe {
    allocations: AtomicU64,
    thread: OnceLock<ThreadCpu>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkerSample {
    pub(crate) allocations: u64,
    /// None where thread CPU time is unavailable (non-Windows or unit tests).
    pub(crate) cpu: Option<Duration>,
    /// CPU cycles charged to the thread (`QueryThreadCycleTime`), finer than `cpu`.
    pub(crate) cycles: Option<u64>,
}

impl WorkerProbe {
    /// Count allocations of the calling thread until the returned guard drops.
    pub(crate) fn count_this_thread(self: &Arc<Self>) -> CountGuard {
        let probe = Arc::clone(self);
        CountGuard {
            _restore: install(&probe.allocations),
            _probe: probe,
        }
    }

    pub(crate) fn attach(&self, thread: &JoinHandle<()>) {
        if let Some(cpu) = ThreadCpu::of(thread) {
            let _ = self.thread.set(cpu);
        }
    }

    pub(crate) fn sample(&self) -> WorkerSample {
        WorkerSample {
            allocations: self.allocations.load(Ordering::Relaxed),
            cpu: self.thread.get().map(ThreadCpu::cpu_time),
            cycles: self.thread.get().map(ThreadCpu::cycles),
        }
    }
}

/// Restores the calling thread's previous counter when dropped, including during
/// unwinding. It holds a raw pointer, so it is neither `Send` nor `Sync` and must
/// be dropped on the thread that installed it.
struct Restore {
    previous: *const AtomicU64,
}

impl Drop for Restore {
    fn drop(&mut self) {
        COUNTER.with(|counter| counter.set(self.previous));
    }
}

/// Point the calling thread's allocation counter at `counter` until the guard drops.
/// The caller keeps `counter` alive for at least as long as the guard.
fn install(counter: &AtomicU64) -> Restore {
    Restore {
        previous: COUNTER.with(|current| current.replace(counter)),
    }
}

/// Counts the worker thread's allocations while alive; thread-affine through
/// `Restore`. Field order matters: the counter pointer is removed before the probe
/// that owns the counter is released.
pub(crate) struct CountGuard {
    _restore: Restore,
    _probe: Arc<WorkerProbe>,
}

/// Allocations made by the calling thread while `action` runs.
#[cfg(test)]
pub(crate) fn allocations_during(action: impl FnOnce()) -> u64 {
    let counter = AtomicU64::new(0);
    let restore = install(&counter);
    action();
    drop(restore);
    counter.load(Ordering::Relaxed)
}

#[cfg(all(windows, feature = "automation"))]
struct ThreadCpu(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a duplicated thread handle may be used from any thread.
#[cfg(all(windows, feature = "automation"))]
unsafe impl Send for ThreadCpu {}
#[cfg(all(windows, feature = "automation"))]
unsafe impl Sync for ThreadCpu {}

#[cfg(all(windows, feature = "automation"))]
impl ThreadCpu {
    fn of(thread: &JoinHandle<()>) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle},
            System::Threading::GetCurrentProcess,
        };
        let mut handle = std::ptr::null_mut();
        // SAFETY: the source handle is owned by the live JoinHandle; the duplicate is
        // owned by the returned value and closed on drop.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                thread.as_raw_handle(),
                GetCurrentProcess(),
                &mut handle,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        (ok != 0).then_some(Self(handle))
    }

    fn cpu_time(&self) -> Duration {
        use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetThreadTimes};
        let mut times = [FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        }; 4];
        let [created, exited, kernel, user] = &mut times;
        // SAFETY: `self.0` is a valid thread handle; the out pointers are live locals.
        let ok = unsafe { GetThreadTimes(self.0, created, exited, kernel, user) };
        if ok == 0 {
            return Duration::ZERO;
        }
        let ticks = |time: &FILETIME| {
            (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
        };
        Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
    }

    fn cycles(&self) -> u64 {
        let mut cycles = 0;
        // SAFETY: `self.0` is a valid thread handle; the out pointer is a live local.
        unsafe {
            windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime(
                self.0,
                &mut cycles,
            )
        };
        cycles
    }
}

#[cfg(all(windows, feature = "automation"))]
impl Drop for ThreadCpu {
    fn drop(&mut self) {
        // SAFETY: the handle was duplicated for this value and is closed once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(not(all(windows, feature = "automation")))]
struct ThreadCpu;

#[cfg(not(all(windows, feature = "automation")))]
impl ThreadCpu {
    fn of(_thread: &JoinHandle<()>) -> Option<Self> {
        None
    }

    fn cpu_time(&self) -> Duration {
        Duration::ZERO
    }

    fn cycles(&self) -> u64 {
        0
    }
}
