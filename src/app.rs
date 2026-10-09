//! Native adapter: maps Workspace windows to egui viewports. The root viewport is
//! the first Workspace window; every other window is a deferred viewport sharing the
//! same Workspace. Closing the root window while others remain hides the root
//! viewport (it keeps running `logic`) instead of ending the process.

use std::{
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Instant,
};

use eframe::egui::{self, ViewportCommand};

use crate::workspace::{NativeDesktop, ROOT_WINDOW, WindowId, Workspace, WorkspaceAction};

#[cfg(feature = "automation")]
type SharedTrace = Option<Arc<Mutex<crate::automation::Trace>>>;

pub struct App {
    workspace: Arc<Mutex<Workspace>>,
    started_at: Instant,
    root_hidden: bool,
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
            root_hidden: false,
            #[cfg(feature = "automation")]
            trace: None,
        }
    }

    #[cfg(feature = "automation")]
    pub(crate) fn with_trace(mut self, trace: crate::automation::Trace) -> Self {
        self.trace = Some(Arc::new(Mutex::new(trace)));
        self
    }

    pub(crate) fn root_builder(&self) -> egui::ViewportBuilder {
        lock(&self.workspace)
            .windows()
            .into_iter()
            .find(|window| window.id == ROOT_WINDOW)
            .map(|window| window.builder)
            .unwrap_or_default()
    }
}

/// One UI pass of a Workspace window, shared by the root and deferred viewports.
fn window_pass(
    workspace: &Mutex<Workspace>,
    window_id: WindowId,
    ui: &mut egui::Ui,
    started_at: Instant,
    #[cfg(feature = "automation")] trace: &SharedTrace,
) {
    let mut workspace = lock(workspace);
    workspace.window_ui(window_id, ui, started_at.elapsed(), &NativeDesktop);
    #[cfg(feature = "automation")]
    if let Some(trace) = trace {
        let mut trace = lock(trace);
        trace.state(&workspace);
        trace.targets(&workspace, window_id);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let mut workspace = lock(&self.workspace);
        workspace.tick(self.started_at.elapsed());
        let windows = workspace.windows();
        if ctx.input(|input| input.viewport().close_requested())
            && workspace.has_window(ROOT_WINDOW)
            && windows.len() > 1
        {
            // Other windows still use the source: close only this window.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            let _ = workspace.apply_action(WorkspaceAction::CloseWindow {
                window_id: ROOT_WINDOW,
            });
        }
        let windows = workspace.windows();
        if !workspace.has_window(ROOT_WINDOW) {
            if windows.is_empty() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else if !self.root_hidden {
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.root_hidden = true;
            }
        }
        #[cfg(feature = "automation")]
        if let Some(trace) = &self.trace {
            lock(trace).state(&workspace);
        }
        drop(workspace);
        for window in windows
            .into_iter()
            .filter(|window| window.id != ROOT_WINDOW)
        {
            let workspace = Arc::clone(&self.workspace);
            let started_at = self.started_at;
            let window_id = window.id;
            #[cfg(feature = "automation")]
            let trace = self.trace.clone();
            ctx.show_viewport_deferred(window_id.viewport_id(), window.builder, move |ui, _| {
                if ui.ctx().input(|input| input.viewport().close_requested()) {
                    let mut workspace = lock(&workspace);
                    if workspace.has_window(window_id) {
                        let _ = workspace.apply_action(WorkspaceAction::CloseWindow { window_id });
                    }
                }
                window_pass(
                    &workspace,
                    window_id,
                    ui,
                    started_at,
                    #[cfg(feature = "automation")]
                    &trace,
                );
            });
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if lock(&self.workspace).has_window(ROOT_WINDOW) {
            window_pass(
                &self.workspace,
                ROOT_WINDOW,
                ui,
                self.started_at,
                #[cfg(feature = "automation")]
                &self.trace,
            );
        }
    }

    fn on_exit(&mut self) {
        lock(&self.workspace).shutdown();
    }
}
