//! Native window state and desktop sync: minimize, close, always-on-top, groups,
//! body drags and placement.

use std::time::Duration;

use eframe::egui::{self, Pos2, Rect, Vec2, ViewportCommand, pos2};

use super::{
    ActionOutcome, DETACH_OFFSET_POINTS, DETACH_TIMEOUT, EMPTY_WINDOW_MIN_SIZE,
    INITIAL_WINDOW_SIZE, WindowId, WindowState, Workspace, WorkspaceError, desktop, layout::Layout,
    moving::MoveRequest,
};

impl Workspace {
    /// Report whether a native window is minimized. Instances in a minimized window
    /// may stop work that only serves the display; their analysis keeps running.
    pub(crate) fn set_window_minimized(&mut self, id: WindowId, minimized: bool) {
        if let Some(window) = self.windows.get_mut(&id)
            && window.minimized != minimized
        {
            window.minimized = minimized;
            self.sync_visibility();
        }
    }

    pub(super) fn close_window_action(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        self.window(window_id)?;
        if self.windows.len() == 1 {
            // Closing the last window ends the session: keep its layout.
            self.autosave();
        }
        self.close_window(window_id)?;
        Ok(ActionOutcome::Applied)
    }

    /// An empty window the size of `window_id` (its restored size is unknown while
    /// maximized: then the default size), down-right of it by the detach offset.
    /// Always-on-top carries over as for a detached window; lock and group do not.
    /// The settings window, when open, moves on to the new window's Window tab.
    pub(super) fn new_window(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let source = self.window(window_id)?;
        let size = if source.maximized {
            INITIAL_WINDOW_SIZE
        } else {
            source.content_size
        }
        .max(EMPTY_WINDOW_MIN_SIZE);
        let ppp = source.pixels_per_point;
        let position = source.outer.map(|outer| {
            let physical = outer.min + Vec2::splat(DETACH_OFFSET_POINTS * ppp);
            pos2(physical.x / ppp, physical.y / ppp)
        });
        let mut window = WindowState::new(Layout::default(), size, source.always_on_top);
        window.initial_position = position;
        window.pixels_per_point = ppp;
        let new_id = WindowId(Self::allocate(&mut self.next_window_id)?);
        self.windows.insert(new_id, window);
        self.retarget_settings(new_id);
        Ok(ActionOutcome::WindowCreated(new_id))
    }

    pub(super) fn set_always_on_top(
        &mut self,
        window_id: WindowId,
        enabled: bool,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window = self.window_mut(window_id)?;
        window.always_on_top = enabled;
        window
            .commands
            .push(ViewportCommand::WindowLevel(if enabled {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }));
        Ok(ActionOutcome::Applied)
    }

    /// Lock or unlock a window. Locking ends a divider drag in it, back to where the
    /// drag started.
    pub(super) fn set_locked(
        &mut self,
        window_id: WindowId,
        enabled: bool,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window = self.window_mut(window_id)?;
        window.locked = enabled;
        if enabled && let Some(drag) = window.ui.divider_drag.take() {
            let _ = window
                .layout
                .set_ratio(drag.split_id, drag.start_ratio, false);
        }
        Ok(ActionOutcome::Applied)
    }

    /// A body drag in this window may not start a native move: the window is
    /// locked, or a member of its group is (a group moves as one).
    pub(super) fn drag_locked(&self, window_id: WindowId) -> bool {
        let Some(window) = self.windows.get(&window_id) else {
            return false;
        };
        window.locked
            || window.group.is_some_and(|group| {
                self.windows
                    .values()
                    .any(|other| other.group == Some(group) && other.locked)
            })
    }

    pub(super) fn toggle_maximized(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window = self.window_mut(window_id)?;
        if window.locked {
            return Err(WorkspaceError::Unsupported("maximizing a locked window"));
        }
        let maximized = !window.maximized;
        window.commands.push(ViewportCommand::Maximized(maximized));
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn leave_group(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let group = self
            .window(window_id)?
            .group
            .ok_or(WorkspaceError::Unsupported("leave group without a group"))?;
        self.window_mut(window_id)?.group = None;
        let remaining: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, window)| window.group == Some(group))
            .map(|(id, _)| *id)
            .collect();
        if remaining.len() < 2 {
            for id in remaining {
                if let Some(window) = self.windows.get_mut(&id) {
                    window.group = None;
                }
            }
        }
        Ok(ActionOutcome::Applied)
    }

    pub(super) fn close_window(&mut self, window_id: WindowId) -> Result<(), WorkspaceError> {
        let window = self
            .windows
            .remove(&window_id)
            .ok_or(WorkspaceError::StaleWindow(window_id))?;
        for instance in window.layout.leaves() {
            if self.move_source == Some(instance) {
                self.move_source = None;
            }
            self.modules.remove(&instance);
        }
        if let Some(group) = window.group {
            let remaining: Vec<WindowId> = self
                .windows
                .iter()
                .filter(|(_, other)| other.group == Some(group))
                .map(|(id, _)| *id)
                .collect();
            if remaining.len() < 2 {
                for id in remaining {
                    if let Some(other) = self.windows.get_mut(&id) {
                        other.group = None;
                    }
                }
            }
        }
        Ok(())
    }

    /// Outer frame of a window as the desktop has it now, else as its last pass
    /// saw it. Other windows' passes may not have run since the desktop moved them.
    pub(super) fn frame_of(&self, window_id: WindowId) -> Option<Rect> {
        self.desktop
            .frame(window_id)
            .or_else(|| self.windows.get(&window_id)?.outer)
    }

    /// Windows touching `window_id` on the desktop; fails while its position is
    /// not known yet.
    pub(super) fn touching_windows(
        &self,
        window_id: WindowId,
    ) -> Result<Vec<WindowId>, WorkspaceError> {
        self.window(window_id)?;
        let outer = self.frame_of(window_id).ok_or(WorkspaceError::Unsupported(
            "group before the window position is known",
        ))?;
        Ok(self
            .windows
            .keys()
            .filter(|id| {
                **id != window_id
                    && self
                        .frame_of(**id)
                        .is_some_and(|other| desktop::touching(outer, other))
            })
            .copied()
            .collect())
    }

    pub(super) fn group_touching(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let touching = self.touching_windows(window_id)?;
        if touching.is_empty() {
            return Err(WorkspaceError::Unsupported(
                "group without a touching window",
            ));
        }
        let group = match self.window(window_id)?.group {
            Some(group) => group,
            None => Self::allocate(&mut self.next_group_id)?,
        };
        let mut merged: Vec<u64> = touching
            .iter()
            .filter_map(|id| self.windows.get(id).and_then(|window| window.group))
            .collect();
        merged.push(group);
        for (id, window) in &mut self.windows {
            if *id == window_id
                || touching.contains(id)
                || window.group.is_some_and(|g| merged.contains(&g))
            {
                window.group = Some(group);
            }
        }
        Ok(ActionOutcome::Applied)
    }

    /// Advance time-based state shared by all windows (detach timeouts, the status
    /// badge's messages). Every pass that runs (the host's logic, each drawn window,
    /// the settings window) ticks, so the clock also moves while the windows involved
    /// are minimized.
    pub(crate) fn tick(&mut self, display_time: Duration) {
        self.display_time = self.display_time.max(display_time);
        let now = self.display_time;
        if self
            .control_error
            .as_ref()
            .is_some_and(|(_, at)| now.saturating_sub(*at) >= super::MESSAGE_LIFETIME)
        {
            self.control_error = None;
        }
        let expired: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, window)| {
                window.pending_detach.is_some_and(|pending| {
                    !window.opened
                        && (now.saturating_sub(pending.requested_at) >= DETACH_TIMEOUT
                            || !self.modules.contains_key(&pending.instance_id)
                            || !self.windows.contains_key(&pending.source))
                })
            })
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.windows.remove(&id);
            self.set_control_error("detach canceled: the new window did not open".into());
        }
    }

    /// A body drag in `window_id`, pressed at `press` (logical, in the window):
    /// the window and its group follow the pointer until the button is released.
    /// A maximized window, or one the desktop does not track, gets the OS move
    /// loop instead (the OS restores a maximized window as it moves).
    pub(super) fn start_move(&mut self, window_id: WindowId, press: Pos2) {
        let Some(window) = self.windows.get(&window_id) else {
            return;
        };
        if window.maximized || !window.tracked {
            self.windows
                .get_mut(&window_id)
                .expect("checked above")
                .commands
                .push(ViewportCommand::StartDrag);
            return;
        }
        let ppp = window.pixels_per_point;
        let group = window.group;
        let movable = |other: &WindowState| other.tracked && !other.maximized && !other.minimized;
        let mut members = Vec::new();
        let mut others = Vec::new();
        for (id, other) in &self.windows {
            if *id == window_id || (group.is_some() && other.group == group && movable(other)) {
                members.push(*id);
            } else if movable(other) {
                others.push(*id);
            }
        }
        let request = MoveRequest {
            leader: window_id,
            grab: press.to_vec2() * ppp,
            members,
            others,
            threshold: desktop::SNAP_DISTANCE_POINTS * ppp,
            min_visible: desktop::MIN_VISIBLE_POINTS * ppp,
        };
        if !self.desktop.begin_move(request)
            && let Some(window) = self.windows.get_mut(&window_id)
        {
            window.commands.push(ViewportCommand::StartDrag);
        }
    }

    /// Apply one window's observed native placement. Returns whether the host must
    /// run another pass: other windows got position commands to deliver, or this
    /// window moved and the others draw the edges they share with it.
    pub(super) fn observe_placement(
        &mut self,
        window_id: WindowId,
        outer_points: Option<Rect>,
        pixels_per_point: f32,
        maximized: bool,
        minimized: bool,
    ) -> bool {
        let probe = std::sync::Arc::clone(&self.desktop);
        let Some(window) = self.windows.get_mut(&window_id) else {
            return false;
        };
        window.maximized = maximized;
        window.pixels_per_point = pixels_per_point;
        if !window.tracked {
            window.tracked = probe.attach(window_id);
        }
        let Some(outer_points) = outer_points else {
            return false;
        };
        // Whole physical pixels: the native frame, without the logical round trip.
        let outer = Rect::from_min_max(
            (outer_points.min * pixels_per_point).round(),
            (outer_points.max * pixels_per_point).round(),
        );
        let previous = window.outer.replace(outer);
        let changed = previous != Some(outer);
        if maximized || minimized {
            // The OS owns the placement of maximized and minimized windows.
            if previous.is_none() {
                window.restore_target = None;
            }
            return changed;
        }
        // The user places the window (or corrects it during a drag): wait for the
        // release, and the saved position no longer applies.
        if probe.primary_button_down() {
            if previous.is_some() && changed {
                window.restore_target = None;
            }
            return changed;
        }
        if probe.moving() {
            return changed;
        }
        let moves = self.placement_moves(window_id, outer, pixels_per_point, probe.as_ref());
        self.place_windows(&moves) || changed
    }

    /// Where windows must go after `window_id` was seen at `outer`: the saved
    /// position still to land on (once), or, when it shows too little of itself on
    /// the nearest monitor, its group moved as one into that monitor's work area.
    fn placement_moves(
        &mut self,
        window_id: WindowId,
        outer: Rect,
        pixels_per_point: f32,
        probe: &dyn desktop::DesktopProbe,
    ) -> Vec<(WindowId, Pos2)> {
        let window = self.windows.get_mut(&window_id).expect("observed above");
        if let Some(target) = window
            .restore_target
            .take()
            .filter(|target| (*target - outer.min).length() > desktop::TOUCH_TOLERANCE_PX)
        {
            return vec![(window_id, target)];
        }
        let Some(work) = probe.work_area(outer.center()) else {
            return Vec::new();
        };
        if desktop::reachable(outer, work, desktop::MIN_VISIBLE_POINTS * pixels_per_point) {
            return Vec::new();
        }
        let group = window.group;
        let members: Vec<(WindowId, Rect)> = self
            .windows
            .iter()
            .filter(|(id, other)| {
                **id != window_id
                    && group.is_some()
                    && other.group == group
                    && !other.maximized
                    && !other.minimized
            })
            .filter_map(|(id, _)| self.frame_of(*id).map(|frame| (*id, frame)))
            .chain(std::iter::once((window_id, outer)))
            .collect();
        let bounds = members
            .iter()
            .fold(outer, |bounds, (_, frame)| bounds.union(*frame));
        let shift = desktop::fit_inside(bounds, work) - bounds.min;
        if shift == Vec2::ZERO {
            return Vec::new();
        }
        members
            .into_iter()
            .map(|(id, frame)| (id, frame.min + shift))
            .collect()
    }

    /// Move windows (outer top-left, physical pixels) together without their
    /// groups following. Returns whether some got viewport commands instead (the
    /// desktop does not track them), which need a pass to be sent.
    pub(super) fn place_windows(&mut self, moves: &[(WindowId, Pos2)]) -> bool {
        if moves.is_empty() || self.desktop.place(moves) {
            return false;
        }
        for (id, target) in moves {
            if let Some(window) = self.windows.get_mut(id) {
                let ppp = window.pixels_per_point;
                window
                    .commands
                    .push(ViewportCommand::OuterPosition(*target / ppp));
            }
        }
        true
    }

    /// Tell the desktop which windows move with their group, when that changed:
    /// grouped windows that are neither maximized nor minimized.
    pub(super) fn publish_groups(&mut self) {
        let current = || {
            self.windows.iter().filter_map(|(id, window)| {
                window
                    .group
                    .filter(|_| !window.maximized && !window.minimized)
                    .map(|group| (*id, group))
            })
        };
        if current().eq(self.published_groups.iter().copied()) {
            return;
        }
        let groups: Vec<(WindowId, u64)> = current().collect();
        self.desktop.set_groups(&groups);
        self.published_groups = groups;
    }
}
