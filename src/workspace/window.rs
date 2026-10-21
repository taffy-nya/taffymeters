//! One window's UI body. Production viewports and the automation harness both call
//! [`Workspace::window_ui`]; every user operation goes through `apply_action`.

use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, PointerButton, Rect, Response, Sense, Stroke,
    Vec2, ViewportCommand, pos2,
};

use super::{
    ModuleInstanceId, MoveTarget, Side, SplitId, WindowId, Workspace, WorkspaceAction,
    desktop::{self, Edge},
    layout::{Axis, DividerGeometry},
    settings_window::SettingsItem,
};
use crate::{
    audio::{AudioDescriptor, ChannelLayout, SourceState, SourceStatus},
    dataflow::ChannelSelection,
    module::{FRAME_INTERVAL, ModuleFrame},
    render::{EguiRenderTarget, Point, Rect as RenderRect},
    theme::ColorRole,
};

/// Width of the right / bottom split entries, in logical points; a quarter of the leaf
/// when that is less.
pub(crate) const SPLIT_ZONE_POINTS: f32 = 20.0;
/// Thickness of the native resize edges and size of the corner squares; small windows
/// scale both down so the middle stays content.
pub(crate) const RESIZE_EDGE_POINTS: f32 = 6.0;
pub(crate) const RESIZE_CORNER_POINTS: f32 = 12.0;
/// Poll interval for state that changes without new audio data or input.
const STATE_POLL: Duration = Duration::from_millis(250);
/// How long a left press in a locked window shows the "locked" hint.
const LOCK_HINT: Duration = Duration::from_secs(2);

/// Catppuccin Mocha colors of chrome that has no theme role.
const MOCHA_CRUST: Color32 = Color32::from_rgb(0x09, 0x09, 0x0d);
const MOCHA_RED: Color32 = Color32::from_rgb(0xf3, 0x8b, 0xa8);
const MOCHA_YELLOW: Color32 = Color32::from_rgb(0xf9, 0xe2, 0xaf);
const MOCHA_PEACH: Color32 = Color32::from_rgb(0xfa, 0xb3, 0x87);
/// Catppuccin Latte counterparts for the Light preset. Warnings use peach: Latte
/// yellow is too faint as text on the light surfaces.
const LATTE_RED: Color32 = Color32::from_rgb(0xd2, 0x0f, 0x39);
const LATTE_PEACH: Color32 = Color32::from_rgb(0xfe, 0x64, 0x0b);

/// egui color of a theme color.
pub(crate) fn color32(color: crate::render::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, color.alpha)
}

/// Make egui's widgets (menus, buttons, the settings window) follow the global
/// palette. Every viewport shares one style, so each pass applies it; the style is
/// only rebuilt when the palette differs from the one last applied to `ctx`.
///
/// Labels are plain text: egui's label selection is one state per `Context`, and
/// every viewport's pass ends by dropping a selection it did not draw, so with
/// several viewports it never selects anything and only showed a text cursor.
pub(crate) fn apply_chrome_style(ctx: &egui::Context, palette: &crate::theme::Palette) {
    let id = Id::new("taffymeters.chrome_palette");
    if ctx.data(|data| data.get_temp::<crate::theme::Palette>(id)) == Some(*palette) {
        return;
    }
    ctx.data_mut(|data| data.insert_temp(id, *palette));
    let theme = if palette.dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    ctx.set_visuals_of(theme, chrome_visuals(palette));
    ctx.all_styles_mut(|style| style.interaction.selectable_labels = false);
}

fn chrome_visuals(palette: &crate::theme::Palette) -> egui::Visuals {
    let opaque = |role| {
        let color = palette.color(role);
        (
            Color32::from_rgb(color.red, color.green, color.blue),
            color.alpha,
        )
    };
    let background = opaque(ColorRole::Background).0;
    // Translucent roles are seen over the background.
    let over = |role| {
        let (color, alpha) = opaque(role);
        background.lerp_to_gamma(color, f32::from(alpha) / 255.0)
    };
    let surface = over(ColorRole::Surface);
    let divider = over(ColorRole::Divider);
    let accent = over(ColorRole::Accent);
    let text = over(ColorRole::Text);
    let muted = over(ColorRole::Muted);
    // Readable text on an accent fill.
    let [red, green, blue, _] = accent.to_array();
    let luma = 0.299 * f32::from(red) + 0.587 * f32::from(green) + 0.114 * f32::from(blue);
    let on_accent = if luma > 140.0 {
        MOCHA_CRUST
    } else {
        Color32::WHITE
    };
    let mut visuals = if palette.dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let line = |width: f32, color: Color32| Stroke::new(width, color);
    let control = surface.lerp_to_gamma(divider, 0.6);
    let hover = divider;
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = surface;
    widgets.noninteractive.weak_bg_fill = surface;
    widgets.noninteractive.bg_stroke = line(1.0, divider);
    widgets.noninteractive.fg_stroke = line(1.0, text);
    widgets.inactive.bg_fill = control;
    widgets.inactive.weak_bg_fill = control;
    widgets.inactive.bg_stroke = line(1.0, divider);
    widgets.inactive.fg_stroke = line(1.0, text);
    for state in [&mut widgets.hovered, &mut widgets.open] {
        state.bg_fill = hover;
        state.weak_bg_fill = hover;
        state.bg_stroke = line(1.0, accent);
        state.fg_stroke = line(1.5, text);
    }
    let pressed = hover.lerp_to_gamma(accent, 0.35);
    widgets.active.bg_fill = pressed;
    widgets.active.weak_bg_fill = pressed;
    widgets.active.bg_stroke = line(1.0, accent);
    widgets.active.fg_stroke = line(2.0, text);
    visuals.selection.bg_fill = accent;
    if palette.dark {
        visuals.error_fg_color = MOCHA_RED;
        visuals.warn_fg_color = MOCHA_YELLOW;
    } else {
        visuals.error_fg_color = LATTE_RED;
        visuals.warn_fg_color = LATTE_PEACH;
    }
    visuals.selection.stroke = line(1.0, on_accent);
    visuals.hyperlink_color = accent;
    visuals.weak_text_color = Some(muted);
    visuals.panel_fill = background;
    visuals.window_fill = surface;
    visuals.window_stroke = line(1.0, divider);
    visuals.faint_bg_color = surface;
    visuals.extreme_bg_color = background;
    visuals.code_bg_color = surface;
    visuals.slider_trailing_fill = true;
    visuals
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ResizeEdge {
    North,
    South,
    East,
    West,
    NorthEast,
    NorthWest,
    SouthEast,
    SouthWest,
}

impl ResizeEdge {
    const ALL: [Self; 8] = [
        Self::NorthWest,
        Self::NorthEast,
        Self::SouthWest,
        Self::SouthEast,
        Self::North,
        Self::South,
        Self::West,
        Self::East,
    ];

    fn direction(self) -> egui::ResizeDirection {
        match self {
            Self::North => egui::ResizeDirection::North,
            Self::South => egui::ResizeDirection::South,
            Self::East => egui::ResizeDirection::East,
            Self::West => egui::ResizeDirection::West,
            Self::NorthEast => egui::ResizeDirection::NorthEast,
            Self::NorthWest => egui::ResizeDirection::NorthWest,
            Self::SouthEast => egui::ResizeDirection::SouthEast,
            Self::SouthWest => egui::ResizeDirection::SouthWest,
        }
    }

    fn cursor(self) -> CursorIcon {
        match self {
            Self::North | Self::South => CursorIcon::ResizeVertical,
            Self::East | Self::West => CursorIcon::ResizeHorizontal,
            Self::NorthWest | Self::SouthEast => CursorIcon::ResizeNwSe,
            Self::NorthEast | Self::SouthWest => CursorIcon::ResizeNeSw,
        }
    }

    /// Non-overlapping hit rect: corners are squares, edges stop at the corners. A
    /// corner is at most a quarter of the shorter side, edges half a corner thick.
    fn rect(self, window: Rect) -> Rect {
        let (min, max) = (window.min, window.max);
        let c = RESIZE_CORNER_POINTS.min(window.width().min(window.height()) / 4.0);
        let b = RESIZE_EDGE_POINTS * c / RESIZE_CORNER_POINTS;
        match self {
            Self::NorthWest => Rect::from_min_max(min, pos2(min.x + c, min.y + c)),
            Self::NorthEast => Rect::from_min_max(pos2(max.x - c, min.y), pos2(max.x, min.y + c)),
            Self::SouthWest => Rect::from_min_max(pos2(min.x, max.y - c), pos2(min.x + c, max.y)),
            Self::SouthEast => Rect::from_min_max(pos2(max.x - c, max.y - c), max),
            Self::North => Rect::from_min_max(pos2(min.x + c, min.y), pos2(max.x - c, min.y + b)),
            Self::South => Rect::from_min_max(pos2(min.x + c, max.y - b), pos2(max.x - c, max.y)),
            Self::West => Rect::from_min_max(pos2(min.x, min.y + c), pos2(min.x + b, max.y - c)),
            Self::East => Rect::from_min_max(pos2(max.x - b, min.y + c), pos2(max.x, max.y - c)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ControlKey {
    LeafBody(ModuleInstanceId),
    SplitZone {
        instance_id: ModuleInstanceId,
        side: Side,
    },
    Divider(SplitId),
    Resize(ResizeEdge),
    EmptyBody,
    /// The empty window's "Add module" button and its entries.
    AddOpen,
    AddModule(&'static str),
    MoveTarget(MoveTarget),
    Settings(SettingsItem),
    /// The typed value next to a slider in the settings window.
    SettingsValue(SettingsItem),
    /// The Reset next to a value in the settings window.
    SettingsReset(SettingsItem),
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ControlTarget {
    pub(crate) rect: Rect,
    pub(crate) enabled: bool,
    pub(crate) hovered: bool,
    pub(crate) frame_token: u64,
    pub(crate) pass_token: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DividerDrag {
    pub(crate) split_id: SplitId,
    pub(crate) start_ratio: f32,
    current_ratio: f32,
    pixels_per_point: f32,
    cancelled: bool,
}

/// A line drawn along a window's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EdgeLine {
    /// Another window touches this edge.
    Divider,
    /// The window moves with the one the settings' Window tab edits.
    Outline,
    /// Grouping (its button hovered) would add the window.
    Candidate,
}

impl EdgeLine {
    fn color(self, chrome: &crate::theme::Palette) -> Color32 {
        match self {
            Self::Divider => color32(chrome.color(ColorRole::Divider)),
            Self::Outline => color32(chrome.color(ColorRole::Accent)),
            Self::Candidate => color32(chrome.color(ColorRole::Accent)).gamma_multiply(0.5),
        }
    }
}

/// A line one pass drew along the window's frame (physical span along `edge`).
#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DrawnEdge {
    pub(crate) line: EdgeLine,
    pub(crate) edge: Edge,
    pub(crate) start: f32,
    pub(crate) end: f32,
}

#[derive(Default)]
pub(crate) struct WindowUiState {
    pub(crate) divider_drag: Option<DividerDrag>,
    pub(crate) display_time: Duration,
    min_size_sent: Option<Vec2>,
    /// Native resizability last commanded (`None`: still the builder's).
    resizable_sent: Option<bool>,
    /// Display time until which the "locked" hint shows.
    lock_hint_until: Option<Duration>,
    #[cfg(any(test, feature = "automation"))]
    targets: std::collections::HashMap<ControlKey, ControlTarget>,
    #[cfg(any(test, feature = "automation"))]
    frame_token: u64,
    #[cfg(any(test, feature = "automation"))]
    pass_token: u64,
    /// Lines the last pass drew along the frame.
    #[cfg(any(test, feature = "automation"))]
    pub(crate) edges: Vec<DrawnEdge>,
    /// The module the last pass outlined, and the logical rect of the outline.
    #[cfg(any(test, feature = "automation"))]
    pub(crate) module_outline: Option<(ModuleInstanceId, Rect)>,
}

/// Responses of probe-visible controls collected during one pass.
#[derive(Default)]
pub(crate) struct Controls {
    #[cfg(any(test, feature = "automation"))]
    pub(super) responses: Vec<(ControlKey, Response)>,
}

impl Controls {
    pub(crate) fn record(&mut self, _key: ControlKey, _response: &Response) {
        #[cfg(any(test, feature = "automation"))]
        self.responses.push((_key, _response.clone()));
    }
}

fn to_render_rect(rect: Rect) -> RenderRect {
    RenderRect {
        min: Point {
            x: rect.min.x,
            y: rect.min.y,
        },
        max: Point {
            x: rect.max.x,
            y: rect.max.y,
        },
    }
}

/// Sides with a split entry: the leaf's right and bottom edges.
const SPLIT_SIDES: [Side; 2] = [Side::Right, Side::Below];

fn split_zone(rect: Rect, side: Side) -> Rect {
    let right = SPLIT_ZONE_POINTS.min(rect.width() / 4.0);
    match side {
        Side::Right => Rect::from_min_max(pos2(rect.max.x - right, rect.min.y), rect.max),
        _ => Rect::from_min_max(
            pos2(
                rect.min.x,
                rect.max.y - SPLIT_ZONE_POINTS.min(rect.height() / 4.0),
            ),
            pos2(rect.max.x - right, rect.max.y),
        ),
    }
}

fn move_zone(rect: Rect, side: Side) -> Rect {
    let w = rect.width() / 4.0;
    let h = rect.height() / 4.0;
    match side {
        Side::Left => Rect::from_min_max(rect.min, pos2(rect.min.x + w, rect.max.y)),
        Side::Right => Rect::from_min_max(pos2(rect.max.x - w, rect.min.y), rect.max),
        Side::Above => Rect::from_min_max(
            pos2(rect.min.x + w, rect.min.y),
            pos2(rect.max.x - w, rect.min.y + h),
        ),
        Side::Below => Rect::from_min_max(
            pos2(rect.min.x + w, rect.max.y - h),
            pos2(rect.max.x - w, rect.max.y),
        ),
    }
}

pub(crate) fn channel_for_index(layout: ChannelLayout, index: u16) -> ChannelSelection {
    match (layout, index) {
        (ChannelLayout::Stereo, 0) => ChannelSelection::Left,
        (ChannelLayout::Stereo, 1) => ChannelSelection::Right,
        _ => ChannelSelection::Discrete(index),
    }
}

pub(crate) fn channel_options(
    descriptor: &AudioDescriptor,
) -> impl Iterator<Item = (u16, ChannelSelection)> + '_ {
    (0..descriptor.channel_count)
        .map(|index| (index, channel_for_index(descriptor.channel_layout, index)))
}

pub(crate) fn source_status_text(status: &SourceStatus) -> String {
    use std::fmt::Write as _;

    let state = match status.state {
        SourceState::Starting => "starting",
        SourceState::Running => "running",
        SourceState::Paused => "paused",
        SourceState::DeviceLost => "device lost",
        SourceState::Stopped => "stopped",
    };
    let mut text = format!("Source: {state}");
    if status.dropped_blocks > 0 {
        let _ = write!(text, " | dropped {}", status.dropped_blocks);
    }
    if status.invalid_samples > 0 {
        let _ = write!(text, " | invalid {}", status.invalid_samples);
    }
    if let Some(detail) = &status.detail {
        let _ = write!(text, " | {detail}");
    }
    text
}

impl Workspace {
    #[cfg(feature = "automation")]
    pub(crate) fn control_targets(
        &self,
        window_id: WindowId,
    ) -> std::collections::HashMap<ControlKey, ControlTarget> {
        self.windows
            .get(&window_id)
            .map(|window| window.ui.targets.clone())
            .unwrap_or_default()
    }

    /// Run one UI pass of `window_id` inside its own viewport. Returns whether the
    /// window was drawn (a minimized window is not).
    pub(crate) fn window_ui(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        display_time: Duration,
    ) -> bool {
        let pass_started = std::time::Instant::now();
        let ctx = ui.ctx().clone();
        // A minimized window is not drawn; its Modules' continuous analysis continues
        // and work that only serves the display stops until it is shown again. Its
        // native commands (e.g. from the settings window) are still sent.
        let minimized = ctx.input(|input| input.viewport().minimized == Some(true));
        self.set_window_minimized(window_id, minimized);
        if minimized {
            self.send_window_commands(window_id, &ctx);
            return false;
        }
        let rect = ui.max_rect();
        apply_chrome_style(&ctx, &self.chrome);
        ui.painter()
            .rect_filled(rect, 0.0, color32(self.chrome.color(ColorRole::Background)));
        let windows_before: Vec<WindowId> = self.windows.keys().copied().collect();
        let settings_before = self.settings.is_some();
        self.tick(display_time);
        let repaint = self.window_pass(window_id, ui, rect, display_time);
        if let Some(delay) = repaint {
            let now = std::time::Instant::now();
            let wait = if delay <= FRAME_INTERVAL {
                // All animating windows share one frame clock, so their repaints come
                // due together instead of each window rescheduling itself at once.
                // Back-to-back immediate repaints of one window keep the Windows message
                // queue busy, and the OS then never delivers the other windows' paints.
                self.next_frame(now).saturating_duration_since(now)
            } else {
                delay.saturating_sub(pass_started.elapsed())
            };
            // egui subtracts its predicted frame time from every delay; add it back.
            let predicted = ctx.input(|input| input.predicted_dt);
            let predicted = Duration::try_from_secs_f32(predicted).unwrap_or_default();
            // Windows are drawn inside the host (root) viewport's pass: schedule that.
            ctx.request_repaint_after_for(wait + predicted, egui::ViewportId::ROOT);
        }
        let windows_after: Vec<WindowId> = self.windows.keys().copied().collect();
        if windows_before != windows_after || settings_before != self.settings.is_some() {
            // The host (root) pass owns native viewport creation and removal.
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        // Drawing is otherwise driven by published data and input; keep polling only
        // for state that changes without either: a detach waiting for its window to
        // open (timeout) and a source that is not running (status badge).
        let pending_detach = self
            .windows
            .values()
            .any(|window| window.pending_detach.is_some() && !window.opened);
        if pending_detach || self.source_status().state != SourceState::Running {
            ctx.request_repaint_after_for(STATE_POLL, egui::ViewportId::ROOT);
        }
        // The status badge drops an error or the notices after a while, with or
        // without new data.
        if let Some(change) = self.badge_change_in() {
            ctx.request_repaint_after_for(
                change.max(Duration::from_millis(1)),
                egui::ViewportId::ROOT,
            );
        }
        self.send_window_commands(window_id, &ctx);
        true
    }

    /// Send the viewport commands queued for `window_id` to its native window.
    fn send_window_commands(&mut self, window_id: WindowId, ctx: &egui::Context) {
        if let Some(window) = self.windows.get_mut(&window_id) {
            for command in window.commands.drain(..) {
                ctx.send_viewport_cmd(command);
            }
        }
    }

    /// Next tick of the shared 60 fps clock after `now`.
    fn next_frame(&mut self, now: std::time::Instant) -> std::time::Instant {
        let next = match self.frame_clock {
            Some(tick) if tick > now => tick,
            Some(tick) if tick + FRAME_INTERVAL > now => tick + FRAME_INTERVAL,
            _ => now + FRAME_INTERVAL,
        };
        self.frame_clock = Some(next);
        next
    }

    /// One window pass; returns the shortest repaint delay its Modules asked for.
    fn window_pass(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        display_time: Duration,
    ) -> Option<Duration> {
        let ctx = ui.ctx().clone();
        let window = self.windows.get_mut(&window_id)?;
        window.ui.display_time = display_time;
        #[cfg(any(test, feature = "automation"))]
        {
            if ctx.current_pass_index() == 0 {
                window.ui.frame_token += 1;
            }
            window.ui.pass_token += 1;
            window.ui.targets.clear();
        }
        if !window.opened {
            window.opened = true;
            if window.pending_detach.is_some() {
                self.commit_detach(window_id);
                if !self.windows.contains_key(&window_id) {
                    return None;
                }
            }
        }

        // Native placement, in this window's own scale.
        let pixels_per_point = ctx.pixels_per_point();
        let (outer, maximized, minimized) = ctx.input(|input| {
            let viewport = input.viewport();
            (
                viewport.outer_rect,
                viewport.maximized.unwrap_or(false),
                viewport.minimized.unwrap_or(false),
            )
        });
        if self.observe_placement(window_id, outer, pixels_per_point, maximized, minimized) {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        self.publish_groups();
        let window = self.windows.get_mut(&window_id).expect("checked above");
        if let Some(drag) = window.ui.divider_drag
            && (drag.pixels_per_point - pixels_per_point).abs() > f32::EPSILON
        {
            // Old logical coordinates are meaningless at the new scale.
            let _ = window
                .layout
                .set_ratio(drag.split_id, drag.start_ratio, false);
            window.ui.divider_drag = None;
        }
        window.content_size = rect.size();
        let min_size = self.window_min_size(&self.windows[&window_id]);
        let window = self.windows.get_mut(&window_id).expect("checked above");
        if window.ui.min_size_sent != Some(min_size) {
            window.ui.min_size_sent = Some(min_size);
            window
                .commands
                .push(ViewportCommand::MinInnerSize(min_size));
        }
        // A locked window takes right clicks only and has no native sizing either.
        let locked = window.locked;
        if window.ui.resizable_sent.unwrap_or(window.initial_resizable) == locked {
            window.commands.push(ViewportCommand::Resizable(!locked));
        }
        window.ui.resizable_sent = Some(!locked);
        if locked && ui.input(|input| input.pointer.primary_pressed()) {
            window.ui.lock_hint_until = Some(display_time + LOCK_HINT);
        }

        self.handle_shortcuts(window_id, ui);
        if !self.windows.contains_key(&window_id) {
            return None;
        }

        let mut controls = Controls::default();
        let mut actions = Vec::new();
        let mut repaint = None;
        if self.windows[&window_id].layout.is_empty() {
            self.empty_window(window_id, ui, rect, locked, &mut controls, &mut actions);
        } else {
            repaint = self.layout_ui(
                window_id,
                ui,
                rect,
                display_time,
                maximized,
                locked,
                &mut controls,
                &mut actions,
            );
        }
        self.window_edges(window_id, ui, rect);
        self.module_outline(window_id, ui, rect);
        if self.move_source.is_some() {
            self.move_targets(window_id, ui, rect, &mut controls, &mut actions);
        }
        if !maximized && !locked {
            Self::resize_edges(ui, rect, &mut controls);
        }
        self.status_badge(window_id, ui, rect);
        if let Some(remaining) = self.lock_hint(window_id, ui, rect) {
            repaint = Some(repaint.map_or(remaining, |current: Duration| current.min(remaining)));
        }

        #[cfg(any(test, feature = "automation"))]
        if let Some(window) = self.windows.get_mut(&window_id) {
            for (key, response) in controls.responses {
                window.ui.targets.insert(
                    key,
                    ControlTarget {
                        rect: response.rect,
                        enabled: response.enabled(),
                        hovered: response.hovered(),
                        frame_token: window.ui.frame_token,
                        pass_token: window.ui.pass_token,
                    },
                );
            }
        }
        for action in actions {
            let _ = self.apply_action(action);
        }
        repaint
    }

    fn handle_shortcuts(&mut self, window_id: WindowId, ui: &egui::Ui) {
        let ctx = ui.ctx();
        let (escape, close, space) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::Escape),
                input.modifiers.command && input.key_pressed(egui::Key::W),
                input.key_pressed(egui::Key::Space),
            )
        });
        if escape && !egui::Popup::is_any_open(ctx) {
            let pending_detach = self
                .windows
                .iter()
                .find(|(_, window)| {
                    !window.opened
                        && window
                            .pending_detach
                            .is_some_and(|pending| pending.source == window_id)
                })
                .map(|(id, _)| *id);
            let window = self.windows.get_mut(&window_id).expect("pass checked");
            if let Some(drag) = window
                .ui
                .divider_drag
                .as_mut()
                .filter(|drag| !drag.cancelled)
            {
                drag.cancelled = true;
                let (split_id, ratio) = (drag.split_id, drag.start_ratio);
                let _ = window.layout.set_ratio(split_id, ratio, false);
            } else if self.move_source.is_some() {
                let _ = self.apply_action(WorkspaceAction::CancelMove);
            } else if let Some(pending) = pending_detach {
                let _ = self.apply_action(WorkspaceAction::CancelDetach { window_id: pending });
            }
        }
        if close {
            let _ = self.apply_action(WorkspaceAction::CloseWindow { window_id });
            return;
        }
        if space
            && matches!(
                self.source_status().state,
                SourceState::Running | SourceState::Paused
            )
        {
            let _ = self.apply_action(WorkspaceAction::TogglePause);
        }
    }

    /// Hit rect of a leaf's split entry, or `None` when it is hidden (`hidden`: while
    /// picking a move target or in a locked window) or a copy would not fit there.
    fn split_entry(
        &self,
        leaf: ModuleInstanceId,
        leaf_rect: Rect,
        side: Side,
        hidden: bool,
    ) -> Option<Rect> {
        (!hidden && self.can_split_in(leaf, side, leaf_rect.size()))
            .then(|| split_zone(leaf_rect, side))
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_ui(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        display_time: Duration,
        maximized: bool,
        locked: bool,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) -> Option<Duration> {
        let ctx = ui.ctx().clone();
        let mut repaint = None;
        let geometry = {
            let window = &self.windows[&window_id];
            window.layout.geometry(rect, &|id| self.module_min_size(id))
        };
        let picking = self.move_source.is_some();
        // No split entries while picking a move target or in a locked window.
        let no_entries = picking || locked;
        let popup_open = egui::Popup::is_any_open(&ctx);
        let reference_dbfs = self.reference_dbfs();

        // Leaf bodies first: everything registered later sits on top of them. A
        // locked window's bodies only take right clicks.
        let body_sense = if locked {
            Sense::click()
        } else {
            Sense::click_and_drag()
        };
        for &(leaf, leaf_rect) in &geometry.leaves {
            let body = ui.interact(
                leaf_rect,
                Id::new(("taffymeters.leaf", window_id.0, leaf.0)),
                body_sense,
            );
            controls.record(ControlKey::LeafBody(leaf), &body);
            // With the settings window open, a left click selects the module there.
            if body.clicked() && !locked && !picking && self.settings_open() {
                actions.push(WorkspaceAction::SelectModule { instance_id: leaf });
            }
            if body.drag_started_by(PointerButton::Primary)
                && !picking
                && !self.drag_locked(window_id)
            {
                let press = ui
                    .input(|input| input.pointer.press_origin())
                    .unwrap_or(leaf_rect.center());
                self.start_move(window_id, press);
            }
            if !picking && body.secondary_clicked() {
                // Right click edits the module in the settings window.
                actions.push(WorkspaceAction::OpenSettings {
                    window_id,
                    instance_id: Some(leaf),
                });
            }
            // The Module only sees the pointer over its own content, not over the
            // window chrome drawn on top of it (split entries, dividers, resize edges);
            // in a locked window it sees no pointer, wheel or zoom at all.
            let entries =
                SPLIT_SIDES.map(|side| self.split_entry(leaf, leaf_rect, side, no_entries));
            let pointer = (body.contains_pointer() && !popup_open && !picking && !locked)
                .then(|| ui.input(|input| input.pointer.hover_pos()))
                .flatten()
                .filter(|position| {
                    leaf_rect.contains(*position)
                        && !entries
                            .iter()
                            .flatten()
                            .any(|zone| zone.contains(*position))
                        && !geometry
                            .dividers
                            .iter()
                            .any(|divider| divider.hit_rect().contains(*position))
                        && (maximized
                            || !ResizeEdge::ALL
                                .iter()
                                .any(|edge| edge.rect(rect).contains(*position)))
                });
            let (scroll_delta_y, zoom_delta) = if pointer.is_some() {
                ui.input(|input| (input.smooth_scroll_delta.y, input.zoom_delta()))
            } else {
                (0.0, 1.0)
            };
            let mut renderer = EguiRenderTarget::new(ui.painter_at(leaf_rect));
            if let Some(entry) = self.modules.get_mut(&leaf) {
                let response = entry.module.draw(ModuleFrame {
                    rect: to_render_rect(leaf_rect),
                    monotonic_time: display_time,
                    pointer: pointer.map(|position| Point {
                        x: position.x,
                        y: position.y,
                    }),
                    scroll_delta_y,
                    zoom_delta,
                    palette: &entry.palette,
                    reference_dbfs,
                    renderer: &mut renderer,
                });
                if let Some(delay) = response.repaint_after {
                    repaint = Some(repaint.map_or(delay, |current: Duration| current.min(delay)));
                }
            }
        }

        // Split entries, only where a copy fits: elsewhere the leaf body takes the
        // clicks, so even a minimum-size leaf stays selectable and draggable.
        let dragging = ui.input(|input| input.pointer.is_decidedly_dragging());
        for &(leaf, leaf_rect) in &geometry.leaves {
            for side in SPLIT_SIDES {
                let Some(zone) = self.split_entry(leaf, leaf_rect, side, no_entries) else {
                    continue;
                };
                let response = ui.interact(
                    zone,
                    Id::new(("taffymeters.split", window_id.0, leaf.0, side as u8)),
                    Sense::click(),
                );
                controls.record(
                    ControlKey::SplitZone {
                        instance_id: leaf,
                        side,
                    },
                    &response,
                );
                if response.hovered() && !dragging {
                    let painter = ui.painter_at(zone);
                    painter.rect_filled(
                        zone,
                        0.0,
                        color32(self.chrome.color(ColorRole::Accent)).gamma_multiply(0.35),
                    );
                    painter.text(
                        zone.center(),
                        Align2::CENTER_CENTER,
                        "+",
                        FontId::proportional(18.0),
                        Color32::WHITE,
                    );
                }
                if response.clicked() {
                    actions.push(WorkspaceAction::SplitModule {
                        instance_id: leaf,
                        side,
                    });
                }
            }
        }

        self.dividers(window_id, ui, &geometry.dividers, locked, controls, actions);
        repaint
    }

    /// Lines along this window's frame. Where another window touches it, the
    /// divider Modules have between them (each window draws its half of the
    /// width). While the settings' Window tab is open, the outline of the windows
    /// that move with the window it edits, and, dimmer while its Group button is
    /// hovered, of the windows grouping would add; edges they share are dividers.
    fn window_edges(&mut self, window_id: WindowId, ui: &egui::Ui, rect: Rect) {
        #[cfg(any(test, feature = "automation"))]
        let mut drawn = Vec::new();
        let window = &self.windows[&window_id];
        if let Some(own) = window.outer.filter(|_| !window.maximized) {
            let ppp = window.pixels_per_point;
            let painter = ui.painter();
            // One logical point in whole pixels, as the dividers inside a window.
            let width = ppp.round().max(1.0);
            let strip = |edge: Edge, start: f32, end: f32, pixels: f32| {
                let thickness = pixels / ppp;
                let x = |value: f32| rect.min.x + (value - own.min.x) / ppp;
                let y = |value: f32| rect.min.y + (value - own.min.y) / ppp;
                match edge {
                    Edge::Left => Rect::from_min_max(
                        pos2(rect.min.x, y(start)),
                        pos2(rect.min.x + thickness, y(end)),
                    ),
                    Edge::Right => Rect::from_min_max(
                        pos2(rect.max.x - thickness, y(start)),
                        pos2(rect.max.x, y(end)),
                    ),
                    Edge::Top => Rect::from_min_max(
                        pos2(x(start), rect.min.y),
                        pos2(x(end), rect.min.y + thickness),
                    ),
                    Edge::Bottom => Rect::from_min_max(
                        pos2(x(start), rect.max.y - thickness),
                        pos2(x(end), rect.max.y),
                    ),
                }
            };
            let this: &Self = self;
            let neighbors = move || {
                this.windows.iter().filter_map(move |(id, other)| {
                    (*id != window_id && !other.minimized && !other.maximized)
                        .then(|| this.frame_of(*id).map(|frame| (*id, frame)))
                        .flatten()
                })
            };
            let divider = EdgeLine::Divider.color(&self.chrome);
            for (_, other) in neighbors() {
                let Some((edge, start, end)) = desktop::shared_edge(own, other) else {
                    continue;
                };
                // The left / top window takes the odd pixel.
                let pixels = if matches!(edge, Edge::Right | Edge::Bottom) {
                    (width / 2.0).ceil()
                } else {
                    (width / 2.0).floor()
                };
                if pixels > 0.0 {
                    painter.rect_filled(strip(edge, start, end, pixels), 0.0, divider);
                }
                #[cfg(any(test, feature = "automation"))]
                drawn.push(DrawnEdge {
                    line: EdgeLine::Divider,
                    edge,
                    start,
                    end,
                });
            }
            let highlight = &self.window_highlight();
            if let Some(line) = highlight(window_id) {
                let color = line.color(&self.chrome);
                for edge in Edge::ALL {
                    let covered = || {
                        neighbors()
                            .filter(move |(id, _)| highlight(*id).is_some())
                            .filter_map(move |(_, other)| desktop::shared_edge(own, other))
                            .filter(move |(side, ..)| *side == edge)
                            .map(|(_, start, end)| (start, end))
                    };
                    desktop::uncovered(edge.span(own), covered, |start, end| {
                        painter.rect_filled(strip(edge, start, end, width), 0.0, color);
                        #[cfg(any(test, feature = "automation"))]
                        drawn.push(DrawnEdge {
                            line,
                            edge,
                            start,
                            end,
                        });
                    });
                }
            }
        }
        #[cfg(any(test, feature = "automation"))]
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.ui.edges = drawn;
        }
    }

    /// Outline of the selected module: the one "Move to…" moves (it stays undimmed
    /// under the target overlay), otherwise the one the settings' Module tab edits.
    /// An accent line inside the leaf, one logical point in whole pixels and on
    /// pixel edges, as the window outline.
    fn module_outline(&mut self, window_id: WindowId, ui: &egui::Ui, rect: Rect) {
        let target = self
            .move_source
            .or_else(|| self.module_tab_target())
            .filter(|id| self.window_of(*id) == Some(window_id));
        let leaf_rect = target.and_then(|id| {
            self.windows[&window_id]
                .layout
                .geometry(rect, &|leaf| self.module_min_size(leaf))
                .leaves
                .into_iter()
                .find_map(|(leaf, leaf_rect)| (leaf == id).then_some(leaf_rect))
        });
        #[cfg(any(test, feature = "automation"))]
        let mut drawn = None;
        if let Some(leaf_rect) = leaf_rect {
            let ppp = ui.ctx().pixels_per_point();
            let snap = |value: f32| (value * ppp).round() / ppp;
            let min = pos2(snap(leaf_rect.min.x), snap(leaf_rect.min.y));
            let max = pos2(snap(leaf_rect.max.x), snap(leaf_rect.max.y));
            let width = ppp.round().max(1.0) / ppp;
            let color = EdgeLine::Outline.color(&self.chrome);
            let painter = ui.painter();
            for strip in [
                Rect::from_min_max(min, pos2(max.x, min.y + width)),
                Rect::from_min_max(pos2(min.x, max.y - width), max),
                Rect::from_min_max(
                    pos2(min.x, min.y + width),
                    pos2(min.x + width, max.y - width),
                ),
                Rect::from_min_max(
                    pos2(max.x - width, min.y + width),
                    pos2(max.x, max.y - width),
                ),
            ] {
                painter.rect_filled(strip, 0.0, color);
            }
            #[cfg(any(test, feature = "automation"))]
            {
                drawn = target.map(|id| (id, Rect::from_min_max(min, max)));
            }
        }
        #[cfg(any(test, feature = "automation"))]
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.ui.module_outline = drawn;
        }
    }

    /// Which outline a window gets: the windows that move with the one the
    /// settings' Window tab edits, and, while its Group button is hovered, the
    /// windows grouping would add.
    fn window_highlight(&self) -> impl Fn(WindowId) -> Option<EdgeLine> + '_ {
        let target = self.window_tab_target();
        move |id| {
            let (target, group_hovered) = target?;
            let edited = self.windows.get(&target)?;
            let window = self.windows.get(&id)?;
            if id == target || (edited.group.is_some() && window.group == edited.group) {
                return Some(EdgeLine::Outline);
            }
            let edited_frame = self.frame_of(target);
            let touches = |other: WindowId| {
                edited_frame
                    .zip(self.frame_of(other))
                    .is_some_and(|(a, b)| desktop::touching(a, b))
            };
            let joins = touches(id)
                || window.group.is_some_and(|group| {
                    self.windows
                        .iter()
                        .any(|(other_id, other)| other.group == Some(group) && touches(*other_id))
                });
            (group_hovered && joins).then_some(EdgeLine::Candidate)
        }
    }

    /// Draw the dividers; outside a locked window they also take hover, drags and
    /// double clicks.
    fn dividers(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        dividers: &[DividerGeometry],
        locked: bool,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let pixels_per_point = ui.ctx().pixels_per_point();
        let grid = !ui.input(|input| input.modifiers.alt);
        let mut drag_seen = false;
        for divider in dividers {
            let line = match divider.axis {
                Axis::LeftRight => [
                    pos2(divider.position, divider.parent.min.y),
                    pos2(divider.position, divider.parent.max.y),
                ],
                Axis::TopBottom => [
                    pos2(divider.parent.min.x, divider.position),
                    pos2(divider.parent.max.x, divider.position),
                ],
            };
            if locked {
                let color = color32(self.chrome.color(ColorRole::Divider));
                ui.painter().line_segment(line, Stroke::new(1.0_f32, color));
                continue;
            }
            let response = ui.interact(
                divider.hit_rect(),
                Id::new(("taffymeters.divider", window_id.0, divider.id.0)),
                Sense::click_and_drag(),
            );
            controls.record(ControlKey::Divider(divider.id), &response);
            let window = self.windows.get_mut(&window_id).expect("pass checked");
            let active = window
                .ui
                .divider_drag
                .is_some_and(|drag| drag.split_id == divider.id);
            if response.hovered() || active {
                ui.ctx().set_cursor_icon(match divider.axis {
                    Axis::LeftRight => CursorIcon::ResizeHorizontal,
                    Axis::TopBottom => CursorIcon::ResizeVertical,
                });
            }
            if response.double_clicked() {
                actions.push(WorkspaceAction::ResetSplit {
                    window_id,
                    split_id: divider.id,
                });
            }
            if response.drag_started_by(PointerButton::Primary) {
                let ratio = window
                    .layout
                    .split(divider.id)
                    .map_or(0.5, |split| split.ratio);
                window.ui.divider_drag = Some(DividerDrag {
                    split_id: divider.id,
                    start_ratio: ratio,
                    current_ratio: ratio,
                    pixels_per_point,
                    cancelled: false,
                });
            }
            if let Some(drag) = window
                .ui
                .divider_drag
                .as_mut()
                .filter(|drag| drag.split_id == divider.id)
            {
                drag_seen = true;
                if !drag.cancelled
                    && response.dragged()
                    && let Some(pointer) = response.interact_pointer_pos()
                {
                    let ratio = divider.ratio_for_pointer(divider.axis.coordinate(pointer), grid);
                    drag.current_ratio = ratio;
                    actions.push(WorkspaceAction::SetSplitRatio {
                        window_id,
                        split_id: divider.id,
                        ratio,
                        manual: false,
                    });
                }
                if response.drag_stopped() {
                    let drag = window.ui.divider_drag.take().expect("matched above");
                    if !drag.cancelled
                        && (drag.current_ratio - drag.start_ratio).abs() > f32::EPSILON
                    {
                        actions.push(WorkspaceAction::SetSplitRatio {
                            window_id,
                            split_id: divider.id,
                            ratio: drag.current_ratio,
                            manual: true,
                        });
                    }
                }
            }
            let color = if response.hovered() || active {
                color32(self.chrome.color(ColorRole::Accent))
            } else {
                color32(self.chrome.color(ColorRole::Divider))
            };
            ui.painter().line_segment(line, Stroke::new(1.0_f32, color));
        }
        // The dragged split disappeared (closed leaf / moved module): end the capture.
        let window = self.windows.get_mut(&window_id).expect("pass checked");
        if !drag_seen && window.ui.divider_drag.is_some() {
            window.ui.divider_drag = None;
        }
    }

    fn resize_edges(ui: &mut egui::Ui, rect: Rect, controls: &mut Controls) {
        for edge in ResizeEdge::ALL {
            let response = ui.interact(
                edge.rect(rect),
                Id::new(("taffymeters.resize", edge as u8)),
                Sense::drag(),
            );
            controls.record(ControlKey::Resize(edge), &response);
            if response.hovered() || response.dragged() {
                ui.ctx().set_cursor_icon(edge.cursor());
            }
            if response.drag_started_by(PointerButton::Primary) {
                ui.ctx()
                    .send_viewport_cmd(ViewportCommand::BeginResize(edge.direction()));
            }
        }
    }

    /// An empty window: its body drags the window and an "Add module" button sits in
    /// the middle; a locked one takes right clicks only and shows no button.
    fn empty_window(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        locked: bool,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let picking = self.move_source.is_some();
        let body = ui.interact(
            rect,
            Id::new(("taffymeters.empty", window_id.0)),
            if locked {
                Sense::click()
            } else {
                Sense::click_and_drag()
            },
        );
        controls.record(ControlKey::EmptyBody, &body);
        if body.drag_started_by(PointerButton::Primary) && !picking && !self.drag_locked(window_id)
        {
            let press = ui
                .input(|input| input.pointer.press_origin())
                .unwrap_or(rect.center());
            self.start_move(window_id, press);
        }
        if self.windows[&window_id].pending_detach.is_some() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Opening…",
                FontId::proportional(14.0),
                color32(self.chrome.color(ColorRole::Muted)),
            );
            return;
        }
        if picking {
            return;
        }
        // With the settings window open, a left click selects the window there.
        if body.clicked() && !locked && self.settings_open() {
            actions.push(WorkspaceAction::SelectWindow { window_id });
        }
        let button = !locked && {
            // egui places the button at the top-left of the space it is given, at
            // its own size: give it exactly that size, centered in the window.
            let label = "Add module";
            let text = egui::WidgetText::from(label)
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::TextStyle::Button,
                )
                .size();
            let spacing = ui.spacing();
            let size = egui::vec2(
                text.x + 2.0 * spacing.button_padding.x,
                (text.y + 2.0 * spacing.button_padding.y).max(spacing.interact_size.y),
            );
            let button_rect = Rect::from_center_size(rect.center(), size);
            ui.scope_builder(egui::UiBuilder::new().max_rect(button_rect), |ui| {
                let response = ui.menu_button(label, |ui| {
                    self.add_menu(ui, controls, actions, window_id);
                });
                controls.record(ControlKey::AddOpen, &response.response);
                response.response.secondary_clicked()
            })
            .inner
        };
        // A right click anywhere in the empty window opens its settings.
        if body.secondary_clicked() || button {
            actions.push(WorkspaceAction::OpenSettings {
                window_id,
                instance_id: None,
            });
        }
    }

    fn add_menu(
        &self,
        ui: &mut egui::Ui,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
        window_id: WindowId,
    ) {
        ui.label("Add module");
        for (type_id, label) in self.registry.module_types() {
            let response = ui.button(label);
            controls.record(ControlKey::AddModule(type_id), &response);
            if response.clicked() {
                actions.push(WorkspaceAction::AddModule { window_id, type_id });
            }
        }
    }

    /// Modal target overlay of "Move to…": every other leaf is dimmed and offers four
    /// edge targets, an empty window is dimmed and offers itself; hovering an edge
    /// shows the half of the leaf the module would take. The moved leaf stays
    /// undimmed, outlined by `module_outline`. Escape or a right click cancels.
    fn move_targets(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let Some(source) = self.move_source else {
            return;
        };
        let geometry = self.windows[&window_id]
            .layout
            .geometry(rect, &|id| self.module_min_size(id));
        let empty = self.windows[&window_id].layout.is_empty();
        let ctx = ui.ctx().clone();
        egui::Area::new(Id::new(("taffymeters.move", window_id.0)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(&ctx, |ui| {
                ui.set_min_size(rect.size());
                let painter = ui.painter_at(rect);
                let shade = Color32::from_black_alpha(90);
                let mut targets: Vec<(MoveTarget, Rect)> = Vec::new();
                if empty {
                    painter.rect_filled(rect, 0.0, shade);
                    targets.push((MoveTarget::EmptyWindow(window_id), rect.shrink(8.0)));
                }
                for &(leaf, leaf_rect) in &geometry.leaves {
                    if leaf == source {
                        continue;
                    }
                    painter.rect_filled(leaf_rect, 0.0, shade);
                    for side in Side::ALL {
                        targets.push((
                            MoveTarget::Beside {
                                instance_id: leaf,
                                side,
                            },
                            move_zone(leaf_rect, side),
                        ));
                    }
                }
                let accent = color32(self.chrome.color(ColorRole::Accent));
                let mut zones = Vec::with_capacity(targets.len());
                for (target, zone) in targets {
                    let valid = self.move_target_valid(source, target);
                    let response = ui
                        .add_enabled_ui(valid, |ui| {
                            ui.interact(
                                zone,
                                Id::new(("taffymeters.move-target", window_id.0, target)),
                                Sense::click(),
                            )
                        })
                        .inner;
                    controls.record(ControlKey::MoveTarget(target), &response);
                    let hovered = valid && response.hovered();
                    // Under the zones: where the module would land.
                    if hovered
                        && let MoveTarget::Beside { instance_id, side } = target
                        && let Some(landing) =
                            self.move_landing(source, instance_id, side, window_id, rect)
                    {
                        painter.rect_filled(landing, 0.0, accent.gamma_multiply(0.3));
                    }
                    if valid && response.clicked() {
                        actions.push(WorkspaceAction::MoveModule {
                            instance_id: source,
                            target,
                        });
                    }
                    zones.push((zone, valid, hovered));
                }
                for (zone, valid, hovered) in zones {
                    let fill = match (valid, hovered) {
                        (true, true) => accent.gamma_multiply(0.7),
                        (true, false) => accent.gamma_multiply(0.25),
                        (false, _) => Color32::from_white_alpha(8),
                    };
                    painter.rect_filled(zone.shrink(2.0), 0.0, fill);
                }
            });
        if ui.input(|input| input.pointer.secondary_clicked()) {
            actions.push(WorkspaceAction::CancelMove);
        }
    }

    /// Text of the status badge: the source state when it is not running, the
    /// operation error and the notices. A single notice shows in full when the
    /// whole line `fits`; otherwise the badge counts them and points to where
    /// they are listed.
    pub(crate) fn badge_text(&self, fits: impl Fn(&str) -> bool) -> Option<String> {
        let status = self.source_status();
        let mut parts = Vec::new();
        if status.state != SourceState::Running {
            parts.push(source_status_text(&status));
        }
        if let Some((error, _)) = &self.control_error {
            parts.push(error.clone());
        }
        let notices = self.badge_notices();
        if let [notice] = notices {
            let mut full = parts.clone();
            full.push(notice.clone());
            let full = full.join(" | ");
            if fits(&full) {
                return Some(full);
            }
        }
        match notices.len() {
            0 => {}
            1 => parts.push("1 notice · Settings › Workspace".to_owned()),
            count => parts.push(format!("{count} notices · Settings › Workspace")),
        }
        (!parts.is_empty()).then(|| parts.join(" | "))
    }

    fn status_badge(&self, window_id: WindowId, ui: &egui::Ui, rect: Rect) {
        if !self.windows.contains_key(&window_id) {
            return;
        }
        let painter = ui.painter_at(rect);
        let font = FontId::proportional(12.0);
        // Inside the window with the badge's margin and padding on both sides.
        let room = rect.width() - 24.0;
        let Some(text) = self.badge_text(|text| {
            painter
                .layout_no_wrap(text.to_owned(), font.clone(), MOCHA_PEACH)
                .size()
                .x
                <= room
        }) else {
            return;
        };
        let galley = painter.layout_no_wrap(text, font, MOCHA_PEACH);
        let origin = pos2(rect.min.x + 8.0, rect.max.y - galley.size().y - 8.0);
        painter.rect_filled(
            Rect::from_min_size(origin, galley.size()).expand(4.0),
            3.0,
            MOCHA_CRUST.gamma_multiply_u8(200),
        );
        painter.galley(origin, galley, Color32::WHITE);
    }

    /// The hint a left press in a locked window shows for a while; returns how long
    /// it still shows.
    fn lock_hint(&mut self, window_id: WindowId, ui: &egui::Ui, rect: Rect) -> Option<Duration> {
        let window = self.windows.get_mut(&window_id)?;
        let now = window.ui.display_time;
        window.ui.lock_hint_until = window
            .ui
            .lock_hint_until
            .filter(|until| window.locked && *until > now);
        let remaining = window.ui.lock_hint_until? - now;
        let painter = ui.painter_at(rect);
        let galley = painter.layout_no_wrap(
            "Locked · right-click for settings".to_owned(),
            FontId::proportional(12.0),
            Color32::WHITE,
        );
        let origin = rect.center() - galley.size() / 2.0;
        painter.rect_filled(
            Rect::from_min_size(origin, galley.size()).expand(4.0),
            3.0,
            MOCHA_CRUST.gamma_multiply_u8(200),
        );
        painter.galley(origin, galley, Color32::WHITE);
        Some(remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Generation, SourceId};

    #[test]
    fn every_discrete_input_channel_has_a_selectable_option() {
        let descriptor = AudioDescriptor::new(
            SourceId(1),
            Generation(1),
            48_000,
            4,
            ChannelLayout::UnknownDiscrete,
        )
        .unwrap();
        let options: Vec<_> = channel_options(&descriptor).collect();
        assert_eq!(
            options,
            vec![
                (0, ChannelSelection::Discrete(0)),
                (1, ChannelSelection::Discrete(1)),
                (2, ChannelSelection::Discrete(2)),
                (3, ChannelSelection::Discrete(3)),
            ]
        );
    }

    #[test]
    fn shared_source_status_exposes_failures_and_telemetry() {
        let status = SourceStatus {
            state: SourceState::DeviceLost,
            dropped_blocks: 3,
            invalid_samples: 7,
            detail: Some("endpoint unavailable".to_owned()),
        };
        assert_eq!(
            source_status_text(&status),
            "Source: device lost | dropped 3 | invalid 7 | endpoint unavailable"
        );
    }

    #[test]
    fn resize_hit_rects_do_not_overlap_corners_win_and_small_windows_keep_a_middle() {
        for size in [egui::vec2(400.0, 300.0), egui::vec2(48.0, 16.0)] {
            let window = Rect::from_min_size(pos2(0.0, 0.0), size);
            for (index, a) in ResizeEdge::ALL.iter().enumerate() {
                for b in &ResizeEdge::ALL[index + 1..] {
                    assert!(
                        !a.rect(window).intersects(b.rect(window))
                            || a.rect(window).intersect(b.rect(window)).area() == 0.0,
                        "{a:?} overlaps {b:?} in {size:?}"
                    );
                }
            }
            assert!(ResizeEdge::NorthWest.rect(window).contains(pos2(1.0, 1.0)));
            // A square half the shorter side wide stays content, not resize.
            let middle =
                Rect::from_center_size(window.center(), egui::Vec2::splat(size.min_elem() / 2.0));
            assert!(
                ResizeEdge::ALL
                    .iter()
                    .all(|edge| !edge.rect(window).intersects(middle)),
                "resize edges cover the middle of {size:?}"
            );
        }
    }
}
