//! Native adapter. egui's root viewport is an invisible host (1×1, off screen, no
//! taskbar entry); every Workspace window is an immediate child viewport rendered
//! inside the host's pass. One native paint therefore draws all windows together.
//!
//! Deferred viewports were tried first: each window then waits for its own Windows
//! WM_PAINT, and the OS keeps choosing the focused window whenever it has a paint
//! pending (input and vsync keep it pending), so other windows stopped updating.

use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use eframe::egui::{self, ViewportCommand};

use crate::workspace::{NativeDesktop, WindowId, Workspace, WorkspaceAction};

#[cfg(feature = "automation")]
type SharedTrace = Option<Arc<Mutex<crate::automation::Trace>>>;

pub struct App {
    workspace: Arc<Mutex<Workspace>>,
    started_at: Instant,
    /// A data wake-up is already queued for the next host pass.
    wake_pending: Arc<AtomicBool>,
    /// Some window was drawn in the last host pass (not all minimized).
    any_window_shown: Arc<AtomicBool>,
    #[cfg(feature = "automation")]
    trace: SharedTrace,
}

fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

impl App {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace: Arc::new(Mutex::new(workspace)),
            started_at: Instant::now(),
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

    /// Repaint the host when the analysis worker publishes new results, instead of
    /// redrawing at a fixed rate. At most one wake-up is queued per host pass, and
    /// none while every window is minimized.
    pub(crate) fn install_waker(&self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        let pending = Arc::clone(&self.wake_pending);
        let shown = Arc::clone(&self.any_window_shown);
        lock(&self.workspace).set_publication_waker(Some(Box::new(move || {
            if shown.load(Ordering::Relaxed) && !pending.swap(true, Ordering::AcqRel) {
                // Non-zero on purpose: a zero-delay request makes egui run two passes.
                ctx.request_repaint_after_for(
                    std::time::Duration::from_millis(1),
                    egui::ViewportId::ROOT,
                );
            }
        })));
    }

    /// The invisible host viewport. It must stay "visible" to the windowing system:
    /// eframe throttles hidden or minimized root viewports to 10 fps.
    pub(crate) fn host_builder() -> egui::ViewportBuilder {
        egui::ViewportBuilder::default()
            .with_title("Taffymeters")
            .with_decorations(false)
            .with_taskbar(false)
            .with_resizable(false)
            .with_active(false)
            .with_mouse_passthrough(true)
            .with_inner_size([1.0, 1.0])
            .with_position([-20_000.0, -20_000.0])
    }
}

/// One UI pass of a Workspace window inside its viewport.
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
    // A minimized window is not drawn; its Modules' continuous analysis continues and
    // work that only serves the display stops until it is shown again.
    let minimized = ui
        .ctx()
        .input(|input| input.viewport().minimized == Some(true));
    workspace.set_window_minimized(window_id, minimized);
    if minimized {
        return false;
    }
    workspace.window_ui(window_id, ui, started_at.elapsed(), &NativeDesktop);
    #[cfg(feature = "automation")]
    if let Some(trace) = trace {
        let mut trace = lock(trace);
        trace.pass(&workspace, window_id);
        trace.state(&workspace);
        trace.targets(&workspace, window_id);
    }
    true
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut workspace = lock(&self.workspace);
        workspace.tick(self.started_at.elapsed());
        if workspace.windows().is_empty() {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.wake_pending.store(false, Ordering::Release);
        let ctx = ui.ctx().clone();
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
                    return;
                }
                workspace.settings_ui(ui, self.started_at.elapsed(), &NativeDesktop);
                #[cfg(feature = "automation")]
                if let Some(trace) = &self.trace {
                    lock(trace).settings_targets(&workspace);
                }
            });
        }
        self.any_window_shown.store(shown, Ordering::Release);
    }

    fn on_exit(&mut self) {
        let mut workspace = lock(&self.workspace);
        workspace.autosave();
        workspace.set_publication_waker(None);
        workspace.shutdown();
    }
}
