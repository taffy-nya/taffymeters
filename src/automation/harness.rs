//! Windowless driver for the real Workspace UI. Every Workspace window gets its own
//! egui Context, as each native viewport would; viewport commands the UI emits are
//! applied to a simulated desktop, never to the OS.

use std::{
    cell::Cell,
    collections::{BTreeMap, HashMap, HashSet},
    time::Duration,
};

use eframe::egui::{self, Event, PointerButton, Pos2, Rect, Vec2, ViewportCommand, pos2, vec2};

use crate::workspace::{
    ControlKey, ControlTarget, DesktopProbe, INITIAL_WINDOW_SIZE, MenuItem, ROOT_WINDOW, WindowId,
    Workspace,
};

/// Simulated desktop facts for the snapping logic.
pub(super) struct FakeDesktop {
    pub button_down: Cell<bool>,
    pub work_area: Cell<Option<Rect>>,
}

impl DesktopProbe for FakeDesktop {
    fn primary_button_down(&self) -> bool {
        self.button_down.get()
    }

    fn work_area(&self, _point: Pos2) -> Option<Rect> {
        self.work_area.get()
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
    /// Last `MinInnerSize` received; the simulated OS keeps the size above it.
    pub min_size: Vec2,
    /// Every viewport command emitted since the last `take_commands`.
    pub commands: Vec<ViewportCommand>,
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
            min_size: Vec2::ZERO,
            commands: Vec::new(),
            targets: HashMap::new(),
        }
    }
}

pub(super) struct Harness {
    pub ws: Workspace,
    pub windows: BTreeMap<WindowId, SimWindow>,
    pub desktop: FakeDesktop,
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
}

impl Harness {
    pub fn new(ws: Workspace) -> Self {
        let mut windows = BTreeMap::new();
        windows.insert(
            ROOT_WINDOW,
            SimWindow::new(INITIAL_WINDOW_SIZE, 1.0, pos2(100.0, 100.0)),
        );
        Self {
            ws,
            windows,
            desktop: FakeDesktop {
                button_down: Cell::new(false),
                work_area: Cell::new(Some(Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1040.0)))),
            },
            display_time: Duration::from_secs(1),
            frame: 0,
            focus: ROOT_WINDOW,
            blocked: HashSet::new(),
            new_window_ppp: 1.0,
            modifiers: egui::Modifiers::NONE,
        }
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
        self.windows
            .retain(|id, _| live.iter().any(|window| window.id == *id));
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
    }

    /// Run one UI pass of `id` with `events`.
    pub fn frame_window(
        &mut self,
        id: WindowId,
        events: Vec<Event>,
    ) -> Result<egui::FullOutput, String> {
        self.frame += 1;
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
            root.minimized = Some(false);
            root.focused = Some(true);
        }
        let context = window.context.clone();
        let ws = &mut self.ws;
        let desktop = &self.desktop;
        let output = context.run_ui(input, |ui| ws.window_ui(id, ui, display_time, desktop));
        let targets = self.ws.control_targets(id);
        if let Some(window) = self.windows.get_mut(&id) {
            window.targets = targets;
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
        self.frame(vec![]);
        Ok(start)
    }

    /// Open the context menu of `body` and click `items` in order (submenus open on
    /// hover, then the entry is located in the current pass).
    pub fn menu(&mut self, body: ControlKey, items: &[MenuItem]) -> Result<(), String> {
        self.right_click(body)?;
        self.frame(vec![]);
        for item in items {
            self.click(ControlKey::Menu(*item))?;
            self.frame(vec![]);
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

    pub fn key_with(&mut self, key: egui::Key, modifiers: egui::Modifiers) {
        self.frame(vec![Self::key_event(key, true, modifiers)]);
        if self.windows.contains_key(&self.focus) {
            self.frame(vec![Self::key_event(key, false, modifiers)]);
        }
    }

    pub fn key(&mut self, key: egui::Key) {
        self.key_with(key, egui::Modifiers::NONE);
    }
}
