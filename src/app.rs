//! Native adapter. egui's root viewport is an invisible host (1×1, off screen, no
//! taskbar entry); every Workspace window is an immediate child viewport rendered
//! inside the host's pass. One native paint therefore draws all windows together.
//!
//! Deferred viewports were tried first: each window then waits for its own Windows
//! WM_PAINT, and the OS keeps choosing the focused window whenever it has a paint
//! pending (input and vsync keep it pending), so other windows stopped updating.

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Instant,
};

use eframe::egui::{self, ViewportCommand};

use crate::workspace::{NativeDesktop, WindowId, Workspace, WorkspaceAction};

#[cfg(feature = "automation")]
type SharedTrace = Option<Arc<Mutex<crate::automation::Trace>>>;

pub struct App {
    workspace: Arc<Mutex<Workspace>>,
    started_at: Instant,
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
            #[cfg(feature = "automation")]
            trace: None,
        }
    }

    #[cfg(feature = "automation")]
    pub(crate) fn with_trace(mut self, trace: crate::automation::Trace) -> Self {
        self.trace = Some(Arc::new(Mutex::new(trace)));
        self
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
) {
    let mut workspace = lock(workspace);
    if ui.ctx().input(|input| input.viewport().close_requested()) && workspace.has_window(window_id)
    {
        let _ = workspace.apply_action(WorkspaceAction::CloseWindow { window_id });
    }
    // A minimized window is not drawn; its Modules' analysis continues.
    if ui
        .ctx()
        .input(|input| input.viewport().minimized == Some(true))
    {
        return;
    }
    workspace.window_ui(window_id, ui, started_at.elapsed(), &NativeDesktop);
    #[cfg(feature = "automation")]
    if let Some(trace) = trace {
        let mut trace = lock(trace);
        trace.pass(&workspace, window_id);
        trace.state(&workspace);
        trace.targets(&workspace, window_id);
    }
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
        let ctx = ui.ctx().clone();
        let windows = lock(&self.workspace).windows();
        for window in windows {
            let window_id = window.id;
            ctx.show_viewport_immediate(window_id.viewport_id(), window.builder, |ui, _| {
                window_pass(
                    &self.workspace,
                    window_id,
                    ui,
                    self.started_at,
                    #[cfg(feature = "automation")]
                    &self.trace,
                );
            });
        }
    }

    fn on_exit(&mut self) {
        lock(&self.workspace).shutdown();
    }
}
