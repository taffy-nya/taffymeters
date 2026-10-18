//! Moving windows with the pointer. A body drag starts a [`MoveSession`]: the
//! dragged window and its group follow the pointer together, snapped to nearby
//! window and work-area edges (unless Alt is held) and kept on the desktop.
//! [`WindowTracker`] is what the native layer knows of the Workspace windows
//! (frames, groups, the move in progress); it is pure, so the Win32 subclass and
//! the harness's simulated desktop drive the same logic.

use std::collections::HashMap;

use eframe::egui::{Pos2, Rect, Vec2};

use super::{WindowId, desktop};

/// What a body drag asks the desktop to move.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MoveRequest {
    /// The dragged window.
    pub(crate) leader: WindowId,
    /// Where the press was, physical pixels from the leader's top-left.
    pub(crate) grab: Vec2,
    /// Windows that move: the leader and the rest of its group.
    pub(crate) members: Vec<WindowId>,
    /// Windows the members snap to.
    pub(crate) others: Vec<WindowId>,
    /// Snap distance, physical pixels.
    pub(crate) threshold: f32,
    /// Each member keeps this much of itself on a monitor, physical pixels.
    pub(crate) min_visible: f32,
}

/// One drag: where the members go as the pointer moves.
#[derive(Debug)]
pub(crate) struct MoveSession {
    leader: WindowId,
    /// Pointer position the members' start frames belong to.
    anchor: Pos2,
    /// Members and their frames at the anchor.
    members: Vec<(WindowId, Rect)>,
    others: Vec<Rect>,
    work_areas: Vec<Rect>,
    threshold: f32,
    min_visible: f32,
    /// Current offset of every member from its start frame.
    offset: Vec2,
}

impl MoveSession {
    /// The offset for the pointer at `cursor`: the pointer's movement, snapped
    /// unless `free`, then shifted so every member stays reachable. Whole pixels,
    /// so members never drift apart.
    fn motion(&mut self, cursor: Pos2, free: bool) -> Vec2 {
        let mut offset = (cursor - self.anchor).round();
        if !free {
            offset += desktop::snap_group(
                self.moved(offset),
                &self.others,
                &self.work_areas,
                self.threshold,
            );
        }
        offset += desktop::keep_reachable(self.moved(offset), &self.work_areas, self.min_visible);
        self.offset = offset.round();
        self.offset
    }

    fn moved(&self, offset: Vec2) -> impl Iterator<Item = Rect> + '_ {
        self.members
            .iter()
            .map(move |(_, frame)| frame.translate(offset))
    }

    fn targets(&self) -> impl Iterator<Item = (WindowId, Pos2)> + '_ {
        self.members
            .iter()
            .map(|(id, frame)| (*id, frame.min + self.offset))
    }

    fn contains(&self, window: WindowId) -> bool {
        self.members.iter().any(|(id, _)| *id == window)
    }

    /// Continue from `cursor` with the members where they are now (a member
    /// changed size: it moved to a monitor of another scale).
    fn reanchor(&mut self, cursor: Pos2, frames: &HashMap<WindowId, Rect>) {
        self.anchor = cursor;
        for (id, frame) in &mut self.members {
            if let Some(now) = frames.get(id) {
                *frame = *now;
            }
        }
        self.offset = Vec2::ZERO;
    }
}

/// Frames, groups and the move in progress, as the native layer sees them.
#[derive(Debug, Default)]
pub(crate) struct WindowTracker {
    /// Outer frames, physical pixels.
    frames: HashMap<WindowId, Rect>,
    /// Group of each window that follows its group's moves.
    groups: HashMap<WindowId, u64>,
    session: Option<MoveSession>,
}

impl WindowTracker {
    pub(crate) fn frame(&self, window: WindowId) -> Option<Rect> {
        self.frames.get(&window).copied()
    }

    /// The window is gone.
    pub(crate) fn forget(&mut self, window: WindowId) {
        self.frames.remove(&window);
        self.groups.remove(&window);
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.contains(window))
        {
            self.session = None;
        }
    }

    pub(crate) fn set_groups(&mut self, groups: &[(WindowId, u64)]) {
        self.groups.clear();
        self.groups.extend(groups.iter().copied());
    }

    pub(crate) fn moving(&self) -> bool {
        self.session.is_some()
    }

    /// The window a move in progress follows the pointer for.
    pub(crate) fn leader(&self) -> Option<WindowId> {
        self.session.as_ref().map(|session| session.leader)
    }

    /// Start moving `request.members`; `false` when the leader is not tracked.
    /// Members and others without a known frame are left out.
    pub(crate) fn begin(&mut self, request: &MoveRequest, work_areas: Vec<Rect>) -> bool {
        let Some(leader) = self.frame(request.leader) else {
            return false;
        };
        let mut members: Vec<(WindowId, Rect)> = request
            .members
            .iter()
            .filter_map(|id| self.frame(*id).map(|frame| (*id, frame)))
            .collect();
        if !members.iter().any(|(id, _)| *id == request.leader) {
            members.push((request.leader, leader));
        }
        let others = request
            .others
            .iter()
            .filter(|id| !request.members.contains(id))
            .filter_map(|id| self.frame(*id))
            .collect();
        self.session = Some(MoveSession {
            leader: request.leader,
            anchor: leader.min + request.grab,
            members,
            others,
            work_areas,
            threshold: request.threshold,
            min_visible: request.min_visible,
            offset: Vec2::ZERO,
        });
        true
    }

    /// The pointer is at `cursor` (Alt held: `free`): where each member goes,
    /// into `out`.
    pub(crate) fn motion(&mut self, cursor: Pos2, free: bool, out: &mut Vec<(WindowId, Pos2)>) {
        if let Some(session) = &mut self.session {
            session.motion(cursor, free);
            out.extend(session.targets());
        }
    }

    /// Escape: the members go back to where the move started (into `out`) and the
    /// move ends.
    pub(crate) fn cancel(&mut self, out: &mut Vec<(WindowId, Pos2)>) {
        if let Some(session) = &mut self.session {
            session.offset = Vec2::ZERO;
            out.extend(session.targets());
        }
        self.session = None;
    }

    /// The button was released: the members stay where they are.
    pub(crate) fn end(&mut self) {
        self.session = None;
    }

    /// A tracked window's frame is now `frame`. `ours` when this desktop moved it.
    /// A member of the move in progress that changes size (a monitor of another
    /// scale) re-anchors the move at `cursor`; members otherwise only move with
    /// the move. Any other pure move of a grouped window, by the user, the OS or
    /// another program, moves the rest of its group as far (into `out`).
    pub(crate) fn observed(
        &mut self,
        window: WindowId,
        frame: Rect,
        ours: bool,
        cursor: Pos2,
        out: &mut Vec<(WindowId, Pos2)>,
    ) {
        let previous = self.frames.insert(window, frame);
        if let Some(session) = &mut self.session
            && session.contains(window)
        {
            if previous.is_some_and(|previous| previous.size() != frame.size()) {
                session.reanchor(cursor, &self.frames);
            }
            return;
        }
        let Some(previous) = previous else {
            return;
        };
        let delta = frame.min - previous.min;
        if ours || previous.size() != frame.size() || delta == Vec2::ZERO {
            return;
        }
        let Some(group) = self.groups.get(&window).copied() else {
            return;
        };
        for (other, other_group) in &self.groups {
            if *other != window
                && *other_group == group
                && let Some(frame) = self.frames.get(other)
            {
                out.push((*other, frame.min + delta));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use eframe::egui::{pos2, vec2};

    use super::*;

    const A: WindowId = WindowId(1);
    const B: WindowId = WindowId(2);
    const C: WindowId = WindowId(3);

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    /// A and B side by side (grouped), C alone below; one 1920×1040 work area.
    fn desktop() -> WindowTracker {
        let mut tracker = WindowTracker::default();
        let mut out = Vec::new();
        for (id, frame) in [
            (A, rect(300.0, 200.0, 320.0, 160.0)),
            (B, rect(620.0, 200.0, 320.0, 160.0)),
            (C, rect(300.0, 600.0, 320.0, 160.0)),
        ] {
            tracker.observed(id, frame, true, Pos2::ZERO, &mut out);
        }
        tracker.set_groups(&[(A, 1), (B, 1)]);
        assert!(out.is_empty());
        tracker
    }

    fn work() -> Vec<Rect> {
        vec![rect(0.0, 0.0, 1920.0, 1040.0)]
    }

    fn request(leader: WindowId, members: &[WindowId], others: &[WindowId]) -> MoveRequest {
        MoveRequest {
            leader,
            grab: vec2(100.0, 50.0),
            members: members.to_vec(),
            others: others.to_vec(),
            threshold: 12.0,
            min_visible: 32.0,
        }
    }

    /// Move to `cursor` and apply the targets as the native layer would.
    fn drive(tracker: &mut WindowTracker, cursor: Pos2, free: bool) -> Vec<(WindowId, Pos2)> {
        let mut out = Vec::new();
        tracker.motion(cursor, free, &mut out);
        apply(tracker, &out);
        out
    }

    fn apply(tracker: &mut WindowTracker, moves: &[(WindowId, Pos2)]) {
        let mut echo = Vec::new();
        for (id, position) in moves {
            let frame = tracker.frame(*id).unwrap();
            tracker.observed(
                *id,
                frame.translate(*position - frame.min),
                true,
                Pos2::ZERO,
                &mut echo,
            );
        }
        assert!(echo.is_empty(), "our own moves were passed on: {echo:?}");
    }

    #[test]
    fn a_group_follows_the_pointer_as_one_and_returns_exactly() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(A, &[A, B], &[C]), work()));
        let grab = pos2(400.0, 250.0);
        // Many fractional moves far from every edge, then back to the start.
        let mut seed = 7_u32;
        for _ in 0..1000 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let dx = (seed >> 8) % 900;
            let dy = (seed >> 20) % 300;
            let cursor = pos2(
                grab.x + 200.0 + dx as f32 + 0.37,
                grab.y + 300.0 + dy as f32 - 0.61,
            );
            drive(&mut tracker, cursor, true);
            let (a, b) = (tracker.frame(A).unwrap(), tracker.frame(B).unwrap());
            assert_eq!(b.min - a.min, vec2(320.0, 0.0));
            assert_eq!(a.min, a.min.round());
        }
        drive(&mut tracker, grab, true);
        assert_eq!(tracker.frame(A).unwrap().min, pos2(300.0, 200.0));
        assert_eq!(tracker.frame(B).unwrap().min, pos2(620.0, 200.0));
    }

    #[test]
    fn snapping_sticks_within_the_threshold_and_alt_moves_freely() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(C, &[C], &[A, B]), work()));
        let grab = pos2(400.0, 650.0);
        // C's top 10 px below A's bottom (360) and 6 px right of A's left edge.
        let near = grab + vec2(6.0, 370.0 - 600.0);
        drive(&mut tracker, near, false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
        // Still within the threshold: stays snapped.
        drive(&mut tracker, near + vec2(5.0, 1.0), false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
        // Exactly at the threshold still snaps; beyond it is free.
        drive(&mut tracker, grab + vec2(12.0, 372.0 - 600.0), false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
        drive(&mut tracker, grab + vec2(13.0, 373.0 - 600.0), false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(313.0, 373.0));
        // Alt: exactly where the pointer puts it, then snapped again without it.
        drive(&mut tracker, near, true);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(306.0, 370.0));
        drive(&mut tracker, near, false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
    }

    #[test]
    fn every_member_snaps_and_the_group_stays_on_the_desktop() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(A, &[A, B], &[C]), work()));
        let grab = pos2(400.0, 250.0);
        // B's right edge (940) to 8 px short of the work area's right edge.
        drive(&mut tracker, grab + vec2(1920.0 - 8.0 - 940.0, 0.0), false);
        assert_eq!(tracker.frame(B).unwrap().max.x, 1920.0);
        assert_eq!(
            tracker.frame(A).unwrap().max.x,
            tracker.frame(B).unwrap().min.x
        );
        // Far past the right edge: B keeps 32 px on screen, A follows.
        drive(&mut tracker, grab + vec2(3000.0, 0.0), false);
        assert_eq!(tracker.frame(B).unwrap().min.x, 1920.0 - 32.0);
        assert_eq!(
            tracker.frame(B).unwrap().min.x - tracker.frame(A).unwrap().min.x,
            320.0
        );
        // Past the top-left corner: A keeps 32 px inside on both axes.
        drive(&mut tracker, grab - vec2(3000.0, 3000.0), true);
        let a = tracker.frame(A).unwrap();
        assert_eq!((a.max.x, a.max.y), (32.0, 32.0));
        assert_eq!(tracker.frame(B).unwrap().min - a.min, vec2(320.0, 0.0));
    }

    #[test]
    fn escape_restores_the_start_and_ends_the_move() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(A, &[A, B], &[C]), work()));
        drive(&mut tracker, pos2(900.0, 700.0), false);
        let mut out = Vec::new();
        tracker.cancel(&mut out);
        apply(&mut tracker, &out);
        assert!(!tracker.moving());
        assert_eq!(tracker.frame(A).unwrap().min, pos2(300.0, 200.0));
        assert_eq!(tracker.frame(B).unwrap().min, pos2(620.0, 200.0));
    }

    #[test]
    fn moves_by_others_take_the_group_along_but_not_resizes_or_our_own() {
        let mut tracker = desktop();
        let mut out = Vec::new();
        tracker.observed(
            A,
            rect(350.0, 210.0, 320.0, 160.0),
            false,
            Pos2::ZERO,
            &mut out,
        );
        assert_eq!(out, vec![(B, pos2(670.0, 210.0))]);
        out.clear();
        // A resize, our own placement and an ungrouped window: nothing follows.
        tracker.observed(
            A,
            rect(350.0, 210.0, 400.0, 160.0),
            false,
            Pos2::ZERO,
            &mut out,
        );
        tracker.observed(
            A,
            rect(10.0, 10.0, 400.0, 160.0),
            true,
            Pos2::ZERO,
            &mut out,
        );
        tracker.observed(C, rect(0.0, 0.0, 320.0, 160.0), false, Pos2::ZERO, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_member_changing_scale_reanchors_the_move() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(A, &[A, B], &[C]), work()));
        drive(&mut tracker, pos2(500.0, 250.0), true);
        // The OS doubles A (a 2x monitor) while the pointer is at (500, 250).
        let mut out = Vec::new();
        tracker.observed(
            A,
            rect(400.0, 200.0, 640.0, 320.0),
            false,
            pos2(500.0, 250.0),
            &mut out,
        );
        assert!(out.is_empty());
        drive(&mut tracker, pos2(510.0, 260.0), true);
        assert_eq!(tracker.frame(A).unwrap().min, pos2(410.0, 210.0));
        assert_eq!(tracker.frame(B).unwrap().min, pos2(730.0, 210.0));
    }
}
