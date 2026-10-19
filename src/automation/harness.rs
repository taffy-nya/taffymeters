//! Windowless driver for the real Workspace UI. Every Workspace window gets its own
//! egui Context, as each native viewport would; viewport commands the UI emits are
//! applied to a simulated desktop, never to the OS.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use eframe::egui::{self, Event, PointerButton, Pos2, Rect, Vec2, ViewportCommand, pos2, vec2};

use crate::workspace::{
    ControlKey, ControlTarget, DesktopProbe, Display, FIRST_WINDOW, INITIAL_WINDOW_SIZE,
    MoveRequest, SettingsItem, WindowId, WindowTracker, Workspace,
};

/// The settings window in the harness's window map. It is not a Workspace window;
/// the ID only addresses its simulated viewport.
pub(super) const SETTINGS: WindowId = WindowId(u64::MAX);
/// Tall enough that every tab's controls are visible without scrolling.
const SETTINGS_SIM_SIZE: Vec2 = vec2(460.0, 1600.0);

/// Simulated desktop facts for the snapping logic and window placement. Shared
/// with the Workspace, which owns its desktop probe. Window moves run the same
/// [`WindowTracker`] the native layer runs; the harness applies the moves to its
/// simulated windows after each pass, as the OS would within the call.
pub(super) struct FakeDesktop {
    button_down: AtomicBool,
    displays: Mutex<Vec<Display>>,
    native: Mutex<FakeNative>,
}

#[derive(Default)]
struct FakeNative {
    tracker: WindowTracker,
    /// Moves to apply to the simulated windows.
    pending: Vec<(WindowId, Pos2)>,
    /// Every `place` so far.
    placed: Vec<(WindowId, Pos2)>,
    moves_started: usize,
}

impl FakeDesktop {
    /// One 1920×1080 monitor at 100% with a 40 px taskbar at the bottom.
    pub fn new() -> Arc<Self> {
        Self::with_displays(vec![Display {
            work: Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1040.0)),
            scale: 1.0,
            primary: true,
        }])
    }

    pub fn with_displays(displays: Vec<Display>) -> Arc<Self> {
        Arc::new(Self {
            button_down: AtomicBool::new(false),
            displays: Mutex::new(displays),
            native: Mutex::default(),
        })
    }

    pub fn set_button_down(&self, down: bool) {
        self.button_down.store(down, Ordering::Relaxed);
    }

    fn native(&self) -> MutexGuard<'_, FakeNative> {
        self.native.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Body drags that started a move so far.
    pub fn moves_started(&self) -> usize {
        self.native().moves_started
    }

    /// Take the positions `place` set so far.
    pub fn take_placed(&self) -> Vec<(WindowId, Pos2)> {
        std::mem::take(&mut self.native().placed)
    }

    /// The pointer of a move in progress is at `cursor`; `free`: Alt is held.
    fn pointer(&self, cursor: Pos2, free: bool) {
        let mut native = self.native();
        let FakeNative {
            tracker, pending, ..
        } = &mut *native;
        tracker.motion(cursor, free, pending);
    }

    fn escape(&self) {
        let mut native = self.native();
        let FakeNative {
            tracker, pending, ..
        } = &mut *native;
        tracker.cancel(pending);
    }

    fn release(&self) {
        self.native().tracker.end();
    }

    /// A window's frame is now `frame`; `ours` when this desktop put it there.
    fn observed(&self, window: WindowId, frame: Rect, ours: bool) {
        let mut native = self.native();
        let FakeNative {
            tracker, pending, ..
        } = &mut *native;
        if tracker.frame(window) != Some(frame) {
            tracker.observed(window, frame, ours, Pos2::ZERO, pending);
        }
    }

    fn forget(&self, window: WindowId) {
        self.native().tracker.forget(window);
    }

    fn take_pending(&self) -> Vec<(WindowId, Pos2)> {
        std::mem::take(&mut self.native().pending)
    }
}

impl DesktopProbe for FakeDesktop {
    fn primary_button_down(&self) -> bool {
        self.button_down.load(Ordering::Relaxed)
    }

    fn work_area(&self, point: Pos2) -> Option<Rect> {
        let displays = self.displays();
        crate::workspace::display_at(&displays, point).map(|index| displays[index].work)
    }

    fn displays(&self) -> Vec<Display> {
        self.displays
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn attach(&self, window: WindowId) -> bool {
        self.native().tracker.frame(window).is_some()
    }

    fn set_groups(&self, groups: &[(WindowId, u64)]) {
        self.native().tracker.set_groups(groups);
    }

    fn frame(&self, window: WindowId) -> Option<Rect> {
        self.native().tracker.frame(window)
    }

    fn begin_move(&self, request: MoveRequest) -> bool {
        let work_areas = self.displays().iter().map(|display| display.work).collect();
        let mut native = self.native();
        native.moves_started += 1;
        native.tracker.begin(&request, work_areas)
    }

    fn moving(&self) -> bool {
        self.native().tracker.moving()
    }

    fn place(&self, moves: &[(WindowId, Pos2)]) -> bool {
        let mut native = self.native();
        if moves
            .iter()
            .any(|(id, _)| native.tracker.frame(*id).is_none())
        {
            return false;
        }
        native.pending.extend_from_slice(moves);
        native.placed.extend_from_slice(moves);
        true
    }
}

/// One simulated native window.
pub(super) struct SimWindow {
    pub context: egui::Context,
    /// Logical inner size (frameless: equals the outer size).
    pub size: Vec2,
    pub ppp: f32,
    /// Outer top-left in physical desktop pixels.
    pub position: Pos2,
    pub maximized: bool,
    /// Reported to the window's passes as the viewport's minimized state.
    pub minimized: bool,
    /// Last `MinInnerSize` received; the simulated OS keeps the size above it.
    pub min_size: Vec2,
    /// Every viewport command emitted since the last `take_commands`.
    pub commands: Vec<ViewportCommand>,
    /// Cursor the last pass asked for.
    pub cursor: egui::CursorIcon,
    /// Every text the passes put on the clipboard.
    pub copied: Vec<String>,
    targets: HashMap<ControlKey, ControlTarget>,
}

impl SimWindow {
    fn new(size: Vec2, ppp: f32, position: Pos2) -> Self {
        let context = egui::Context::default();
        context.set_visuals(egui::Visuals::dark());
        Self {
            context,
            size,
            ppp,
            position,
            maximized: false,
            minimized: false,
            min_size: Vec2::ZERO,
            commands: Vec::new(),
            cursor: egui::CursorIcon::Default,
            copied: Vec::new(),
            targets: HashMap::new(),
        }
    }
}

pub(super) struct Harness {
    pub ws: Workspace,
    pub windows: BTreeMap<WindowId, SimWindow>,
    pub desktop: Arc<FakeDesktop>,
    pub display_time: Duration,
    pub frame: u64,
    /// The window receiving input from the single-window helpers.
    pub focus: WindowId,
    /// Windows whose native creation is simulated to fail.
    pub blocked: HashSet<WindowId>,
    /// Scale of windows created later.
    pub new_window_ppp: f32,
    /// Modifier state reported in every frame's RawInput.
    pub modifiers: egui::Modifiers,
    /// Where `grab` holds the pointer in the focused window (logical).
    grabbed: Option<Pos2>,
}

impl Harness {
    /// Drive `ws` on a fresh simulated desktop (one 1920×1080 monitor).
    pub fn new(ws: Workspace) -> Self {
        Self::with_desktop(ws, FakeDesktop::new())
    }

    /// Drive `ws` on `desktop`, which the Workspace then uses too. Windows open at
    /// the scale of its primary display.
    pub fn with_desktop(mut ws: Workspace, desktop: Arc<FakeDesktop>) -> Self {
        ws.set_desktop(Arc::clone(&desktop) as Arc<dyn DesktopProbe + Send + Sync>);
        let new_window_ppp = desktop
            .displays()
            .iter()
            .find(|display| display.primary)
            .map_or(1.0, |display| display.scale);
        let mut harness = Self {
            ws,
            windows: BTreeMap::new(),
            desktop,
            display_time: Duration::from_secs(1),
            frame: 0,
            focus: FIRST_WINDOW,
            blocked: HashSet::new(),
            new_window_ppp,
            modifiers: egui::Modifiers::NONE,
            grabbed: None,
        };
        // The initial default window opens at a fixed spot; restored windows open
        // where their builder says.
        harness.sync_windows();
        if let Some(window) = harness.windows.get_mut(&FIRST_WINDOW)
            && harness
                .ws
                .windows()
                .iter()
                .any(|summary| summary.id == FIRST_WINDOW && summary.builder.position.is_none())
        {
            window.position = pos2(100.0, 100.0);
        }
        if let Some(first) = harness.windows.keys().next() {
            harness.focus = *first;
        }
        harness
    }

    pub fn window(&self, id: WindowId) -> Result<&SimWindow, String> {
        self.windows
            .get(&id)
            .ok_or_else(|| format!("window {} is not open", id.0))
    }

    pub fn window_mut(&mut self, id: WindowId) -> Result<&mut SimWindow, String> {
        self.windows
            .get_mut(&id)
            .ok_or_else(|| format!("window {} is not open", id.0))
    }

    pub fn context(&self) -> egui::Context {
        self.windows[&self.focus].context.clone()
    }

    /// Size of the focused window (logical points).
    pub fn set_size(&mut self, size: Vec2) {
        if let Some(window) = self.windows.get_mut(&self.focus) {
            window.size = size.max(window.min_size);
        }
    }

    /// Mirror the Workspace's window set like the native adapter: open new windows
    /// (unless blocked) and forget closed ones.
    fn sync_windows(&mut self) {
        let live: Vec<_> = self.ws.windows();
        let settings_open = self.ws.settings_open();
        let desktop = Arc::clone(&self.desktop);
        self.windows.retain(|id, _| {
            let keep =
                (*id == SETTINGS && settings_open) || live.iter().any(|window| window.id == *id);
            if !keep {
                desktop.forget(*id);
            }
            keep
        });
        for summary in live {
            if self.windows.contains_key(&summary.id) || self.blocked.contains(&summary.id) {
                continue;
            }
            let ppp = self.new_window_ppp;
            let size = summary.builder.inner_size.unwrap_or(INITIAL_WINDOW_SIZE);
            let position = summary
                .builder
                .position
                .map_or(pos2(200.0, 200.0), |position| position * ppp);
            self.windows
                .insert(summary.id, SimWindow::new(size, ppp, position));
        }
        if settings_open && !self.windows.contains_key(&SETTINGS) {
            let ppp = self.new_window_ppp;
            let position = self
                .ws
                .settings_builder()
                .and_then(|builder| builder.position)
                .map_or(pos2(1000.0, 100.0), |position| position * ppp);
            self.windows
                .insert(SETTINGS, SimWindow::new(SETTINGS_SIM_SIZE, ppp, position));
        }
    }

    /// Workspace windows only (the settings window excluded).
    pub fn workspace_windows(&self) -> impl Iterator<Item = (&WindowId, &SimWindow)> {
        self.windows.iter().filter(|(id, _)| **id != SETTINGS)
    }

    /// Outer frame of a simulated window, physical pixels.
    fn frame_of(window: &SimWindow) -> Rect {
        Rect::from_min_size(window.position, window.size * window.ppp)
    }

    /// Tell the fake desktop where every Workspace window is (as the native layer
    /// sees every frame change), then apply the moves it made in response.
    fn sync_desktop(&mut self) {
        for (id, window) in self.windows.iter().filter(|(id, _)| **id != SETTINGS) {
            self.desktop.observed(*id, Self::frame_of(window), true);
        }
        self.apply_moves();
    }

    /// Apply the moves the fake desktop made (placements, a move in progress, a
    /// group following), as the OS would within the call.
    fn apply_moves(&mut self) {
        loop {
            let moves = self.desktop.take_pending();
            if moves.is_empty() {
                return;
            }
            for (id, position) in moves {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.position = position;
                    self.desktop.observed(id, Self::frame_of(window), true);
                }
            }
        }
    }

    /// Another program (or the OS) moves a window to `position`: its group
    /// follows.
    pub fn external_move(&mut self, id: WindowId, position: Pos2) -> Result<(), String> {
        let window = self.window_mut(id)?;
        window.position = position;
        let frame = Self::frame_of(window);
        self.desktop.observed(id, frame, false);
        self.apply_moves();
        Ok(())
    }

    /// Press on a body control of the focused window and drag it past egui's
    /// threshold: the pass that sees the drag asks the desktop to move the window
    /// (and its group) with the pointer. Returns the pointer on the desktop
    /// (physical) at the press; `move_pointer` continues from there.
    pub fn grab(&mut self, key: ControlKey) -> Result<Pos2, String> {
        let target = self.locate(&key)?;
        let start = self.hover(&key, target)?;
        self.desktop.set_button_down(true);
        let none = egui::Modifiers::NONE;
        self.frame(vec![Self::button(
            start,
            PointerButton::Primary,
            true,
            none,
        )]);
        self.frame(vec![Event::PointerMoved(start + vec2(10.0, 0.0))]);
        self.grabbed = Some(start + vec2(10.0, 0.0));
        let window = self.window(self.focus)?;
        Ok(window.position + start.to_vec2() * window.ppp)
    }

    /// The pointer of the move in progress is at `cursor` (desktop, physical);
    /// `alt`: Alt is held. Every window then runs a pass.
    pub fn move_pointer(&mut self, cursor: Pos2, alt: bool) -> Result<(), String> {
        self.desktop.pointer(cursor, alt);
        self.apply_moves();
        self.desktop.native().tracker.settle(cursor);
        self.frame_all()
    }

    /// Escape during the move: the windows go back and the move ends.
    pub fn cancel_move(&mut self) -> Result<(), String> {
        self.desktop.escape();
        self.apply_moves();
        self.frame_all()
    }

    /// Release the button that `grab` pressed.
    pub fn release(&mut self) -> Result<(), String> {
        self.desktop.release();
        self.desktop.set_button_down(false);
        let at = self.grabbed.take().unwrap_or_default();
        self.frame(vec![Self::button(
            at,
            PointerButton::Primary,
            false,
            egui::Modifiers::NONE,
        )]);
        self.frame_all()
    }

    /// Run one UI pass of `id` with `events`.
    pub fn frame_window(
        &mut self,
        id: WindowId,
        events: Vec<Event>,
    ) -> Result<egui::FullOutput, String> {
        self.frame += 1;
        self.sync_desktop();
        let display_time = self.display_time;
        let window = self
            .windows
            .get_mut(&id)
            .ok_or_else(|| format!("window {} is not open", id.0))?;
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, window.size)),
            time: Some(display_time.as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            focused: true,
            events,
            modifiers: self.modifiers,
            ..Default::default()
        };
        if let Some(root) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
            root.native_pixels_per_point = Some(window.ppp);
            let outer = Rect::from_min_size(window.position / window.ppp, window.size);
            root.outer_rect = Some(outer);
            root.inner_rect = Some(outer);
            root.maximized = Some(window.maximized);
            root.minimized = Some(window.minimized);
            root.focused = Some(true);
        }
        let context = window.context.clone();
        let ws = &mut self.ws;
        let output = context.run_ui(input, |ui| {
            if id == SETTINGS {
                ws.settings_ui(ui, display_time);
            } else {
                ws.window_ui(id, ui, display_time);
            }
        });
        let targets = if id == SETTINGS {
            self.ws.settings_targets()
        } else {
            self.ws.control_targets(id)
        };
        if let Some(window) = self.windows.get_mut(&id) {
            window.targets = targets;
            window.cursor = output.platform_output.cursor_icon;
            window.copied.extend(
                output
                    .platform_output
                    .commands
                    .iter()
                    .filter_map(|command| match command {
                        egui::OutputCommand::CopyText(text) => Some(text.clone()),
                        _ => None,
                    }),
            );
            for viewport in output.viewport_output.values() {
                for command in &viewport.commands {
                    match command {
                        ViewportCommand::OuterPosition(position) => {
                            window.position = *position * window.ppp;
                        }
                        ViewportCommand::MinInnerSize(size) => {
                            window.min_size = *size;
                            window.size = window.size.max(*size);
                        }
                        ViewportCommand::Maximized(maximized) => window.maximized = *maximized,
                        _ => {}
                    }
                    window.commands.push(command.clone());
                }
            }
        }
        self.sync_windows();
        self.apply_moves();
        Ok(output)
    }

    /// One pass of the focused window. A closed focus window runs nothing; its
    /// controls then report Missing.
    pub fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        self.frame_window(self.focus, events).unwrap_or_default()
    }

    /// One pass of every open window, including windows opened during this call
    /// (new windows show their first frame).
    pub fn frame_all(&mut self) -> Result<(), String> {
        let mut done = HashSet::new();
        loop {
            let pending: Vec<WindowId> = self
                .windows
                .keys()
                .copied()
                .filter(|id| !done.contains(id))
                .collect();
            if pending.is_empty() {
                return Ok(());
            }
            for id in pending {
                done.insert(id);
                if self.windows.contains_key(&id) {
                    self.frame_window(id, vec![])?;
                }
            }
        }
    }

    pub fn take_commands(&mut self, id: WindowId) -> Vec<ViewportCommand> {
        self.windows
            .get_mut(&id)
            .map(|window| std::mem::take(&mut window.commands))
            .unwrap_or_default()
    }

    pub fn locate(&self, key: &ControlKey) -> Result<ControlTarget, String> {
        let target = self
            .windows
            .get(&self.focus)
            .and_then(|window| window.targets.get(key))
            .ok_or_else(|| format!("Missing control: {key:?}"))?;
        if !target.enabled {
            return Err(format!("Disabled control: {key:?}"));
        }
        Ok(*target)
    }

    /// Move the pointer to a point this control actually reports as hovered.
    fn hover(&mut self, key: &ControlKey, target: ControlTarget) -> Result<Pos2, String> {
        let current = self.locate(key)?;
        if current.frame_token != target.frame_token || current.pass_token != target.pass_token {
            return Err(format!("Stale control: {key:?}"));
        }
        let rect = target.rect;
        let size = rect.size();
        let points = [
            rect.center(),
            rect.min + size * 0.25,
            pos2(rect.max.x - size.x * 0.25, rect.min.y + size.y * 0.25),
            pos2(rect.min.x + size.x * 0.25, rect.max.y - size.y * 0.25),
            rect.max - size * 0.25,
        ];
        for point in points {
            self.frame(vec![Event::PointerMoved(point)]);
            let hover = self.locate(key)?;
            if hover.rect.contains(point) && hover.hovered {
                return Ok(point);
            }
        }
        Err(format!(
            "No hit-testable point in control: {key:?}, target={target:?}"
        ))
    }

    /// Rest the pointer on a control (it reports hovered), without clicking.
    pub fn point_at(&mut self, key: ControlKey) -> Result<Pos2, String> {
        let target = self.locate(&key)?;
        self.hover(&key, target)
    }

    fn button(
        pos: Pos2,
        button: PointerButton,
        pressed: bool,
        modifiers: egui::Modifiers,
    ) -> Event {
        Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        }
    }

    pub fn click(&mut self, key: ControlKey) -> Result<(), String> {
        let target = self.locate(&key)?;
        self.click_target(key, target)
    }

    pub fn click_target(&mut self, key: ControlKey, target: ControlTarget) -> Result<(), String> {
        self.click_with(key, target, PointerButton::Primary)
    }

    pub fn right_click(&mut self, key: ControlKey) -> Result<(), String> {
        let target = self.locate(&key)?;
        self.click_with(key, target, PointerButton::Secondary)
    }

    fn click_with(
        &mut self,
        key: ControlKey,
        target: ControlTarget,
        button: PointerButton,
    ) -> Result<(), String> {
        let pos = self.hover(&key, target)?;
        let none = egui::Modifiers::NONE;
        self.frame(vec![
            Event::PointerMoved(pos),
            Self::button(pos, button, true, none),
        ]);
        let pressed = self.locate(&key);
        if !pressed
            .as_ref()
            .is_ok_and(|pressed| pressed.rect.contains(pos))
        {
            self.frame(vec![Self::button(pos, button, false, none)]);
            return Err(format!(
                "Control moved between press and release: {key:?}, before={:?}, pressed={pressed:?}",
                target.rect
            ));
        }
        self.frame(vec![
            Event::PointerMoved(pos),
            Self::button(pos, button, false, none),
        ]);
        // Actions apply at the end of the release pass; settle so targets describe
        // the resulting layout.
        self.frame(vec![]);
        Ok(())
    }

    pub fn double_click(&mut self, key: ControlKey) -> Result<(), String> {
        let target = self.locate(&key)?;
        let pos = self.hover(&key, target)?;
        let none = egui::Modifiers::NONE;
        for _ in 0..2 {
            self.frame(vec![Self::button(pos, PointerButton::Primary, true, none)]);
            self.frame(vec![Self::button(pos, PointerButton::Primary, false, none)]);
        }
        Ok(())
    }

    /// Press on a control, move through `path` (one frame per point) and release at
    /// the last point. `escape_at` injects Escape after that many moves.
    pub fn drag(
        &mut self,
        key: ControlKey,
        path: &[Pos2],
        modifiers: egui::Modifiers,
        escape_at: Option<usize>,
    ) -> Result<Pos2, String> {
        let target = self.locate(&key)?;
        let start = self.hover(&key, target)?;
        let previous = std::mem::replace(&mut self.modifiers, modifiers);
        self.frame(vec![Self::button(
            start,
            PointerButton::Primary,
            true,
            modifiers,
        )]);
        let mut last = start;
        for (index, point) in path.iter().enumerate() {
            let mut events = vec![Event::PointerMoved(*point)];
            if escape_at == Some(index) {
                events.push(Self::key_event(egui::Key::Escape, true, modifiers));
            }
            self.frame(events);
            if escape_at == Some(index) {
                self.frame(vec![Self::key_event(egui::Key::Escape, false, modifiers)]);
            }
            last = *point;
        }
        self.frame(vec![Self::button(
            last,
            PointerButton::Primary,
            false,
            modifiers,
        )]);
        self.modifiers = previous;
        // A body drag's move ends with the button.
        self.desktop.release();
        self.frame(vec![]);
        Ok(start)
    }

    /// Right-click `body` in the focused window (opening the settings window on it),
    /// then click `items` in the settings window in order. Focus returns to the
    /// original window if it still exists; the settings window stays open.
    pub fn settings(&mut self, body: ControlKey, items: &[SettingsItem]) -> Result<(), String> {
        let origin = self.focus;
        self.right_click(body)?;
        if !self.windows.contains_key(&SETTINGS) {
            return Err(format!(
                "right click on {body:?} did not open the settings window"
            ));
        }
        let result = self.in_settings(items);
        if self.windows.contains_key(&origin) {
            self.focus = origin;
            self.frame(vec![]);
        }
        result
    }

    /// Click `items` in the open settings window; focus stays there.
    pub fn in_settings(&mut self, items: &[SettingsItem]) -> Result<(), String> {
        self.focus = SETTINGS;
        self.frame(vec![]);
        for item in items {
            self.click(ControlKey::Settings(*item))?;
            if !self.windows.contains_key(&SETTINGS) {
                break;
            }
        }
        Ok(())
    }

    pub fn dropdown(&mut self, opener: ControlKey, item: ControlKey) -> Result<(), String> {
        self.click(opener)?;
        self.frame(vec![]);
        self.click(item)?;
        self.frame(vec![]);
        Ok(())
    }

    fn key_event(key: egui::Key, pressed: bool, modifiers: egui::Modifiers) -> Event {
        Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    /// Press and release `key` with `modifiers` held (also in the frames' modifier
    /// state, which egui's shortcuts read).
    pub fn key_with(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        let previous = self.modifiers;
        self.modifiers = previous | modifiers;
        self.frame(vec![Self::key_event(key, true, modifiers)]);
        if self.windows.contains_key(&self.focus) {
            self.frame(vec![Self::key_event(key, false, modifiers)]);
        }
        self.modifiers = previous;
    }

    pub fn key(&mut self, key: egui::Key) {
        self.key_with(key, egui::Modifiers::NONE);
    }
}
