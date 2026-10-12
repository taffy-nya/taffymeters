//! One window's UI body. Production viewports and the automation harness both call
//! [`Workspace::window_ui`]; every user operation goes through `apply_action`.

use std::time::Duration;

use eframe::egui::{
    self, Align2, Color32, CursorIcon, FontId, Id, PointerButton, Rect, Response, Sense, Stroke,
    StrokeKind, Vec2, ViewportCommand, pos2,
};

use super::{
    ModuleInstanceId, MoveTarget, Side, SplitId, WindowId, Workspace, WorkspaceAction,
    desktop::DesktopProbe,
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

/// Width of the right / bottom split entries, in logical points.
pub(crate) const SPLIT_ZONE: f32 = 20.0;
/// Thickness of the native resize edges and size of the corner squares.
pub(crate) const RESIZE_EDGE: f32 = 6.0;
pub(crate) const RESIZE_CORNER: f32 = 12.0;
/// Poll interval for state that changes without new audio data or input.
const STATE_POLL: Duration = Duration::from_millis(250);
/// Poll interval while a native move may still be in progress.
const MOVE_POLL: Duration = Duration::from_millis(50);

/// egui color of a theme color.
pub(crate) fn color32(color: crate::render::Color) -> Color32 {
    Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, color.alpha)
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

    /// Non-overlapping hit rect: corners are squares, edges stop at the corners.
    fn rect(self, window: Rect) -> Rect {
        let (min, max) = (window.min, window.max);
        let (b, c) = (RESIZE_EDGE, RESIZE_CORNER);
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
    start_ratio: f32,
    current_ratio: f32,
    pixels_per_point: f32,
    cancelled: bool,
}

#[derive(Default)]
pub(crate) struct WindowUiState {
    pub(crate) divider_drag: Option<DividerDrag>,
    pub(crate) display_time: Duration,
    min_size_sent: Option<Vec2>,
    #[cfg(any(test, feature = "automation"))]
    targets: std::collections::HashMap<ControlKey, ControlTarget>,
    #[cfg(any(test, feature = "automation"))]
    frame_token: u64,
    #[cfg(any(test, feature = "automation"))]
    pass_token: u64,
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

fn split_zone(rect: Rect, side: Side) -> Rect {
    match side {
        Side::Right => Rect::from_min_max(pos2(rect.max.x - SPLIT_ZONE, rect.min.y), rect.max),
        _ => Rect::from_min_max(
            pos2(rect.min.x, rect.max.y - SPLIT_ZONE),
            pos2(rect.max.x - SPLIT_ZONE, rect.max.y),
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
        SourceState::Starting => "STARTING",
        SourceState::Running => "RUNNING",
        SourceState::Paused => "PAUSED",
        SourceState::DeviceLost => "DEVICE LOST",
        SourceState::Stopped => "STOPPED",
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
        probe: &dyn DesktopProbe,
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
        if ctx.global_style().visuals.dark_mode != self.chrome.dark {
            ctx.set_visuals(if self.chrome.dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            });
        }
        ui.painter()
            .rect_filled(rect, 0.0, color32(self.chrome.color(ColorRole::Background)));
        let windows_before: Vec<WindowId> = self.windows.keys().copied().collect();
        let settings_before = self.settings.is_some();
        self.tick(display_time);
        let repaint = self.window_pass(window_id, ui, rect, display_time, probe);
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
        probe: &dyn DesktopProbe,
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
        let wake = self.observe_placement(
            window_id,
            outer,
            pixels_per_point,
            maximized,
            minimized,
            probe,
        );
        if !wake.is_empty() {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        let window = self.windows.get_mut(&window_id).expect("checked above");
        if window.desktop.moving {
            ctx.request_repaint_after_for(MOVE_POLL, egui::ViewportId::ROOT);
        }
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

        self.handle_shortcuts(window_id, ui);
        if !self.windows.contains_key(&window_id) {
            return None;
        }

        let mut controls = Controls::default();
        let mut actions = Vec::new();
        let mut repaint = None;
        if self.windows[&window_id].layout.is_empty() {
            self.empty_window(window_id, ui, rect, &mut controls, &mut actions);
        } else {
            repaint = self.layout_ui(
                window_id,
                ui,
                rect,
                display_time,
                maximized,
                &mut controls,
                &mut actions,
            );
        }
        if self.move_source.is_some() {
            self.move_targets(window_id, ui, rect, &mut controls, &mut actions);
        }
        if !maximized {
            Self::resize_edges(ui, rect, &mut controls);
        }
        self.status_badge(window_id, ui, rect);

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
        let (escape, close, space, mono, one, two) = ui.input(|input| {
            (
                input.key_pressed(egui::Key::Escape),
                input.modifiers.command && input.key_pressed(egui::Key::W),
                input.key_pressed(egui::Key::Space),
                input.key_pressed(egui::Key::M),
                input.key_pressed(egui::Key::Num1),
                input.key_pressed(egui::Key::Num2),
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
            } else {
                let _ = self.apply_action(WorkspaceAction::CloseWindow { window_id });
                return;
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
        let target = self.windows[&window_id].active.filter(|id| {
            self.modules
                .get(id)
                .is_some_and(|entry| entry.module.channel().is_some())
        });
        if let Some(instance_id) = target {
            let layout = self.services.source.descriptor().channel_layout;
            let channels = self.services.source.descriptor().channel_count;
            let mut channel = None;
            if mono {
                channel = Some(ChannelSelection::MonoAverage);
            }
            if one {
                channel = Some(channel_for_index(layout, 0));
            }
            if two && channels > 1 {
                channel = Some(channel_for_index(layout, 1));
            }
            if let Some(channel) = channel {
                let _ = self.apply_action(WorkspaceAction::SetChannel {
                    instance_id,
                    channel,
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_ui(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        display_time: Duration,
        maximized: bool,
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
        let popup_open = egui::Popup::is_any_open(&ctx);

        // Leaf bodies first: everything registered later sits on top of them.
        for &(leaf, leaf_rect) in &geometry.leaves {
            let body = ui.interact(
                leaf_rect,
                Id::new(("taffymeters.leaf", window_id.0, leaf.0)),
                Sense::click_and_drag(),
            );
            controls.record(ControlKey::LeafBody(leaf), &body);
            if body.clicked() {
                actions.push(WorkspaceAction::SelectModule { instance_id: leaf });
            }
            if body.drag_started_by(PointerButton::Primary)
                && !picking
                && let Some(window) = self.windows.get_mut(&window_id)
            {
                window.commands.push(ViewportCommand::StartDrag);
                window.desktop.native_drag = true;
            }
            if !picking && body.secondary_clicked() {
                // Right click edits the module in the settings window.
                actions.push(WorkspaceAction::SelectModule { instance_id: leaf });
                actions.push(WorkspaceAction::OpenSettings {
                    window_id,
                    instance_id: Some(leaf),
                });
            }
            // The Module only sees the pointer over its own content, not over the
            // window chrome drawn on top of it (split entries, dividers, resize edges).
            let pointer = (body.contains_pointer() && !popup_open && !picking)
                .then(|| ui.input(|input| input.pointer.hover_pos()))
                .flatten()
                .filter(|position| {
                    leaf_rect.contains(*position)
                        && !split_zone(leaf_rect, Side::Right).contains(*position)
                        && !split_zone(leaf_rect, Side::Below).contains(*position)
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
                    renderer: &mut renderer,
                });
                if let Some(delay) = response.repaint_after {
                    repaint = Some(repaint.map_or(delay, |current: Duration| current.min(delay)));
                }
            }
        }

        // Split entries.
        if !picking {
            let dragging = ui.input(|input| input.pointer.is_decidedly_dragging());
            for &(leaf, leaf_rect) in &geometry.leaves {
                for side in [Side::Right, Side::Below] {
                    let zone = split_zone(leaf_rect, side);
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
                    let allowed = self.can_split(leaf, side);
                    if response.hovered() && allowed && !dragging {
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
        }

        self.dividers(window_id, ui, &geometry.dividers, controls, actions);

        if self.windows[&window_id].group.is_some() {
            ui.painter().rect_stroke(
                rect,
                0.0,
                Stroke::new(1.0_f32, color32(self.chrome.color(ColorRole::Accent))),
                StrokeKind::Inside,
            );
        }
        repaint
    }

    fn dividers(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        dividers: &[DividerGeometry],
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let pixels_per_point = ui.ctx().pixels_per_point();
        let grid = !ui.input(|input| input.modifiers.alt);
        let mut drag_seen = false;
        for divider in dividers {
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

    fn empty_window(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let picking = self.move_source.is_some();
        let body = ui.interact(
            rect,
            Id::new(("taffymeters.empty", window_id.0)),
            Sense::click_and_drag(),
        );
        controls.record(ControlKey::EmptyBody, &body);
        if body.drag_started_by(PointerButton::Primary)
            && !picking
            && let Some(window) = self.windows.get_mut(&window_id)
        {
            window.commands.push(ViewportCommand::StartDrag);
        }
        if self.windows[&window_id].pending_detach.is_some() {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Opening…",
                FontId::proportional(14.0),
                Color32::GRAY,
            );
            return;
        }
        if picking {
            return;
        }
        let button_rect = Rect::from_center_size(rect.center(), egui::vec2(140.0, 28.0));
        let button = ui
            .scope_builder(egui::UiBuilder::new().max_rect(button_rect), |ui| {
                let response = ui.menu_button("Add module", |ui| {
                    self.add_menu(ui, controls, actions, window_id);
                });
                controls.record(ControlKey::AddOpen, &response.response);
                response.response.secondary_clicked()
            })
            .inner;
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

    /// Modal target overlay of "Move to…": every leaf offers four edge targets and an
    /// empty window offers itself. Escape or a right click cancels.
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
                painter.rect_filled(rect, 0.0, Color32::from_black_alpha(90));
                let mut targets: Vec<(MoveTarget, Rect)> = Vec::new();
                if empty {
                    targets.push((MoveTarget::EmptyWindow(window_id), rect.shrink(8.0)));
                }
                for &(leaf, leaf_rect) in &geometry.leaves {
                    if leaf == source {
                        painter.rect_stroke(
                            leaf_rect.shrink(2.0),
                            0.0,
                            Stroke::new(2.0_f32, color32(self.chrome.color(ColorRole::Accent))),
                            StrokeKind::Inside,
                        );
                        painter.text(
                            leaf_rect.center(),
                            Align2::CENTER_CENTER,
                            "Moving this module",
                            FontId::proportional(14.0),
                            Color32::WHITE,
                        );
                        continue;
                    }
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
                    let accent = color32(self.chrome.color(ColorRole::Accent));
                    let fill = match (valid, response.hovered()) {
                        (true, true) => accent.gamma_multiply(0.7),
                        (true, false) => accent.gamma_multiply(0.25),
                        (false, _) => Color32::from_white_alpha(8),
                    };
                    painter.rect_filled(zone.shrink(2.0), 0.0, fill);
                    if valid && response.clicked() {
                        actions.push(WorkspaceAction::MoveModule {
                            instance_id: source,
                            target,
                        });
                    }
                }
                painter.text(
                    rect.center_top() + egui::vec2(0.0, 12.0),
                    Align2::CENTER_TOP,
                    "Move: click an edge target · Esc or right click cancels",
                    FontId::proportional(13.0),
                    Color32::WHITE,
                );
            });
        if ui.input(|input| input.pointer.secondary_clicked()) {
            actions.push(WorkspaceAction::CancelMove);
        }
    }

    fn status_badge(&self, window_id: WindowId, ui: &egui::Ui, rect: Rect) {
        let status = self.source_status();
        let mut text = String::new();
        if status.state != SourceState::Running {
            text = source_status_text(&status);
        }
        if let Some(error) = &self.control_error {
            if !text.is_empty() {
                text.push_str(" | ");
            }
            text.push_str(error);
        }
        for notice in &self.notices {
            if !text.is_empty() {
                text.push_str(" | ");
            }
            text.push_str(notice);
        }
        if text.is_empty() || !self.windows.contains_key(&window_id) {
            return;
        }
        let painter = ui.painter_at(rect);
        let galley = painter.layout_no_wrap(
            text,
            FontId::proportional(12.0),
            Color32::from_rgb(255, 190, 120),
        );
        let origin = pos2(rect.min.x + 8.0, rect.max.y - galley.size().y - 8.0);
        painter.rect_filled(
            Rect::from_min_size(origin, galley.size()).expand(4.0),
            3.0,
            Color32::from_black_alpha(200),
        );
        painter.galley(origin, galley, Color32::WHITE);
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
            "Source: DEVICE LOST | dropped 3 | invalid 7 | endpoint unavailable"
        );
    }

    #[test]
    fn resize_hit_rects_do_not_overlap_and_corners_win() {
        let window = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
        for (index, a) in ResizeEdge::ALL.iter().enumerate() {
            for b in &ResizeEdge::ALL[index + 1..] {
                assert!(
                    !a.rect(window).intersects(b.rect(window))
                        || a.rect(window).intersect(b.rect(window)).area() == 0.0,
                    "{a:?} overlaps {b:?}"
                );
            }
        }
        assert!(ResizeEdge::NorthWest.rect(window).contains(pos2(2.0, 2.0)));
    }
}
