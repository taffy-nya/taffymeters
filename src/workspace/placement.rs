//! Native window state and desktop sync: minimize, close, always-on-top, groups and placement.

use std::time::Duration;

use eframe::egui::{self, Pos2, Rect, Vec2, ViewportCommand, pos2};

use super::{
    ActionOutcome, DETACH_TIMEOUT, DesktopProbe, WindowId, WindowState, Workspace, WorkspaceError,
    desktop,
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

    pub(super) fn set_pinned(
        &mut self,
        window_id: WindowId,
        enabled: bool,
    ) -> Result<ActionOutcome, WorkspaceError> {
        self.window_mut(window_id)?.pinned = enabled;
        Ok(ActionOutcome::Applied)
    }

    /// A body drag in this window may not start a native move: the window is
    /// pinned, or a member of its group is (a group moves as one).
    pub(super) fn drag_pinned(&self, window_id: WindowId) -> bool {
        let Some(window) = self.windows.get(&window_id) else {
            return false;
        };
        window.pinned
            || window.group.is_some_and(|group| {
                self.windows
                    .values()
                    .any(|other| other.group == Some(group) && other.pinned)
            })
    }

    pub(super) fn toggle_maximized(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let window = self.window_mut(window_id)?;
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

    pub(super) fn group_touching(
        &mut self,
        window_id: WindowId,
    ) -> Result<ActionOutcome, WorkspaceError> {
        let outer = self
            .window(window_id)?
            .desktop
            .outer
            .ok_or(WorkspaceError::Unsupported(
                "group before the window position is known",
            ))?;
        let touching: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(id, window)| {
                **id != window_id
                    && window
                        .desktop
                        .outer
                        .is_some_and(|other| desktop::touching(outer, other))
            })
            .map(|(id, _)| *id)
            .collect();
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

    /// Apply one window's observed native placement. Returns windows that received
    /// position commands and need a pass to deliver them.
    pub(super) fn observe_placement(
        &mut self,
        window_id: WindowId,
        outer_points: Option<Rect>,
        pixels_per_point: f32,
        maximized: bool,
        minimized: bool,
        probe: &dyn DesktopProbe,
    ) -> Vec<WindowId> {
        let mut followers_to_wake = Vec::new();
        let Some(window) = self.windows.get_mut(&window_id) else {
            return followers_to_wake;
        };
        window.maximized = maximized;
        window.pixels_per_point = pixels_per_point;
        let Some(outer_points) = outer_points else {
            return followers_to_wake;
        };
        let outer = Rect::from_min_max(
            outer_points.min * pixels_per_point,
            outer_points.max * pixels_per_point,
        );
        let previous = window.desktop.outer.replace(outer);
        let Some(previous) = previous else {
            // First placement: land on the saved desktop position, or bring a window
            // that opened off every monitor back where it can be used. The OS owns
            // the placement of maximized and minimized windows; during a drag the
            // correction waits for the release (handled below on later passes).
            if maximized || minimized {
                window.restore_target = None;
            }
            let target = (!maximized && !minimized && !probe.primary_button_down())
                .then(|| Self::placement_target(window, outer, pixels_per_point, probe))
                .flatten();
            if let Some(target) = target {
                window.desktop.expected = Some(target);
                window
                    .commands
                    .push(ViewportCommand::OuterPosition(target / pixels_per_point));
                followers_to_wake.push(window_id);
            }
            return followers_to_wake;
        };
        let delta = outer.min - previous.min;
        let button_down = probe.primary_button_down();
        let mut user_delta = None;
        // Maximize, minimize and edge resizes change the frame without being a move:
        // they neither snap nor drag the group along.
        let reframed = maximized || minimized || previous.size() != outer.size();
        if delta.length() > 0.5 && !reframed {
            if window.desktop.native_drag || window.desktop.expected.is_none() {
                // The OS move loop this window started, or an external move.
                window.desktop.expected = None;
                window.desktop.moving = true;
                user_delta = Some(delta);
            } else if window.desktop.expected.is_some_and(|expected| {
                (expected - outer.min).length() <= desktop::TOUCH_TOLERANCE_PX
            }) {
                window.desktop.expected = None;
            }
            // Otherwise an earlier position command is still landing: never echoed
            // back to the group.
        } else if window.desktop.expected.is_some() && !window.desktop.native_drag && !button_down {
            // A command landed elsewhere (the OS adjusted it) and nothing moves now.
            window.desktop.expected = None;
        }
        if !button_down && !window.desktop.moving {
            window.desktop.native_drag = false;
        }
        let mut snap = Vec2::ZERO;
        if window.desktop.moving && !button_down && !maximized && !minimized {
            window.desktop.moving = false;
            window.desktop.native_drag = false;
            let group = window.group;
            let others: Vec<Rect> = self
                .windows
                .iter()
                .filter(|(id, other)| {
                    **id != window_id && (group.is_none() || other.group != group)
                })
                .filter_map(|(_, other)| other.desktop.outer)
                .collect();
            let threshold = desktop::SNAP_DISTANCE_POINTS * pixels_per_point;
            snap = desktop::snap_offset(outer, &others, probe.work_area(outer.center()), threshold);
            if snap != Vec2::ZERO {
                let window = self.windows.get_mut(&window_id).expect("checked above");
                let target = outer.min + snap;
                window.desktop.expected = Some(target);
                window.commands.push(ViewportCommand::OuterPosition(pos2(
                    target.x / pixels_per_point,
                    target.y / pixels_per_point,
                )));
                followers_to_wake.push(window_id);
            }
        }
        let shift = user_delta.unwrap_or(Vec2::ZERO) + snap;
        if shift != Vec2::ZERO
            && let Some(group) = self.windows[&window_id].group
        {
            for (id, follower) in &mut self.windows {
                if *id == window_id || follower.group != Some(group) || follower.maximized {
                    continue;
                }
                let Some(base) = follower
                    .desktop
                    .expected
                    .or(follower.desktop.outer.map(|r| r.min))
                else {
                    continue;
                };
                let target = base + shift;
                let ppp = follower.pixels_per_point;
                follower.desktop.expected = Some(target);
                follower.commands.push(ViewportCommand::OuterPosition(pos2(
                    target.x / ppp,
                    target.y / ppp,
                )));
                followers_to_wake.push(*id);
            }
        }
        // A restore correction that waited for a release, then a window left
        // without a usable area on any monitor (its display was disconnected, or it
        // was restored there): it comes back into the nearest one.
        let window = self.windows.get_mut(&window_id).expect("checked above");
        if user_delta.is_some() {
            // The user placed the window; the saved position no longer applies.
            window.restore_target = None;
        }
        if !button_down
            && !maximized
            && !minimized
            && !window.desktop.moving
            && window.desktop.expected.is_none()
            && let Some(target) = Self::placement_target(window, outer, pixels_per_point, probe)
        {
            window.desktop.expected = Some(target);
            window
                .commands
                .push(ViewportCommand::OuterPosition(target / pixels_per_point));
            followers_to_wake.push(window_id);
        }
        followers_to_wake
    }

    /// Where `outer` (physical pixels) must go to show enough of itself on the
    /// nearest monitor, if it does not already.
    fn rescue_target(outer: Rect, pixels_per_point: f32, probe: &dyn DesktopProbe) -> Option<Pos2> {
        let work = probe.work_area(outer.center())?;
        desktop::reachable_position(outer, work, desktop::MIN_VISIBLE_POINTS * pixels_per_point)
    }

    /// The saved position still to land on (once), otherwise a position that makes
    /// the window reachable again.
    fn placement_target(
        window: &mut WindowState,
        outer: Rect,
        pixels_per_point: f32,
        probe: &dyn DesktopProbe,
    ) -> Option<Pos2> {
        window
            .restore_target
            .take()
            .filter(|target| (*target - outer.min).length() > desktop::TOUCH_TOLERANCE_PX)
            .or_else(|| Self::rescue_target(outer, pixels_per_point, probe))
    }
}
