//! Immediate child viewports share one invisible host pass, so all windows paint together.
//! Separate deferred viewports can starve unfocused windows when Windows keeps selecting
//! the focused window's pending WM_PAINT.

use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use eframe::egui::{self, ViewportCommand};

use crate::workspace::{FramePacer, WindowId, Workspace, WorkspaceAction};

#[cfg(feature = "automation")]
type SharedTrace = Option<Arc<Mutex<crate::automation::Trace>>>;

pub(crate) struct App {
    workspace: Arc<Mutex<Workspace>>,
    started_at: Instant,
    pacer: Arc<FramePacer>,
    wake_pending: Arc<AtomicBool>,
    /// Whether any Workspace window drew in the last host pass.
    any_window_shown: Arc<AtomicBool>,
    #[cfg(feature = "automation")]
    trace: SharedTrace,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

impl App {
    pub(crate) fn new(workspace: Workspace) -> Self {
        let pacer = Arc::clone(workspace.frame_pacer());
        Self {
            workspace: Arc::new(Mutex::new(workspace)),
            started_at: Instant::now(),
            pacer,
            wake_pending: Arc::new(AtomicBool::new(false)),
            any_window_shown: Arc::new(AtomicBool::new(true)),
            #[cfg(feature = "automation")]
            trace: None,
        }
    }

    #[cfg(feature = "automation")]
    pub(crate) fn with_trace(mut self, trace: crate::automation::Trace) -> Self {
        self.trace = Some(Arc::new(Mutex::new(trace)));
        self
    }

    /// Coalesces data wake-ups to one per host pass, paced by the frame-rate limit; none
    /// while all Workspace windows are minimized. Endpoint changes always wake the host.
    pub(crate) fn install_waker(&self, ctx: &egui::Context) {
        let wake = |ctx: &egui::Context, pending: &AtomicBool, pacer: &FramePacer| {
            if !pending.swap(true, Ordering::AcqRel) {
                let delay = pacer.request_delay(Instant::now(), Duration::ZERO);
                ctx.request_repaint_after_for(delay, egui::ViewportId::ROOT);
            }
        };
        let (data_ctx, pending, shown, pacer) = (
            ctx.clone(),
            Arc::clone(&self.wake_pending),
            Arc::clone(&self.any_window_shown),
            Arc::clone(&self.pacer),
        );
        let (endpoint_ctx, endpoint_pending, endpoint_pacer) = (
            ctx.clone(),
            Arc::clone(&self.wake_pending),
            Arc::clone(&self.pacer),
        );
        let mut workspace = lock(&self.workspace);
        workspace.set_publication_waker(Some(Box::new(move || {
            if shown.load(Ordering::Relaxed) {
                wake(&data_ctx, &pending, &pacer);
            }
        })));
        workspace.set_endpoint_waker(Some(Arc::new(move || {
            wake(&endpoint_ctx, &endpoint_pending, &endpoint_pacer);
        })));
    }

    #[cfg(windows)]
    pub(crate) fn save_on_session_end(&self) {
        let workspace = Arc::downgrade(&self.workspace);
        crate::workspace::on_session_end(Box::new(move || {
            let Some(workspace) = workspace.upgrade() else {
                return;
            };
            // Waiting would deadlock if a pass holds the lock; the debounced save covers it.
            let mut workspace = match workspace.try_lock() {
                Ok(workspace) => workspace,
                Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => return,
            };
            workspace.autosave();
        }));
    }

    /// eframe throttles hidden or minimized roots to 10 fps, so this off-screen host stays visible.
    pub(crate) fn host_builder() -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title("Taffymeters")
            .with_icon(crate::icon::large())
            .with_decorations(false)
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_mouse_passthrough(true)
            .with_inner_size([1.0, 1.0])
            .with_position([-20_000.0, -20_000.0])
    }
}

fn window_pass(
    workspace: &Mutex<Workspace>,
    window_id: WindowId,
    ui: &mut egui::Ui,
    started_at: Instant,
    #[cfg(feature = "automation")] trace: &SharedTrace,
) -> bool {
    let mut workspace = lock(workspace);
    if ui.ctx().input(|input| input.viewport().close_requested()) && workspace.has_window(window_id)
    {
        let _ = workspace.apply_action(WorkspaceAction::CloseWindow { window_id });
    }
    let drawn = workspace.window_ui(window_id, ui, started_at.elapsed());
    #[cfg(feature = "automation")]
    if let Some(trace) = trace {
        let mut trace = lock(trace);
        if drawn {
            trace.pass(&workspace, window_id);
            trace.targets(&workspace, window_id);
        }
        trace.state(&workspace);
    }
    drawn
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let predicted = ctx.input(|input| input.predicted_dt);
        let predicted = Duration::try_from_secs_f32(predicted).unwrap_or_default();
        // Before `ui` clears the wake flag: a wake-up in between must see this pass's start.
        self.pacer.begin_pass(Instant::now(), predicted);
        let mut workspace = lock(&self.workspace);
        workspace.tick(self.started_at.elapsed());
        if workspace.windows().is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        } else if let Some(delay) = [
            workspace.autosave_when_settled(),
            workspace.endpoint_retry_delay(),
        ]
        .into_iter()
        .flatten()
        .min()
        {
            let delay = self.pacer.request_delay(Instant::now(), delay);
            ctx.request_repaint_after_for(delay, egui::ViewportId::ROOT);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.wake_pending.store(false, Ordering::Release);
        let windows = lock(&self.workspace).windows();
        let mut shown = false;
        for window in windows {
            let window_id = window.id;
            shown |=
                ctx.show_viewport_immediate(window_id.viewport_id(), window.builder, |ui, _| {
                    window_pass(
                        &self.workspace,
                        window_id,
                        ui,
                        self.started_at,
                        #[cfg(feature = "automation")]
                        &self.trace,
                    )
                });
        }
        let settings = {
            let workspace = lock(&self.workspace);
            workspace
                .settings_builder()
                .zip(workspace.settings_viewport_id())
        };
        if let Some((builder, viewport_id)) = settings {
            ctx.show_viewport_immediate(viewport_id, builder, |ui, _| {
                let mut workspace = lock(&self.workspace);
                if ui.ctx().input(|input| input.viewport().close_requested()) {
                    let _ = workspace.apply_action(WorkspaceAction::CloseSettings);
                } else {
                    workspace.settings_ui(ui, self.started_at.elapsed());
                }
                // Recorded here too: with every Workspace window minimized, no other
                // pass would report the settings window opening or closing.
                #[cfg(feature = "automation")]
                if let Some(trace) = &self.trace {
                    let mut trace = lock(trace);
                    if workspace.settings_open() {
                        trace.settings_targets(&workspace);
                    }
                    trace.state(&workspace);
                }
            });
        }
        self.any_window_shown.store(shown, Ordering::Release);
    }

    fn on_exit(&mut self) {
        let mut workspace = lock(&self.workspace);
        workspace.autosave();
        workspace.set_publication_waker(None);
        workspace.set_endpoint_waker(None);
        workspace.shutdown();
    }
}
