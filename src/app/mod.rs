//! Native host: one winit event loop owns every Workspace window and the settings window.
//! They share one egui Context and one wgpu painter; each native window runs its pass under
//! its own viewport ID, so there is no hidden host window or surface.
//!
//! One batch draws every Workspace window in turn on the frame-rate grid: deferred
//! per-window repaints would starve unfocused windows, because Windows keeps picking the
//! focused window's pending WM_PAINT.

#[cfg(windows)]
mod fonts;
#[cfg(windows)]
pub(super) mod instance;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    num::{NonZeroU32, NonZeroUsize},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, TryLockError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use egui::{ViewportBuilder, ViewportEvent, ViewportId, ViewportIdSet, ViewportInfo};
use egui_wgpu::{WgpuConfiguration, WgpuSetup, wgpu, winit::Painter};
use egui_winit::{
    ActionRequested,
    winit::{
        application::ApplicationHandler,
        event::WindowEvent,
        event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy},
        window::{Window, WindowId as NativeId},
    },
};

use crate::workspace::{FramePacer, PassRequest, WindowId, Workspace, WorkspaceAction};

#[cfg(feature = "automation")]
type SharedTrace = Option<Arc<Mutex<crate::automation::Trace>>>;

pub(crate) struct App {
    workspace: Arc<Mutex<Workspace>>,
    started_at: Instant,
    pacer: Arc<FramePacer>,
    wake_pending: Arc<AtomicBool>,
    /// Whether any Workspace window drew in the last batch.
    any_window_shown: Arc<AtomicBool>,
    #[cfg(feature = "automation")]
    trace: SharedTrace,
    #[cfg(feature = "perf-trace")]
    perf: Option<crate::diagnostics::perf::PerfTrace>,
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
            #[cfg(feature = "perf-trace")]
            perf: None,
        }
    }

    #[cfg(feature = "perf-trace")]
    pub(crate) fn with_perf(mut self, perf: crate::diagnostics::perf::PerfTrace) -> Self {
        self.perf = Some(perf);
        self
    }

    #[cfg(feature = "automation")]
    pub(crate) fn with_trace(mut self, trace: crate::automation::Trace) -> Self {
        self.trace = Some(Arc::new(Mutex::new(trace)));
        self
    }

    /// Coalesces data wake-ups to one per batch, paced by the frame-rate limit; none
    /// while all Workspace windows are minimized. Endpoint changes always wake the host.
    fn install_waker(&self, proxy: &EventLoopProxy<AppEvent>) {
        let data = Mutex::new(proxy.clone());
        let endpoint = Mutex::new(proxy.clone());
        let (pending, shown, pacer) = (
            Arc::clone(&self.wake_pending),
            Arc::clone(&self.any_window_shown),
            Arc::clone(&self.pacer),
        );
        let mut workspace = lock(&self.workspace);
        workspace.set_publication_waker(Some(Box::new(move || {
            if shown.load(Ordering::Relaxed) && !pending.swap(true, Ordering::AcqRel) {
                let at = pacer.next_pass_at(Instant::now(), Duration::ZERO);
                let _ = lock(&data).send_event(AppEvent::Data(at));
            }
        })));
        workspace.set_endpoint_waker(Some(Arc::new(move || {
            let _ = lock(&endpoint).send_event(AppEvent::Endpoint);
        })));
    }

    #[cfg(windows)]
    fn save_on_session_end(&self) {
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

    fn on_exit(&mut self) {
        let mut workspace = lock(&self.workspace);
        workspace.autosave();
        workspace.set_publication_waker(None);
        workspace.set_endpoint_waker(None);
        workspace.shutdown();
        #[cfg(feature = "perf-trace")]
        if let Some(perf) = &self.perf {
            match perf.write() {
                Ok(path) => eprintln!("perf trace: {}", path.display()),
                Err(error) => eprintln!("perf trace not written: {error}"),
            }
        }
    }
}

/// Runs the event loop until the last Workspace window closes.
pub(crate) fn run(app: App) -> Result<(), String> {
    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .map_err(|error| error.to_string())?;
    let proxy = event_loop.create_proxy();
    let ctx = egui::Context::default();
    // Every viewport is a native window of its own.
    ctx.set_embed_viewports(false);
    // Allow egui's discard-and-rerun pass (`Context::request_discard`).
    ctx.options_mut(|options| options.max_passes = NonZeroUsize::MIN.saturating_add(1));
    let repaint_proxy = Mutex::new(proxy.clone());
    ctx.set_request_repaint_callback(move |info| {
        let _ = lock(&repaint_proxy).send_event(AppEvent::Repaint {
            viewport: info.viewport_id,
            at: Instant::now() + info.delay,
            pass: info.current_cumulative_pass_nr,
        });
    });
    #[cfg(windows)]
    fonts::install_fallbacks(&ctx);
    app.install_waker(&proxy);
    #[cfg(windows)]
    app.save_on_session_end();
    let now = Instant::now();
    let mut host = Host {
        app,
        ctx,
        painter: None,
        textures: egui::TexturesDelta::default(),
        windows: BTreeMap::new(),
        by_native: HashMap::new(),
        failed: BTreeSet::new(),
        windows_at: Some(now),
        settings_at: Some(now),
        error: None,
        buttons_down: 0,
    };
    event_loop
        .run_app(&mut host)
        .map_err(|error| error.to_string())?;
    host.error.map_or(Ok(()), Err)
}

fn wgpu_configuration() -> WgpuConfiguration {
    let mut configuration = WgpuConfiguration::default();
    if let WgpuSetup::CreateNew(setup) = &mut configuration.wgpu_setup {
        setup.instance_descriptor.backends = wgpu::Backends::DX12;
        setup.power_preference = wgpu::PowerPreference::LowPower;
    }
    configuration.desired_maximum_frame_latency = Some(1);
    configuration
}

#[derive(Debug)]
enum AppEvent {
    /// New analysis results: a paced batch of Workspace windows.
    Data(Instant),
    /// The audio endpoint changed: every window, now.
    Endpoint,
    /// egui asked to repaint `viewport`; `pass` is its pass count at the request.
    Repaint {
        viewport: ViewportId,
        at: Instant,
        pass: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Target {
    Workspace(WindowId),
    Settings,
}

struct NativeWindow {
    viewport: ViewportId,
    window: Arc<Window>,
    state: egui_winit::State,
    info: ViewportInfo,
    actions: Vec<ActionRequested>,
}

struct Host {
    app: App,
    ctx: egui::Context,
    /// Created with the first window: the adapter must suit a real surface.
    painter: Option<Painter>,
    /// Texture changes from a pass that painted nothing; applied with the next paint.
    textures: egui::TexturesDelta,
    windows: BTreeMap<Target, NativeWindow>,
    by_native: HashMap<NativeId, Target>,
    /// Viewports whose native window could not be created; not retried every batch.
    failed: BTreeSet<ViewportId>,
    /// When the next batch of Workspace windows is due.
    windows_at: Option<Instant>,
    /// When the settings window's next pass is due.
    settings_at: Option<Instant>,
    error: Option<String>,
    /// Mouse buttons held over Workspace windows; motion then is a drag.
    buttons_down: u32,
}

fn earliest(current: &mut Option<Instant>, at: Instant) {
    *current = Some(current.map_or(at, |current| current.min(at)));
}

impl Host {
    /// Runs whatever is due, then sleeps until the next deadline.
    fn service(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let windows = self.windows_at.is_some_and(|at| at <= now);
        let settings = self.settings_at.is_some_and(|at| at <= now);
        if windows || settings {
            self.batch(event_loop, windows, settings);
        }
        let next = [self.windows_at, self.settings_at]
            .into_iter()
            .flatten()
            .min();
        event_loop.set_control_flow(next.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn batch(&mut self, event_loop: &ActiveEventLoop, windows: bool, settings: bool) {
        if windows {
            self.windows_at = None;
        }
        if settings {
            self.settings_at = None;
        }
        #[cfg(feature = "perf-trace")]
        let perf_started = crate::diagnostics::perf::now();
        if windows {
            // Before clearing the wake flag: a wake-up in between must see this batch's start.
            self.app.pacer.begin_pass(Instant::now());
        }
        {
            let mut workspace = lock(&self.app.workspace);
            workspace.tick(self.app.started_at.elapsed());
            if workspace.windows().is_empty() {
                event_loop.exit();
                return;
            }
            if let Some(delay) = workspace.autosave_when_settled() {
                workspace.request_pass(delay, false);
            }
            if let Some(delay) = workspace.endpoint_retry_delay() {
                workspace.request_pass(delay, true);
            }
        }
        #[cfg(feature = "perf-trace")]
        if let Some(perf) = &mut self.app.perf {
            perf.logic(perf_started);
        }
        if let Err(error) = self.reconcile(event_loop) {
            self.error = Some(error);
            event_loop.exit();
            return;
        }
        #[cfg(feature = "perf-trace")]
        let perf_pass = crate::diagnostics::perf::UiPass::begin();
        if windows {
            self.app.wake_pending.store(false, Ordering::Release);
        }
        let targets: Vec<Target> = self
            .windows
            .keys()
            .copied()
            .filter(|target| match target {
                Target::Workspace(_) => windows,
                Target::Settings => settings,
            })
            .collect();
        let mut shown = false;
        for target in targets {
            shown |= self.draw(
                target,
                #[cfg(feature = "perf-trace")]
                &perf_pass,
            );
        }
        if windows {
            self.app.any_window_shown.store(shown, Ordering::Release);
        }
        #[cfg(feature = "perf-trace")]
        if let Some(perf) = &mut self.app.perf {
            let ended = crate::diagnostics::perf::now();
            perf.end(perf_pass, ended);
        }
        let (request, closed) = {
            let mut workspace = lock(&self.app.workspace);
            (
                workspace.take_pass_request(),
                workspace.windows().is_empty(),
            )
        };
        if closed {
            event_loop.exit();
        }
        self.schedule(request);
    }

    fn schedule(&mut self, request: PassRequest) {
        if let Some(at) = request.at {
            earliest(&mut self.windows_at, at);
            if request.settings {
                earliest(&mut self.settings_at, at);
            }
        }
    }

    /// Creates native windows the Workspace gained and drops the ones it lost.
    fn reconcile(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let (wanted, settings) = {
            let workspace = lock(&self.app.workspace);
            (
                workspace.windows(),
                workspace
                    .settings_viewport_id()
                    .zip(workspace.settings_builder()),
            )
        };
        let keep: BTreeMap<Target, ViewportId> = wanted
            .iter()
            .map(|summary| (Target::Workspace(summary.id), summary.id.viewport_id()))
            .chain(settings.as_ref().map(|(id, _)| (Target::Settings, *id)))
            .collect();
        let by_native = &mut self.by_native;
        let before = self.windows.len();
        self.windows.retain(|target, native| {
            let kept = keep.get(target) == Some(&native.viewport);
            if !kept {
                by_native.remove(&native.window.id());
            }
            kept
        });
        if self.windows.len() != before {
            let live: Vec<_> = wanted
                .iter()
                .map(|summary| (summary.id.viewport_id(), summary.builder.clone()))
                .chain(settings.clone())
                .collect();
            self.release_closed_viewports(&live);
        }
        if let Some(painter) = &mut self.painter {
            let active: ViewportIdSet = self
                .windows
                .values()
                .map(|native| native.viewport)
                .collect();
            painter.gc_viewports(&active);
        }
        for summary in &wanted {
            self.open(
                event_loop,
                Target::Workspace(summary.id),
                summary.id.viewport_id(),
                &summary.builder,
            )?;
        }
        if let Some((viewport, builder)) = &settings {
            self.open(event_loop, Target::Settings, *viewport, builder)?;
        }
        if self.windows.is_empty() {
            return Err("no window could be opened".to_owned());
        }
        Ok(())
    }

    /// egui forgets a viewport (its memory and input) only at the end of a ROOT pass that did
    /// not show it. The windows never run a ROOT pass, so one runs here that shows just the
    /// live viewports. Its `max_texture_side` must match the windows': a different value
    /// recreates the shared fonts.
    fn release_closed_viewports(&mut self, live: &[(ViewportId, ViewportBuilder)]) {
        let input = egui::RawInput {
            max_texture_side: self.painter.as_ref().and_then(Painter::max_texture_side),
            ..Default::default()
        };
        let output = self.ctx.run_ui(input, |ui| {
            for (viewport, builder) in live {
                ui.ctx()
                    .show_viewport_deferred(*viewport, builder.clone(), |_, _| {});
            }
        });
        self.textures.append(output.textures_delta);
    }

    fn open(
        &mut self,
        event_loop: &ActiveEventLoop,
        target: Target,
        viewport: ViewportId,
        builder: &ViewportBuilder,
    ) -> Result<(), String> {
        if self.windows.contains_key(&target) || self.failed.contains(&viewport) {
            return Ok(());
        }
        let window = match egui_winit::create_window(&self.ctx, event_loop, builder) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                eprintln!("couldn't create a window: {error}");
                self.failed.insert(viewport);
                return Ok(());
            }
        };
        let painter = match &mut self.painter {
            Some(painter) => painter,
            None => self.painter.insert(pollster::block_on(Painter::new(
                self.ctx.clone(),
                wgpu_configuration(),
                false,
                egui_wgpu::RendererOptions::default(),
            ))),
        };
        pollster::block_on(painter.set_window(viewport, Some(Arc::clone(&window))))
            .map_err(|error| format!("couldn't render to a window: {error}"))?;
        let state = egui_winit::State::new(
            self.ctx.clone(),
            viewport,
            event_loop,
            Some(window.scale_factor() as f32),
            event_loop.system_theme(),
            painter.max_texture_side(),
        );
        let mut info = ViewportInfo::default();
        egui_winit::update_viewport_info(&mut info, &self.ctx, &window, true);
        self.by_native.insert(window.id(), target);
        self.windows.insert(
            target,
            NativeWindow {
                viewport,
                window,
                state,
                info,
                actions: Vec::new(),
            },
        );
        Ok(())
    }

    /// One window's egui pass and paint; returns whether a Workspace window drew.
    fn draw(
        &mut self,
        target: Target,
        #[cfg(feature = "perf-trace")] perf_pass: &crate::diagnostics::perf::UiPass,
    ) -> bool {
        let Self {
            app,
            ctx,
            painter,
            textures,
            windows,
            ..
        } = self;
        let (Some(native), Some(painter)) = (windows.get_mut(&target), painter.as_mut()) else {
            return false;
        };
        // D9: only the pass body counts as UI wall; input, egui's run and painting do not.
        #[cfg(feature = "perf-trace")]
        let (call_started, body_before) =
            (crate::diagnostics::perf::now(), perf_pass.body_so_far());
        egui_winit::update_viewport_info(&mut native.info, ctx, &native.window, false);
        let mut input = native.state.take_egui_input(&native.window);
        input.time = Some(app.started_at.elapsed().as_secs_f64());
        input.viewports = std::iter::once((native.viewport, native.info.clone())).collect();
        native.info.events.clear();
        let mut drawn = false;
        let output = ctx.run_ui(input, |ui| {
            #[cfg(feature = "perf-trace")]
            let body_started = crate::diagnostics::perf::now();
            drawn = match target {
                Target::Workspace(window_id) => window_pass(
                    &app.workspace,
                    window_id,
                    ui,
                    app.started_at,
                    #[cfg(feature = "automation")]
                    &app.trace,
                ),
                Target::Settings => {
                    settings_pass(
                        &app.workspace,
                        ui,
                        app.started_at,
                        #[cfg(feature = "automation")]
                        &app.trace,
                    );
                    false
                }
            };
            #[cfg(feature = "perf-trace")]
            perf_pass.body(body_started, drawn, ui);
        });
        native
            .state
            .handle_platform_output(&native.window, output.platform_output);
        for (viewport, viewport_output) in output.viewport_output {
            if viewport == native.viewport {
                egui_winit::process_viewport_commands(
                    ctx,
                    &mut native.info,
                    viewport_output.commands,
                    &native.window,
                    &mut native.actions,
                );
            }
        }
        for action in native.actions.drain(..) {
            let event = match action {
                ActionRequested::Cut => Some(egui::Event::Cut),
                ActionRequested::Copy => Some(egui::Event::Copy),
                ActionRequested::Paste => native
                    .state
                    .clipboard_text()
                    .map(|text| text.replace("\r\n", "\n"))
                    .filter(|text| !text.is_empty())
                    .map(egui::Event::Paste),
                ActionRequested::Screenshot(_) => None,
            };
            native.state.egui_input_mut().events.extend(event);
        }
        textures.append(output.textures_delta);
        // A minimized window shows nothing, but texture changes must still reach the
        // shared renderer.
        if native.info.minimized != Some(true) || !textures.is_empty() {
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            painter.paint_and_update_textures(
                native.viewport,
                output.pixels_per_point,
                [0.0; 4],
                &primitives,
                &std::mem::take(textures),
                Vec::new(),
            );
        }
        #[cfg(feature = "perf-trace")]
        perf_pass.viewport(call_started, body_before);
        drawn
    }

    /// Input wakes its own window's kind, and the settings window when `settings`.
    fn wake(&mut self, target: Target, settings: bool) {
        let now = Instant::now();
        if settings {
            earliest(&mut self.settings_at, now);
        }
        if let Target::Workspace(_) = target {
            earliest(&mut self.windows_at, now);
        }
    }
}

impl ApplicationHandler<AppEvent> for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.service(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        native_id: NativeId,
        event: WindowEvent,
    ) {
        let Some(&target) = self.by_native.get(&native_id) else {
            return;
        };
        let Some(native) = self.windows.get_mut(&target) else {
            return;
        };
        let mut redraw_now = false;
        match &event {
            WindowEvent::Resized(size) => {
                // Windows reports zero size while minimized.
                if let (Some(width), Some(height), Some(painter)) = (
                    NonZeroU32::new(size.width),
                    NonZeroU32::new(size.height),
                    self.painter.as_mut(),
                ) {
                    painter.on_window_resized(native.viewport, width, height);
                    redraw_now = true;
                }
            }
            WindowEvent::CloseRequested => native.info.events.push(ViewportEvent::Close),
            WindowEvent::Occluded(occluded) => native.info.occluded = Some(*occluded),
            WindowEvent::MouseInput { state, .. } => {
                self.buttons_down = if state.is_pressed() {
                    self.buttons_down + 1
                } else {
                    self.buttons_down.saturating_sub(1)
                };
            }
            WindowEvent::Focused(false) => self.buttons_down = 0,
            _ => {}
        }
        let response = native.state.on_window_event(&native.window, &event);
        let wants_pass = response.repaint
            || matches!(
                event,
                WindowEvent::CloseRequested | WindowEvent::RedrawRequested
            );
        if wants_pass || redraw_now {
            // Workspace input wakes settings too (a click retargets it, the wheel changes a
            // range), but plain pointer motion changes nothing it shows and would rebuild it
            // at the mouse rate. Should settings ever show hover state, the Workspace asks for
            // a settings pass when that state changes (`request_pass_now(true)`).
            let motion = matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::CursorEntered { .. }
                    | WindowEvent::CursorLeft { .. }
            );
            let settings =
                !matches!(target, Target::Workspace(_)) || !motion || self.buttons_down > 0;
            self.wake(target, settings);
        }
        if redraw_now {
            // Draw at the new size before returning, or DWM stretches the old frame.
            let workspace = matches!(target, Target::Workspace(_));
            self.batch(event_loop, workspace, !workspace);
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::Data(at) => earliest(&mut self.windows_at, at),
            AppEvent::Endpoint => {
                let now = Instant::now();
                earliest(&mut self.windows_at, now);
                earliest(&mut self.settings_at, now);
            }
            AppEvent::Repaint { viewport, at, pass } => {
                // Already repainted since the request.
                let current = self.ctx.cumulative_pass_nr_for(viewport);
                if current != pass && current != pass + 1 {
                    return;
                }
                let settings = self
                    .windows
                    .get(&Target::Settings)
                    .is_some_and(|native| native.viewport == viewport);
                if settings {
                    earliest(&mut self.settings_at, at);
                } else {
                    earliest(&mut self.windows_at, at);
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.service(event_loop);
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.app.on_exit();
        self.windows.clear();
        self.by_native.clear();
        if let Some(painter) = &mut self.painter {
            painter.destroy();
        }
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
        // The host drops the native window between passes.
        workspace.request_pass_now(true);
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

fn settings_pass(
    workspace: &Mutex<Workspace>,
    ui: &mut egui::Ui,
    started_at: Instant,
    #[cfg(feature = "automation")] trace: &SharedTrace,
) {
    let mut workspace = lock(workspace);
    if ui.ctx().input(|input| input.viewport().close_requested()) {
        let _ = workspace.apply_action(WorkspaceAction::CloseSettings);
        workspace.request_pass_now(true);
    } else {
        workspace.settings_ui(ui, started_at.elapsed());
    }
    // Recorded here too: with every Workspace window minimized, no other pass would
    // report the settings window opening or closing.
    #[cfg(feature = "automation")]
    if let Some(trace) = trace {
        let mut trace = lock(trace);
        if workspace.settings_open() {
            trace.settings_targets(&workspace);
        }
        trace.state(&workspace);
    }
}
