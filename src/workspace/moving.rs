//! Native and simulated desktops share this tracker. Frames, anchors and offsets
//! are physical desktop pixels; request distances use the logical scales below.

use std::collections::HashMap;

use eframe::egui::{Pos2, Rect, Vec2};

use super::{
    WindowId,
    desktop::{self, Display},
};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MoveRequest {
    pub(crate) leader: WindowId,
    /// Where the press was, physical pixels from the leader's top-left.
    pub(crate) grab: Vec2,
    /// Each member's physical pixels per logical point.
    pub(crate) members: Vec<(WindowId, f32)>,
    pub(crate) others: Vec<WindowId>,
    /// Snap distance, logical points of the leader.
    pub(crate) threshold: f32,
    /// Each member keeps this much of itself on a monitor, its logical points.
    pub(crate) min_visible: f32,
}

#[derive(Clone, Copy, Debug)]
struct Member {
    id: WindowId,
    /// Frame at the anchor.
    frame: Rect,
    /// Top-left when the drag started: where Escape puts it back.
    start: Pos2,
    /// Physical pixels per logical point; follows a change of monitor scale.
    scale: f32,
}

#[derive(Debug)]
pub(crate) struct MoveSession {
    leader: WindowId,
    /// Pointer position the members' anchor frames belong to.
    anchor: Pos2,
    members: Vec<Member>,
    others: Vec<Rect>,
    displays: Vec<Display>,
    /// Logical points, converted at the leader's scale for snapping.
    threshold: f32,
    /// Logical points, converted at each member's scale for reachability.
    min_visible: f32,
    offset: Vec2,
    /// Defer re-anchoring after a size change until the whole move batch has landed.
    reanchor_pending: bool,
}

impl MoveSession {
    /// Round the shared offset to whole pixels so members cannot drift apart.
    fn motion(&mut self, cursor: Pos2, free: bool) -> Vec2 {
        let mut offset = (cursor - self.anchor).round();
        if !free {
            let threshold = self.threshold * self.leader_scale();
            offset +=
                desktop::snap_group(self.moved(offset), &self.others, &self.displays, threshold);
        }
        let min_visible = self.min_visible;
        offset += desktop::keep_reachable(
            self.members
                .iter()
                .map(|member| (member.frame.translate(offset), min_visible * member.scale)),
            &self.displays,
        );
        self.offset = offset.round();
        self.offset
    }

    fn leader_scale(&self) -> f32 {
        self.members
            .iter()
            .find(|member| member.id == self.leader)
            .map_or(1.0, |member| member.scale)
    }

    fn moved(&self, offset: Vec2) -> impl Iterator<Item = Rect> + '_ {
        self.members
            .iter()
            .map(move |member| member.frame.translate(offset))
    }

    fn targets(&self) -> impl Iterator<Item = (WindowId, Pos2)> + '_ {
        self.members
            .iter()
            .map(|member| (member.id, member.frame.min + self.offset))
    }

    fn contains(&self, window: WindowId) -> bool {
        self.members.iter().any(|member| member.id == window)
    }

    /// Frameless windows keep their logical size, so a width change reveals a DPI change.
    fn reanchor(&mut self, cursor: Pos2, frames: &HashMap<WindowId, Rect>) {
        self.anchor = cursor;
        for member in &mut self.members {
            if let Some(now) = frames.get(&member.id) {
                if member.frame.width() > 0.0 && now.width() != member.frame.width() {
                    member.scale *= now.width() / member.frame.width();
                }
                member.frame = *now;
            }
        }
        self.offset = Vec2::ZERO;
        self.reanchor_pending = false;
    }
}

#[derive(Debug, Default)]
pub(crate) struct WindowTracker {
    frames: HashMap<WindowId, Rect>,
    groups: HashMap<WindowId, u64>,
    session: Option<MoveSession>,
}

impl WindowTracker {
    pub(crate) fn frame(&self, window: WindowId) -> Option<Rect> {
        self.frames.get(&window).copied()
    }

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

    pub(crate) fn leader(&self) -> Option<WindowId> {
        self.session.as_ref().map(|session| session.leader)
    }

    pub(crate) fn begin(&mut self, request: &MoveRequest, displays: Vec<Display>) -> bool {
        let Some(leader) = self.frame(request.leader) else {
            return false;
        };
        let member = |id: WindowId, frame: Rect, scale: f32| Member {
            id,
            frame,
            start: frame.min,
            scale,
        };
        let mut members: Vec<Member> = request
            .members
            .iter()
            .filter_map(|(id, scale)| self.frame(*id).map(|frame| member(*id, frame, *scale)))
            .collect();
        if !members.iter().any(|member| member.id == request.leader) {
            members.push(member(request.leader, leader, 1.0));
        }
        let others = request
            .others
            .iter()
            .filter(|id| !request.members.iter().any(|(member, _)| member == *id))
            .filter_map(|id| self.frame(*id))
            .collect();
        self.session = Some(MoveSession {
            leader: request.leader,
            anchor: leader.min + request.grab,
            members,
            others,
            displays,
            threshold: request.threshold,
            min_visible: request.min_visible,
            offset: Vec2::ZERO,
            reanchor_pending: false,
        });
        true
    }

    /// `free` bypasses snapping, not reachability.
    pub(crate) fn motion(&mut self, cursor: Pos2, free: bool, out: &mut Vec<(WindowId, Pos2)>) {
        if let Some(session) = &mut self.session {
            session.motion(cursor, free);
            out.extend(session.targets());
        }
    }

    /// Call only after the entire native move batch has landed.
    pub(crate) fn settle(&mut self, cursor: Pos2) {
        if let Some(session) = &mut self.session
            && session.reanchor_pending
        {
            session.reanchor(cursor, &self.frames);
        }
    }

    pub(crate) fn cancel(&mut self, out: &mut Vec<(WindowId, Pos2)>) {
        if let Some(session) = self.session.take() {
            out.extend(
                session
                    .members
                    .iter()
                    .map(|member| (member.id, member.start)),
            );
        }
    }

    pub(crate) fn end(&mut self) {
        self.session = None;
    }

    /// `ours` suppresses group following for feedback from our own placement.
    /// A moving session member's size change defers re-anchoring until `settle`
    /// during our batch, or re-anchors immediately when the change is external.
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
                if ours {
                    session.reanchor_pending = true;
                } else {
                    session.reanchor(cursor, &self.frames);
                }
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

    // 1920×1080 with a 40 px taskbar at the bottom.
    fn work() -> Vec<Display> {
        vec![Display {
            work: rect(0.0, 0.0, 1920.0, 1040.0),
            bounds: rect(0.0, 0.0, 1920.0, 1080.0),
            scale: 1.0,
            primary: true,
        }]
    }

    fn request(leader: WindowId, members: &[WindowId], others: &[WindowId]) -> MoveRequest {
        MoveRequest {
            leader,
            grab: vec2(100.0, 50.0),
            members: members.iter().map(|id| (*id, 1.0)).collect(),
            others: others.to_vec(),
            threshold: 12.0,
            min_visible: 32.0,
        }
    }

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
        // C's top is 10 px below A's bottom (360), and its left edge is 6 px to the right.
        let near = grab + vec2(6.0, 370.0 - 600.0);
        drive(&mut tracker, near, false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
        drive(&mut tracker, near + vec2(5.0, 1.0), false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
        // The 12 px boundary still snaps; 13 px is outside the threshold.
        drive(&mut tracker, grab + vec2(12.0, 372.0 - 600.0), false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
        drive(&mut tracker, grab + vec2(13.0, 373.0 - 600.0), false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(313.0, 373.0));
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
        // B's right edge (940) ends 8 px short of the work area's right edge.
        drive(&mut tracker, grab + vec2(1920.0 - 8.0 - 940.0, 0.0), false);
        assert_eq!(tracker.frame(B).unwrap().max.x, 1920.0);
        assert_eq!(
            tracker.frame(A).unwrap().max.x,
            tracker.frame(B).unwrap().min.x
        );
        drive(&mut tracker, grab + vec2(3000.0, 0.0), false);
        assert_eq!(tracker.frame(B).unwrap().min.x, 1920.0 - 32.0);
        assert_eq!(
            tracker.frame(B).unwrap().min.x - tracker.frame(A).unwrap().min.x,
            320.0
        );
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

    #[test]
    fn a_scale_change_within_a_batch_reanchors_after_the_whole_batch() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(A, &[A, B], &[C]), work()));
        let cursor = pos2(500.0, 250.0);
        let mut out = Vec::new();
        tracker.motion(cursor, true, &mut out);
        assert_eq!(out, vec![(A, pos2(400.0, 200.0)), (B, pos2(720.0, 200.0))]);
        // Within the batch A lands first and doubles (a 2x monitor), B after it.
        let mut echo = Vec::new();
        tracker.observed(A, rect(400.0, 200.0, 640.0, 320.0), true, cursor, &mut echo);
        tracker.observed(B, rect(720.0, 200.0, 320.0, 160.0), true, cursor, &mut echo);
        tracker.settle(cursor);
        assert!(echo.is_empty());
        drive(&mut tracker, cursor + vec2(10.0, 10.0), true);
        assert_eq!(tracker.frame(A).unwrap().min, pos2(410.0, 210.0));
        assert_eq!(tracker.frame(B).unwrap().min, pos2(730.0, 210.0));
    }

    #[test]
    fn escape_after_a_scale_change_returns_to_where_the_drag_started() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(A, &[A, B], &[C]), work()));
        drive(&mut tracker, pos2(500.0, 250.0), true);
        let mut out = Vec::new();
        tracker.observed(
            A,
            rect(400.0, 200.0, 640.0, 320.0),
            false,
            pos2(500.0, 250.0),
            &mut out,
        );
        drive(&mut tracker, pos2(700.0, 450.0), true);
        tracker.cancel(&mut out);
        assert_eq!(out, vec![(A, pos2(300.0, 200.0)), (B, pos2(620.0, 200.0))]);
        assert!(!tracker.moving());
    }

    #[test]
    fn snapping_follows_the_dragged_window_to_a_monitor_of_another_scale() {
        let mut tracker = desktop();
        assert!(tracker.begin(&request(C, &[C], &[A, B]), work()));
        let grab = pos2(400.0, 650.0);
        // C's top 20 px below A's bottom (360), 6 px right of its left edge:
        // beyond 12 pt at 1x.
        let near = grab + vec2(6.0, 380.0 - 600.0);
        drive(&mut tracker, near, false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(306.0, 380.0));
        // Back at the start, C moves to a 2x monitor (twice the size).
        drive(&mut tracker, grab, false);
        let mut out = Vec::new();
        tracker.observed(C, rect(300.0, 600.0, 640.0, 320.0), false, grab, &mut out);
        // At 2x, the 12-point snap threshold is 24 px, so the same 20 px gap now snaps.
        drive(&mut tracker, near, false);
        assert_eq!(tracker.frame(C).unwrap().min, pos2(300.0, 360.0));
    }
}
