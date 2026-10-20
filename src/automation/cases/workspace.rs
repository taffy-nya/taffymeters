//! W01–W11: Workspace / BSP / multi-window behavior through the real Workspace,
//! Module factories, manual source and egui events. Native window management, real
//! DPI changes and the Win32 window moves are verified separately on a desktop.

use std::time::Duration;

use eframe::egui::{self, Event, PointerButton, Pos2, ViewportCommand, pos2, vec2};
use serde_json::{Value, json};

use crate::{
    dataflow::ChannelSelection,
    module::MIN_EXTENT,
    modules::{PIXEL_SPECTRUM_STYLE_ID, WAVEFORM_MODULE_ID},
    workspace::{
        ActionOutcome, ControlKey, Edge, EdgeLine, FIRST_WINDOW, ModuleInstanceId, MoveTarget,
        ResizeEdge, SettingsItem, Side, SplitId, Tab, WindowId, Workspace, WorkspaceAction,
    },
};

use crate::automation::{
    harness::{Harness, SETTINGS},
    support::{action, add_module, check, created, deadline, first_leaf, wait_until, workspace},
};

type Scenario = fn() -> Result<Value, String>;

pub(super) const CASES: [(&str, Scenario); 12] = [
    ("W01", w01),
    ("W02", w02),
    ("W03", w03),
    ("W04", w04),
    ("W05", w05),
    ("W06", w06),
    ("W07", w07),
    ("W08", w08),
    ("W09", w09),
    ("W10", w10),
    ("W11", w11),
    ("W12", w12),
];

fn split(
    ws: &mut Workspace,
    instance_id: ModuleInstanceId,
    side: Side,
) -> Result<ModuleInstanceId, String> {
    created(action(
        ws,
        WorkspaceAction::SplitModule { instance_id, side },
    )?)
}

fn object(ws: &Workspace, id: ModuleInstanceId) -> Result<usize, String> {
    ws.observation()
        .module(id)
        .map(|module| module.object)
        .ok_or_else(|| format!("instance {} missing", id.0))
}

fn leaves(ws: &Workspace, window: WindowId) -> Vec<ModuleInstanceId> {
    ws.observation()
        .window(window)
        .map(|w| w.leaves.clone())
        .unwrap_or_default()
}

fn root_split(ws: &Workspace, index: usize) -> Result<(SplitId, f32, bool), String> {
    ws.observation()
        .window(FIRST_WINDOW)
        .and_then(|w| w.splits.get(index).map(|s| (s.id, s.ratio, s.manual)))
        .ok_or_else(|| "missing split".to_owned())
}

fn subscriptions_settle(ws: &Workspace, expected: u64) -> Result<(), String> {
    let handle = ws.dataflow_handle_for_automation();
    wait_until(deadline(), &format!("{expected} subscriptions"), || {
        (handle.metrics().subscriptions == expected).then_some(())
    })
}

fn new_window(outcome: ActionOutcome) -> Result<WindowId, String> {
    match outcome {
        ActionOutcome::WindowCreated(id) => Ok(id),
        other => Err(format!("expected a new window, got {other:?}")),
    }
}

/// Split, equalize, manual ratios, close with sibling fill, size rejection.
fn w01() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let a = first_leaf(&h.ws)?;
    let a_object = object(&h.ws, a)?;
    let b = split(&mut h.ws, a, Side::Right)?;
    let c = split(&mut h.ws, b, Side::Right)?;
    h.frame(vec![]);
    let o = h.ws.observation();
    let root = o.window(FIRST_WINDOW).ok_or("root closed")?;
    let widths: Vec<f32> = root.leaf_rects.iter().map(|(_, r)| r.width()).collect();
    check(
        root.leaves == vec![a, b, c] && widths.iter().all(|w| (w - 300.0).abs() < 0.5),
        &format!(
            "untouched chain not equalized: {:?} {widths:?}",
            root.leaves
        ),
    )?;
    for id in [b, c] {
        let module = o.module(id).ok_or("new instance missing")?;
        check(
            module.channel == Some(ChannelSelection::MonoAverage)
                && module.active_style == Some(crate::modules::DEFAULT_SPECTRUM_STYLE_ID)
                && module.type_id == crate::modules::SPECTRUM_MODULE_ID,
            "split did not create a default instance of the same type",
        )?;
    }
    check(
        object(&h.ws, a)? == a_object,
        "split rebuilt the original object",
    )?;
    let (first_split, _, _) = root_split(&h.ws, 0)?;
    action(
        &mut h.ws,
        WorkspaceAction::SetSplitRatio {
            window_id: FIRST_WINDOW,
            split_id: first_split,
            ratio: 0.25,
            manual: true,
        },
    )?;
    let d = split(&mut h.ws, c, Side::Right)?;
    let (kept_id, kept_ratio, kept_manual) = root_split(&h.ws, 0)?;
    check(
        kept_id == first_split && (kept_ratio - 0.25).abs() < 1e-6 && kept_manual,
        "adding a leaf rewrote a manually adjusted ratio",
    )?;
    subscriptions_settle(&h.ws, 4)?;
    let objects: Vec<usize> = [a, c, d]
        .iter()
        .map(|id| object(&h.ws, *id))
        .collect::<Result<_, _>>()?;
    action(&mut h.ws, WorkspaceAction::CloseModule { instance_id: b })?;
    let after: Vec<usize> = [a, c, d]
        .iter()
        .map(|id| object(&h.ws, *id))
        .collect::<Result<_, _>>()?;
    check(
        leaves(&h.ws, FIRST_WINDOW) == vec![a, c, d] && after == objects,
        "closing a leaf disturbed the remaining instances",
    )?;
    subscriptions_settle(&h.ws, 3)?;
    // Shrink the window: the minimum width (three leaf minimums) is enforced and a
    // further split is rejected without creating anything.
    h.frame(vec![]);
    let min_size =
        h.ws.observation()
            .window(FIRST_WINDOW)
            .ok_or("root closed")?
            .min_size;
    h.set_size(vec2(30.0, 10.0));
    h.frame(vec![]);
    let size = h.window(FIRST_WINDOW)?.size;
    let expected = vec2(3.0 * MIN_EXTENT, MIN_EXTENT);
    check(
        min_size == expected && h.window(FIRST_WINDOW)?.min_size == min_size && size == expected,
        &format!("window minimum not derived from the layout: {min_size:?}, size {size:?}"),
    )?;
    let before = h.ws.observation();
    check(
        action(
            &mut h.ws,
            WorkspaceAction::SplitModule {
                instance_id: a,
                side: Side::Right,
            },
        )
        .is_err_and(|error| error.contains("not enough space")),
        "split below the minimum size was accepted",
    )?;
    let after = h.ws.observation();
    check(
        after.windows == before.windows && after.modules_created == before.modules_created,
        "rejected split changed the layout or created an instance",
    )?;
    let rects = &after.window(FIRST_WINDOW).ok_or("root closed")?.leaf_rects;
    check(
        rects
            .iter()
            .all(|(_, rect)| rect.width() >= MIN_EXTENT - 0.01),
        &format!("leaf below its minimum width: {rects:?}"),
    )?;
    // A leaf at its minimum has no split entries; its body (inside the scaled resize
    // edges) still takes a right click and opens the settings window.
    check(
        [Side::Right, Side::Below].into_iter().all(|side| {
            h.locate(&ControlKey::SplitZone {
                instance_id: a,
                side,
            })
            .is_err()
        }),
        "a leaf without room for a copy still has a split entry",
    )?;
    h.settings(ControlKey::LeafBody(a), &[])?;
    h.ws.check_ownership()?;
    Ok(json!({"equal_widths":widths,"manual_ratio_kept":kept_ratio,
        "min_inner_size":[expected.x,expected.y],"remaining":[a.0,c.0,d.0]}))
}

/// Divider drag through real egui events: grid, Alt, legal clamp, Escape, double click,
/// and no window drag while the divider owns the gesture.
fn w02() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    split(&mut ws, a, Side::Right)?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let (split_id, _, _) = root_split(&h.ws, 0)?;
    let divider = ControlKey::Divider(split_id);
    let y = 210.0;
    let moves = h.desktop.moves_started();
    h.drag(
        divider,
        &[pos2(470.0, y), pos2(523.0, y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let (_, ratio, manual) = root_split(&h.ws, 0)?;
    check(
        (ratio * 900.0 - 528.0).abs() < 0.01 && manual,
        &format!(
            "grid drag ended at {} (manual {manual}), expected 528",
            ratio * 900.0
        ),
    )?;
    check(
        h.desktop.moves_started() == moves,
        "divider drag also started a window move",
    )?;
    h.drag(
        divider,
        &[pos2(560.0, y), pos2(601.0, y)],
        egui::Modifiers::ALT,
        None,
    )?;
    let (_, ratio, _) = root_split(&h.ws, 0)?;
    check(
        (ratio * 900.0 - 601.0).abs() < 0.01,
        &format!("Alt drag snapped: {}", ratio * 900.0),
    )?;
    // The far end of the legal interval (900 − 16 = 884) is off the grid (880) and
    // still reachable: the interval's ends are snap points.
    h.drag(
        divider,
        &[pos2(700.0, y), pos2(899.0, y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let (_, far, _) = root_split(&h.ws, 0)?;
    check(
        (far * 900.0 - (900.0 - MIN_EXTENT)).abs() < 0.01,
        &format!("extreme drag missed the far minimum: {}", far * 900.0),
    )?;
    h.drag(
        divider,
        &[pos2(500.0, y), pos2(5.0, y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let (_, clamped, _) = root_split(&h.ws, 0)?;
    check(
        (clamped * 900.0 - MIN_EXTENT).abs() < 0.01,
        &format!(
            "extreme drag broke or missed the minimum: {}",
            clamped * 900.0
        ),
    )?;
    h.drag(
        divider,
        &[pos2(300.0, y), pos2(650.0, y), pos2(700.0, y)],
        egui::Modifiers::NONE,
        Some(1),
    )?;
    let (_, after_escape, _) = root_split(&h.ws, 0)?;
    let observation = h.ws.observation();
    check(
        (after_escape - clamped).abs() < 1e-6
            && observation
                .window(FIRST_WINDOW)
                .is_some_and(|w| w.divider_drag.is_none()),
        &format!(
            "Escape did not restore the drag start: {} vs {}",
            after_escape * 900.0,
            clamped * 900.0
        ),
    )?;
    check(
        h.ws.has_window(FIRST_WINDOW),
        "Escape during a drag closed the window",
    )?;
    h.double_click(divider)?;
    let (_, reset, reset_manual) = root_split(&h.ws, 0)?;
    check(
        (reset - 0.5).abs() < 1e-6 && !reset_manual,
        "double click did not reset the split",
    )?;
    let moves = h.desktop.moves_started();
    h.drag(
        ControlKey::LeafBody(a),
        &[pos2(120.0, 120.0), pos2(160.0, 140.0)],
        egui::Modifiers::NONE,
        None,
    )?;
    let started = h.desktop.moves_started() - moves;
    check(
        started == 1 && (root_split(&h.ws, 0)?.1 - 0.5).abs() < 1e-6,
        &format!("body drag should start exactly one window move: {started}"),
    )?;
    Ok(
        json!({"grid_position":528,"alt_position":601,"far_position":900.0 - MIN_EXTENT,
        "clamped_position":MIN_EXTENT,
        "escape_restored":true,"double_click_ratio":0.5,"body_start_drag":1}),
    )
}

/// One owner per gesture: split entry drag moves the window, resize edges and
/// corners resize, the divider wins over the overlapping split entry.
fn w03() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    split(&mut ws, a, Side::Right)?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let before = leaves(&h.ws, FIRST_WINDOW);
    let moves = h.desktop.moves_started();
    let zone = ControlKey::SplitZone {
        instance_id: a,
        side: Side::Right,
    };
    let start = h.locate(&zone)?.rect.center();
    h.drag(
        zone,
        &[start + vec2(-20.0, 30.0), start + vec2(-60.0, 60.0)],
        egui::Modifiers::NONE,
        None,
    )?;
    check(
        leaves(&h.ws, FIRST_WINDOW) == before && h.desktop.moves_started() == moves + 1,
        "drag on a split entry split or did not move the window",
    )?;
    let mut resize = Vec::new();
    for edge in [
        ResizeEdge::SouthEast,
        ResizeEdge::East,
        ResizeEdge::NorthWest,
    ] {
        let rect = h.locate(&ControlKey::Resize(edge))?.rect;
        let from = rect.center();
        let moves = h.desktop.moves_started();
        h.drag(
            ControlKey::Resize(edge),
            &[from + vec2(-15.0, -15.0), from + vec2(-30.0, -30.0)],
            egui::Modifiers::NONE,
            None,
        )?;
        let commands = h.take_commands(FIRST_WINDOW);
        let resizes: Vec<_> = commands
            .iter()
            .filter(|command| matches!(command, ViewportCommand::BeginResize(_)))
            .collect();
        check(
            resizes.len() == 1 && h.desktop.moves_started() == moves,
            &format!(
                "{edge:?} gesture had {} owners: {commands:?}",
                resizes.len()
            ),
        )?;
        resize.push(format!("{:?}", resizes[0]));
    }
    // x = 449 lies in both the divider (447..453) and a's split entry (430..450).
    let point = pos2(449.0, 200.0);
    let none = egui::Modifiers::NONE;
    h.frame(vec![Event::PointerMoved(point)]);
    h.frame(vec![Event::PointerButton {
        pos: point,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: none,
    }]);
    h.frame(vec![Event::PointerButton {
        pos: point,
        button: PointerButton::Primary,
        pressed: false,
        modifiers: none,
    }]);
    h.frame(vec![]);
    check(
        leaves(&h.ws, FIRST_WINDOW) == before,
        "a click on the divider over a split entry split the leaf",
    )?;
    Ok(
        json!({"split_entry_drag":"window move only","resize":resize,"divider_over_entry":"divider"}),
    )
}

/// Detach: the instance moves only once the new window shows its first frame;
/// creation failure and Escape keep it in the source.
fn w04() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = split(&mut ws, a, Side::Right)?;
    action(
        &mut ws,
        WorkspaceAction::SetChannel {
            instance_id: b,
            channel: ChannelSelection::Right,
        },
    )?;
    action(
        &mut ws,
        WorkspaceAction::SetStyle {
            instance_id: b,
            style_id: PIXEL_SPECTRUM_STYLE_ID,
        },
    )?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    subscriptions_settle(&h.ws, 2)?;
    let b_object = object(&h.ws, b)?;
    let created_before = h.ws.observation().modules_created;
    h.settings(ControlKey::LeafBody(b), &[SettingsItem::Detach])?;
    let o = h.ws.observation();
    let pending = o
        .windows
        .iter()
        .find(|w| w.pending_detach == Some(b))
        .map(|w| w.id)
        .ok_or("Detach did not open a window")?;
    check(
        leaves(&h.ws, FIRST_WINDOW) == vec![a, b] && leaves(&h.ws, pending).is_empty(),
        "instance left its layout before the new window existed",
    )?;
    h.ws.check_ownership()?;
    h.frame_window(pending, vec![])?;
    let o = h.ws.observation();
    let module = o.module(b).ok_or("detached instance dropped")?;
    check(
        leaves(&h.ws, FIRST_WINDOW) == vec![a]
            && leaves(&h.ws, pending) == vec![b]
            && module.object == b_object
            && module.channel == Some(ChannelSelection::Right)
            && module.active_style == Some(PIXEL_SPECTRUM_STYLE_ID)
            && o.modules_created == created_before,
        "detach rebuilt or lost the instance",
    )?;
    subscriptions_settle(&h.ws, 2)?;
    let position = h.window(pending)?.position;
    check(
        position == pos2(132.0, 132.0),
        &format!("detached window not offset from its source: {position:?}"),
    )?;
    h.ws.check_ownership()?;

    // Native creation fails: the window never shows a frame.
    let c = split(&mut h.ws, a, Side::Right)?;
    let blocked = WindowId(pending.0 + 1);
    h.blocked.insert(blocked);
    let failed = new_window(action(
        &mut h.ws,
        WorkspaceAction::DetachModule { instance_id: c },
    )?)?;
    check(failed == blocked, "unexpected window id")?;
    h.frame(vec![]);
    h.display_time += Duration::from_secs(6);
    h.frame(vec![]);
    check(
        !h.ws.has_window(blocked) && leaves(&h.ws, FIRST_WINDOW) == vec![a, c],
        "failed window creation lost the instance",
    )?;
    // Escape in the source cancels a pending detach.
    let pending_escape = new_window(action(
        &mut h.ws,
        WorkspaceAction::DetachModule { instance_id: c },
    )?)?;
    h.key(egui::Key::Escape);
    check(
        !h.ws.has_window(pending_escape)
            && h.ws.has_window(FIRST_WINDOW)
            && leaves(&h.ws, FIRST_WINDOW) == vec![a, c],
        "Escape did not cancel the pending detach",
    )?;
    let sole = leaves(&h.ws, pending)[0];
    check(
        action(
            &mut h.ws,
            WorkspaceAction::DetachModule { instance_id: sole },
        )
        .is_err(),
        "detaching a window's only module was accepted",
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"detached":b.0,"window":pending.0,"object_kept":true,"subscriptions":2,
        "creation_failure":"kept in source","escape":"canceled"}),
    )
}

/// Move / merge through the "Move to…" target overlay across windows.
fn w05() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = split(&mut ws, a, Side::Right)?;
    action(
        &mut ws,
        WorkspaceAction::SetChannel {
            instance_id: b,
            channel: ChannelSelection::Left,
        },
    )?;
    let window = new_window(action(
        &mut ws,
        WorkspaceAction::DetachModule { instance_id: b },
    )?)?;
    let mut h = Harness::new(ws);
    h.frame_all()?;
    check(leaves(&h.ws, window) == vec![b], "detach precondition")?;
    let b_object = object(&h.ws, b)?;
    let created_before = h.ws.observation().modules_created;
    h.focus = window;
    h.settings(ControlKey::LeafBody(b), &[SettingsItem::MoveTo])?;
    check(
        h.ws.observation().move_source == Some(b),
        "Move to… not armed",
    )?;
    check(
        h.locate(&ControlKey::MoveTarget(MoveTarget::EmptyWindow(window)))
            .is_err(),
        "source window offered itself",
    )?;
    h.focus = FIRST_WINDOW;
    // A new egui Area spends its first pass sizing itself (invisible, disabled).
    h.frame(vec![]);
    h.frame(vec![]);
    let target = MoveTarget::Beside {
        instance_id: a,
        side: Side::Below,
    };
    h.click(ControlKey::MoveTarget(target))?;
    let o = h.ws.observation();
    check(
        leaves(&h.ws, FIRST_WINDOW) == vec![a, b]
            && !h.ws.has_window(window)
            && o.module(b)
                .is_some_and(|m| m.object == b_object && m.channel == Some(ChannelSelection::Left))
            && o.modules_created == created_before
            && o.move_source.is_none(),
        "merge did not move the original object into the target or keep the source window",
    )?;
    check(
        o.window(FIRST_WINDOW).is_some_and(|w| {
            w.splits.len() == 1 && w.splits[0].axis == crate::workspace::Axis::TopBottom
        }),
        "merge did not insert below the target",
    )?;
    subscriptions_settle(&h.ws, 2)?;
    h.ws.check_ownership()?;

    // Escape and right click cancel; the tree is unchanged.
    let tree = h.ws.observation().windows;
    action(&mut h.ws, WorkspaceAction::BeginMove { instance_id: b })?;
    h.frame(vec![]);
    h.key(egui::Key::Escape);
    check(
        h.ws.observation().move_source.is_none() && h.ws.has_window(FIRST_WINDOW),
        "Escape did not cancel Move",
    )?;
    action(&mut h.ws, WorkspaceAction::BeginMove { instance_id: b })?;
    h.frame(vec![]);
    let point = pos2(300.0, 300.0);
    h.frame(vec![
        Event::PointerMoved(point),
        Event::PointerButton {
            pos: point,
            button: PointerButton::Secondary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        },
    ]);
    h.frame(vec![Event::PointerButton {
        pos: point,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    h.frame(vec![]);
    check(
        h.ws.observation().move_source.is_none() && h.ws.observation().windows == tree,
        "right click did not cancel Move or changed the tree",
    )?;

    // Too-small targets are disabled; a vanished target fails without losing the source.
    let c = split(&mut h.ws, a, Side::Right)?;
    let d = split(&mut h.ws, c, Side::Right)?;
    // Three 20-pt columns cannot take a second 16-pt column beside them.
    h.set_size(vec2(60.0, 420.0));
    h.frame(vec![]);
    action(&mut h.ws, WorkspaceAction::BeginMove { instance_id: b })?;
    h.frame(vec![]);
    check(
        h.locate(&ControlKey::MoveTarget(MoveTarget::Beside {
            instance_id: c,
            side: Side::Right,
        }))
        .is_err_and(|error| error.starts_with("Disabled")),
        "a target narrower than both minimums was offered",
    )?;
    let target = ControlKey::MoveTarget(MoveTarget::Beside {
        instance_id: d,
        side: Side::Below,
    });
    let stale = h.locate(&target)?;
    action(&mut h.ws, WorkspaceAction::CloseModule { instance_id: d })?;
    h.frame(vec![]);
    check(
        h.click_target(target, stale).is_err() && h.ws.observation().module(b).is_some(),
        "a vanished target was clicked or lost the source",
    )?;
    action(&mut h.ws, WorkspaceAction::CancelMove)?;

    // An empty window is a target.
    let e = split(&mut h.ws, a, Side::Below)?;
    let empty = new_window(action(
        &mut h.ws,
        WorkspaceAction::DetachModule { instance_id: e },
    )?)?;
    h.frame_all()?;
    action(&mut h.ws, WorkspaceAction::CloseModule { instance_id: e })?;
    action(&mut h.ws, WorkspaceAction::BeginMove { instance_id: c })?;
    h.focus = empty;
    h.frame(vec![]);
    h.frame(vec![]);
    h.click(ControlKey::MoveTarget(MoveTarget::EmptyWindow(empty)))?;
    check(
        leaves(&h.ws, empty) == vec![c] && !leaves(&h.ws, FIRST_WINDOW).contains(&c),
        "move into an empty window failed",
    )?;
    h.ws.check_ownership()?;
    Ok(
        json!({"merged":b.0,"source_window_closed":window.0,"cancel":["Escape","right click"],
        "disabled_target":"narrow","vanished_target":"rejected","empty_window_target":empty.0}),
    )
}

/// Closing windows releases exactly their instances; the shared source and other
/// windows continue, including after the root window closes.
fn w06() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let a = first_leaf(&ws)?;
    let w = add_module(&mut ws, WAVEFORM_MODULE_ID)?;
    let second = new_window(action(
        &mut ws,
        WorkspaceAction::DetachModule { instance_id: w },
    )?)?;
    let mut h = Harness::new(ws);
    h.frame_all()?;
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let both = handle.metrics();
    check(both.pcm_frames > 0, "waveform window not recording")?;
    h.focus = second;
    h.settings(
        ControlKey::LeafBody(w),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::CloseWindow],
    )?;
    check(
        !h.ws.has_window(second) && h.ws.observation().module(w).is_none(),
        "Close window did not release its module",
    )?;
    h.focus = FIRST_WINDOW;
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let after = handle.metrics();
    check(
        after.pcm_frames == before.pcm_frames && after.fft_evaluations > before.fft_evaluations,
        "closing a window stopped the other window or kept its demand",
    )?;
    // The root window closes (as the adapter does on a native close request) while
    // another window keeps the shared source busy.
    let b = split(&mut h.ws, a, Side::Right)?;
    let third = new_window(action(
        &mut h.ws,
        WorkspaceAction::DetachModule { instance_id: b },
    )?)?;
    h.frame_all()?;
    action(
        &mut h.ws,
        WorkspaceAction::CloseWindow {
            window_id: FIRST_WINDOW,
        },
    )?;
    subscriptions_settle(&h.ws, 1)?;
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    check(
        h.ws.has_window(third)
            && h.ws.observation().module(b).is_some()
            && handle.metrics().fft_evaluations > before.fft_evaluations,
        "closing the root window stopped another window",
    )?;
    h.focus = third;
    h.key_with(egui::Key::W, egui::Modifiers::COMMAND);
    check(
        h.ws.windows().is_empty(),
        "closing the last window left windows",
    )?;
    subscriptions_settle(&h.ws, 0)?;
    let idle = handle.metrics();
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    check(
        handle.metrics() == idle,
        "no window remains but analysis continues",
    )?;
    Ok(
        json!({"closed_window_pcm_stopped":true,"root_closed_other_continues":third.0,
        "last_window":"all demand released"}),
    )
}

/// Desktop placement in physical pixels across scales: snapping while dragged, the
/// divider between touching windows, grouping (outlined on the Window tab) and
/// group movement; logical hit testing and grid at 2x; scale change cancels a
/// divider drag.
fn w07() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = split(&mut ws, a, Side::Right)?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    h.new_window_ppp = 2.0;
    let second = new_window(action(
        &mut h.ws,
        WorkspaceAction::DetachModule { instance_id: b },
    )?)?;
    h.frame_all()?;
    check(
        leaves(&h.ws, second) == vec![b],
        "detach to a 2x window failed",
    )?;
    // Drag the 2x window close to the root's right edge: it snaps while the button
    // is still held, in physical pixels; Alt moves it freely.
    h.focus = second;
    let start = h.window(second)?.position;
    let grab = h.grab(ControlKey::LeafBody(b))?;
    let near = grab + (pos2(1007.0, 120.0) - start);
    h.move_pointer(near, false)?;
    check(
        h.window(second)?.position == pos2(1000.0, 100.0),
        &format!(
            "2x window did not snap to the 1x neighbor while dragged: {:?}",
            h.window(second)?.position
        ),
    )?;
    // Both windows draw the divider in the passes the move ran.
    let dividers = |h: &Harness, window| edge_lines(h, window, EdgeLine::Divider);
    check(
        dividers(&h, FIRST_WINDOW) == [(Edge::Right, 100.0, 520.0)]
            && dividers(&h, second) == [(Edge::Left, 100.0, 520.0)],
        &format!(
            "snapped windows draw no divider: root {:?}, 2x {:?}",
            dividers(&h, FIRST_WINDOW),
            dividers(&h, second)
        ),
    )?;
    h.move_pointer(near, true)?;
    check(
        h.window(second)?.position == pos2(1007.0, 120.0)
            && dividers(&h, FIRST_WINDOW).is_empty()
            && dividers(&h, second).is_empty(),
        "Alt did not move freely, or apart windows kept a divider",
    )?;
    h.move_pointer(near, false)?;
    h.release()?;
    for _ in 0..3 {
        h.frame_all()?;
    }
    check(
        h.window(second)?.position == pos2(1000.0, 100.0)
            && h.window(FIRST_WINDOW)?.position == pos2(100.0, 100.0),
        "windows moved after the release",
    )?;
    let outline = |h: &Harness, window| edge_lines(h, window, EdgeLine::Outline);
    check(
        outline(&h, FIRST_WINDOW).is_empty() && outline(&h, second).is_empty(),
        "windows outlined without the Window tab",
    )?;
    h.settings(
        ControlKey::LeafBody(b),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::Group],
    )?;
    h.frame_all()?;
    let o = h.ws.observation();
    let group = o.window(second).and_then(|w| w.group);
    check(
        group.is_some() && o.window(FIRST_WINDOW).and_then(|w| w.group) == group,
        "touching windows were not grouped",
    )?;
    // The Window tab outlines the group: its outer boundary, not the shared edge.
    check(
        outline(&h, FIRST_WINDOW)
            == [
                (Edge::Left, 100.0, 520.0),
                (Edge::Top, 100.0, 1000.0),
                (Edge::Bottom, 100.0, 1000.0),
            ]
            && outline(&h, second)
                == [
                    (Edge::Left, 520.0, 940.0),
                    (Edge::Top, 1000.0, 1900.0),
                    (Edge::Right, 100.0, 940.0),
                    (Edge::Bottom, 1000.0, 1900.0),
                ],
        &format!(
            "group outline: root {:?}, 2x {:?}",
            outline(&h, FIRST_WINDOW),
            outline(&h, second)
        ),
    )?;
    h.in_settings(&[SettingsItem::Tab(Tab::Module)])?;
    h.frame_all()?;
    check(
        outline(&h, FIRST_WINDOW).is_empty() && outline(&h, second).is_empty(),
        "the outline stayed after leaving the Window tab",
    )?;
    // Locking the member keeps the whole group in place: neither body starts a
    // move until it is unlocked.
    h.focus = second;
    h.settings(
        ControlKey::LeafBody(b),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::Lock],
    )?;
    let mut locked_drags = Vec::new();
    for (window, leaf) in [(second, b), (FIRST_WINDOW, a)] {
        h.focus = window;
        let moves = h.desktop.moves_started();
        let start = h.locate(&ControlKey::LeafBody(leaf))?.rect.center();
        h.drag(
            ControlKey::LeafBody(leaf),
            &[start + vec2(20.0, 10.0), start + vec2(40.0, 20.0)],
            egui::Modifiers::NONE,
            None,
        )?;
        locked_drags.push(h.desktop.moves_started() - moves);
    }
    check(
        h.ws.observation().window(second).is_some_and(|w| w.locked) && locked_drags == [0, 0],
        &format!("a locked group still started moves: {locked_drags:?}"),
    )?;
    h.focus = second;
    h.settings(
        ControlKey::LeafBody(b),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::Lock],
    )?;
    // Dragging the root moves the member by the same physical delta, at once.
    h.focus = FIRST_WINDOW;
    let grab = h.grab(ControlKey::LeafBody(a))?;
    h.move_pointer(grab + vec2(50.0, 30.0), false)?;
    check(
        h.window(second)?.position == pos2(1050.0, 130.0)
            && h.window(FIRST_WINDOW)?.position == pos2(150.0, 130.0),
        &format!(
            "group did not move together: root {:?}, member {:?}",
            h.window(FIRST_WINDOW)?.position,
            h.window(second)?.position
        ),
    )?;
    h.release()?;
    // Another program moving the root takes the member along too.
    h.external_move(FIRST_WINDOW, pos2(200.0, 160.0))?;
    h.frame_all()?;
    check(
        h.window(second)?.position == pos2(1100.0, 160.0),
        &format!(
            "group did not follow an external move: {:?}",
            h.window(second)?.position
        ),
    )?;
    h.focus = second;
    h.settings(
        ControlKey::LeafBody(b),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::LeaveGroup],
    )?;
    check(
        h.ws.observation().windows.iter().all(|w| w.group.is_none()),
        "Leave group left a one-member group",
    )?;
    h.external_move(FIRST_WINDOW, pos2(400.0, 300.0))?;
    h.focus = FIRST_WINDOW;
    let grab = h.grab(ControlKey::LeafBody(a))?;
    h.move_pointer(grab + vec2(-100.0, 0.0), false)?;
    h.release()?;
    check(
        h.window(second)?.position == pos2(1100.0, 160.0)
            && h.window(FIRST_WINDOW)?.position == pos2(300.0, 300.0),
        "ungrouped window followed",
    )?;
    // Logical hit testing and grid on a 2x window.
    h.focus = FIRST_WINDOW;
    h.window_mut(FIRST_WINDOW)?.ppp = 2.0;
    h.frame(vec![]);
    h.click(ControlKey::SplitZone {
        instance_id: a,
        side: Side::Right,
    })?;
    check(
        leaves(&h.ws, FIRST_WINDOW).len() == 2,
        "split entry missed at 2x",
    )?;
    let (split_id, _, _) = root_split(&h.ws, 0)?;
    h.drag(
        ControlKey::Divider(split_id),
        &[pos2(470.0, 200.0), pos2(523.0, 200.0)],
        egui::Modifiers::NONE,
        None,
    )?;
    let (_, ratio, _) = root_split(&h.ws, 0)?;
    let width = h.window(FIRST_WINDOW)?.size.x;
    check(
        (ratio * width - 528.0).abs() < 0.01,
        &format!("2x grid in physical units: {}", ratio * width),
    )?;
    // A scale change mid-drag cancels the drag and restores its start.
    let start = ratio;
    let divider = h.locate(&ControlKey::Divider(split_id))?.rect.center();
    h.frame(vec![Event::PointerMoved(divider)]);
    h.frame(vec![Event::PointerButton {
        pos: divider,
        button: PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    }]);
    h.frame(vec![Event::PointerMoved(divider + vec2(60.0, 0.0))]);
    h.window_mut(FIRST_WINDOW)?.ppp = 1.0;
    h.frame(vec![Event::PointerMoved(divider + vec2(90.0, 0.0))]);
    h.frame(vec![Event::PointerButton {
        pos: divider + vec2(90.0, 0.0),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    h.frame(vec![]);
    let (_, after, _) = root_split(&h.ws, 0)?;
    check(
        (after - start).abs() < 1e-6,
        &format!("scale change kept a stale drag: {after} vs {start}"),
    )?;
    Ok(
        json!({"snap_while_dragged":[1000,100],"divider":"both sides of the shared edge",
        "group_outline":"Window tab only","group_follow":[1050,130],"external_follow":[1100,160],
        "ungrouped":"independent","locked_group_moves":[0,0],
        "grid_2x_logical":528,"scale_change":"drag canceled"}),
    )
}

/// Repeated split / detach / merge / close returns to the same windows, instances
/// and analysis demand, without leftover gesture state.
fn w08() -> Result<Value, String> {
    let (ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let a = first_leaf(&ws)?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    subscriptions_settle(&h.ws, 1)?;
    let baseline = handle.metrics();
    for round in 0..10 {
        let x = split(&mut h.ws, a, Side::Right)?;
        let window = new_window(action(
            &mut h.ws,
            WorkspaceAction::DetachModule { instance_id: x },
        )?)?;
        h.frame_all()?;
        h.ws.check_ownership()?;
        action(&mut h.ws, WorkspaceAction::BeginMove { instance_id: x })?;
        action(
            &mut h.ws,
            WorkspaceAction::MoveModule {
                instance_id: x,
                target: MoveTarget::Beside {
                    instance_id: a,
                    side: Side::Below,
                },
            },
        )?;
        h.frame_all()?;
        check(
            !h.ws.has_window(window) && leaves(&h.ws, FIRST_WINDOW) == vec![a, x],
            &format!("round {round}: merge left a window or lost the module"),
        )?;
        h.ws.check_ownership()?;
        action(&mut h.ws, WorkspaceAction::CloseModule { instance_id: x })?;
        h.frame_all()?;
    }
    subscriptions_settle(&h.ws, 1)?;
    let o = h.ws.observation();
    check(
        o.windows.len() == 1
            && h.workspace_windows().count() == 1
            && o.modules.len() == 1
            && o.move_source.is_none()
            && o.windows
                .iter()
                .all(|w| w.divider_drag.is_none() && w.pending_detach.is_none()),
        "cycles left windows, instances or gesture state",
    )?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    let after = handle.metrics();
    check(
        after.subscriptions == baseline.subscriptions
            && after.nodes == baseline.nodes
            && after.pcm_frames == baseline.pcm_frames,
        &format!("cycles changed analysis demand: {baseline:?} -> {after:?}"),
    )?;
    Ok(json!({"rounds":10,"windows":1,"modules":1,"subscriptions":after.subscriptions}))
}

/// A minimized window stops only work that serves the display. The Spectrum has
/// none: its Tilt feeds its Peak, so both keep running, as does the Waveform's
/// recording; restoring brings the curve back.
fn w09() -> Result<Value, String> {
    let (ws, mut driver) = workspace()?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    add_module(&mut h.ws, WAVEFORM_MODULE_ID)?;
    let handle = h.ws.dataflow_handle_for_automation();
    driver
        .advance_frames(4800, deadline())
        .map_err(|e| e.to_string())?;
    let shown = handle.metrics();
    check(
        shown.active_spectrum_nodes == 2,
        &format!("visible Spectrum runs Tilt/Peak: {shown:?}"),
    )?;
    // The viewport reports itself minimized: the pass draws nothing.
    h.window_mut(FIRST_WINDOW)?.minimized = true;
    let minimized = h.frame(vec![]);
    check(minimized.shapes.is_empty(), "a minimized window was drawn")?;
    let hidden = handle.metrics();
    driver
        .advance_frames(4800, deadline())
        .map_err(|e| e.to_string())?;
    let while_hidden = handle.metrics();
    check(
        while_hidden.active_spectrum_nodes == 2
            && while_hidden.pcm_frames == hidden.pcm_frames + 4800
            && while_hidden.spectrum_evaluations > hidden.spectrum_evaluations,
        &format!("minimizing stopped continuous analysis: {hidden:?} -> {while_hidden:?}"),
    )?;
    h.window_mut(FIRST_WINDOW)?.minimized = false;
    h.frame(vec![]);
    driver
        .advance_frames(4800, deadline())
        .map_err(|e| e.to_string())?;
    let restored = h.frame(vec![]);
    check(
        restored.shapes.len() > 3,
        "restored window did not draw its Modules",
    )?;
    Ok(json!({
        "visible_spectrum_nodes": shown.active_spectrum_nodes,
        "hidden_spectrum_nodes": while_hidden.active_spectrum_nodes,
        "hidden_pcm_frames": while_hidden.pcm_frames - hidden.pcm_frames,
    }))
}

fn primary(pos: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// Left clicks at a fixed point. The harness's own gestures need a registered
/// control; a locked window registers no chrome, so W10 aims at remembered points.
fn raw_clicks(h: &mut Harness, at: Pos2, count: usize) {
    h.frame(vec![Event::PointerMoved(at)]);
    for _ in 0..count {
        h.frame(vec![primary(at, true)]);
        h.frame(vec![primary(at, false)]);
    }
    h.frame(vec![]);
}

/// Left press at `from`, move to `to` in two frames, release there.
fn raw_drag(h: &mut Harness, from: Pos2, to: Pos2) {
    h.frame(vec![Event::PointerMoved(from)]);
    h.frame(vec![primary(from, true)]);
    h.frame(vec![Event::PointerMoved(from + (to - from) / 2.0)]);
    h.frame(vec![Event::PointerMoved(to)]);
    h.frame(vec![primary(to, false)]);
    h.frame(vec![]);
}

/// A wheel notch and a pinch over `at`; display time advances so egui's smoothed
/// scrolling delivers the notch.
fn wheel_and_pinch(h: &mut Harness, at: Pos2) {
    h.frame(vec![Event::PointerMoved(at)]);
    h.frame(vec![
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 40.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        },
        Event::Zoom(1.5),
    ]);
    for _ in 0..30 {
        h.display_time += Duration::from_millis(16);
        h.frame(vec![]);
    }
}

fn gestures(commands: &[ViewportCommand]) -> usize {
    commands
        .iter()
        .filter(|command| {
            matches!(
                command,
                ViewportCommand::StartDrag | ViewportCommand::BeginResize(_)
            )
        })
        .count()
}

fn view(h: &Harness, id: ModuleInstanceId) -> Result<Vec<(&'static str, String)>, String> {
    h.ws.observation()
        .module(id)
        .map(|module| {
            module
                .view
                .iter()
                .map(|(key, value)| (*key, format!("{value:?}")))
                .collect()
        })
        .ok_or_else(|| format!("instance {} missing", id.0))
}

/// Whether a present control of the focused window is enabled.
fn enabled(h: &Harness, key: &ControlKey) -> Result<bool, String> {
    match h.locate(key) {
        Ok(_) => Ok(true),
        Err(error) if error.starts_with("Disabled control") => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether the settings window's Maximize button is enabled; focus stays.
fn maximize_enabled(h: &mut Harness) -> Result<bool, String> {
    let origin = h.focus;
    h.focus = SETTINGS;
    h.frame(vec![]);
    let enabled = enabled(h, &ControlKey::Settings(SettingsItem::Maximize));
    h.focus = origin;
    enabled
}

/// Whether "Move to…" offers the right edge of `beside` to `source`.
fn move_target_enabled(
    h: &mut Harness,
    source: ModuleInstanceId,
    beside: ModuleInstanceId,
) -> Result<bool, String> {
    action(
        &mut h.ws,
        WorkspaceAction::BeginMove {
            instance_id: source,
        },
    )?;
    h.frame(vec![]);
    let enabled = enabled(
        h,
        &ControlKey::MoveTarget(MoveTarget::Beside {
            instance_id: beside,
            side: Side::Right,
        }),
    );
    action(&mut h.ws, WorkspaceAction::CancelMove)?;
    h.frame(vec![]);
    enabled
}

/// Lock: a locked window takes right clicks only. Left clicks, drags and double
/// clicks, the wheel and pinch reach nothing (no split entry, divider or resize
/// edge, no selection, no window move, no Module input); native sizing, Maximize
/// and move targets in it are off; locking ends a divider drag at its start.
/// Unlocking brings all of it back.
fn w10() -> Result<Value, String> {
    let (mut ws, mut driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = split(&mut ws, a, Side::Right)?;
    let mut h = Harness::new(ws);
    // The Spectrum takes wheel and pinch only once it has a spectrum to show.
    driver
        .advance_frames(4800, deadline())
        .map_err(|e| e.to_string())?;
    h.frame(vec![]);
    let (split_id, ratio, _) = root_split(&h.ws, 0)?;
    let entry_key = ControlKey::SplitZone {
        instance_id: a,
        side: Side::Below,
    };
    let chrome = [
        entry_key,
        ControlKey::Divider(split_id),
        ControlKey::Resize(ResizeEdge::SouthEast),
    ];
    let entry = h.locate(&entry_key)?.rect.center();
    let divider = h.locate(&ControlKey::Divider(split_id))?.rect.center();
    let corner = h
        .locate(&ControlKey::Resize(ResizeEdge::SouthEast))?
        .rect
        .center();
    let body_a = h.locate(&ControlKey::LeafBody(a))?.rect.center();
    let body_b = h.locate(&ControlKey::LeafBody(b))?.rect.center();

    // Locking in the middle of a divider drag ends the drag at its start.
    let aside = divider + vec2(-80.0, 0.0);
    h.frame(vec![Event::PointerMoved(divider)]);
    h.frame(vec![primary(divider, true)]);
    h.frame(vec![Event::PointerMoved(divider + vec2(-40.0, 0.0))]);
    h.frame(vec![Event::PointerMoved(aside)]);
    let dragged = root_split(&h.ws, 0)?.1;
    action(
        &mut h.ws,
        WorkspaceAction::SetLocked {
            window_id: FIRST_WINDOW,
            enabled: true,
        },
    )?;
    h.frame(vec![primary(aside, false)]);
    h.frame(vec![]);
    let o = h.ws.observation();
    let window = o.window(FIRST_WINDOW).ok_or("root window missing")?;
    check(
        (dragged - ratio).abs() > 0.01
            && root_split(&h.ws, 0)?.1 == ratio
            && window.divider_drag.is_none(),
        &format!(
            "locking did not end the divider drag at its start: {ratio} -> {dragged} -> {:?}",
            root_split(&h.ws, 0)?
        ),
    )?;
    action(
        &mut h.ws,
        WorkspaceAction::SetLocked {
            window_id: FIRST_WINDOW,
            enabled: false,
        },
    )?;
    h.frame(vec![]);
    h.take_commands(FIRST_WINDOW);

    // Lock through the settings window: the right click still opens it.
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::Lock],
    )?;
    let commands = h.take_commands(FIRST_WINDOW);
    let registered: Vec<_> = chrome.iter().filter(|key| h.locate(key).is_ok()).collect();
    let refused = action(
        &mut h.ws,
        WorkspaceAction::ToggleMaximized {
            window_id: FIRST_WINDOW,
        },
    )
    .is_err();
    check(
        h.ws.observation()
            .window(FIRST_WINDOW)
            .is_some_and(|w| w.locked)
            && commands.contains(&ViewportCommand::Resizable(false))
            && registered.is_empty()
            && !maximize_enabled(&mut h)?
            && refused
            && !move_target_enabled(&mut h, a, b)?,
        &format!(
            "locked window: commands {commands:?}, chrome still registered {registered:?}, \
             maximize refused {refused}"
        ),
    )?;

    // Every left gesture, the wheel and pinch change nothing, the selection in the
    // open settings window included.
    let before = h.ws.observation().window(FIRST_WINDOW).cloned();
    let target_before = h.ws.observation().settings_target;
    let view_before = view(&h, a)?;
    let moves = h.desktop.moves_started();
    raw_clicks(&mut h, entry, 1);
    raw_clicks(&mut h, body_b, 1);
    raw_clicks(&mut h, divider, 2);
    raw_drag(&mut h, divider, aside);
    raw_drag(&mut h, corner, corner + vec2(-30.0, -30.0));
    raw_drag(&mut h, body_b, body_b + vec2(40.0, 20.0));
    wheel_and_pinch(&mut h, body_a);
    let commands = h.take_commands(FIRST_WINDOW);
    let after = h.ws.observation().window(FIRST_WINDOW).cloned();
    let target_after = h.ws.observation().settings_target;
    check(
        after == before
            && target_after == target_before
            && view(&h, a)? == view_before
            && gestures(&commands) == 0
            && h.desktop.moves_started() == moves,
        &format!(
            "a locked window reacted to the left button, wheel or pinch: \
             {before:?} -> {after:?}, commands {commands:?}"
        ),
    )?;

    // Unlocking brings everything back, through the same raw gestures.
    h.settings(
        ControlKey::LeafBody(a),
        &[SettingsItem::Tab(Tab::Window), SettingsItem::Lock],
    )?;
    let commands = h.take_commands(FIRST_WINDOW);
    let registered = chrome.iter().filter(|key| h.locate(key).is_ok()).count();
    check(
        commands.contains(&ViewportCommand::Resizable(true))
            && registered == chrome.len()
            && maximize_enabled(&mut h)?
            && move_target_enabled(&mut h, a, b)?,
        &format!("unlocked window: commands {commands:?}, {registered} chrome controls"),
    )?;
    // A left click selects the module in the open settings window again.
    raw_clicks(&mut h, body_b, 1);
    let selected = h.ws.observation().settings_target.and_then(|(id, _)| id);
    raw_drag(&mut h, divider, aside);
    let moved_ratio = root_split(&h.ws, 0)?.1;
    raw_drag(&mut h, corner, corner + vec2(-30.0, -30.0));
    let resizes = gestures(&h.take_commands(FIRST_WINDOW));
    wheel_and_pinch(&mut h, body_a);
    let view_after = view(&h, a)?;
    check(
        selected == Some(b) && moved_ratio != ratio && resizes == 1 && view_after != view_before,
        &format!(
            "unlocked gestures: selected {selected:?}, ratio {ratio} -> {moved_ratio}, \
             {resizes} resizes, view {view_before:?} -> {view_after:?}"
        ),
    )?;
    Ok(json!({
        "lock_ends_divider_drag": [ratio, dragged, ratio],
        "locked": {"resizable": false, "chrome": 0, "maximize": "disabled",
                   "move_target": "disabled", "left_wheel_pinch": "no effect"},
        "unlocked": {"resizable": true, "chrome": registered, "selected": selected.map(|id| id.0),
                     "ratio": moved_ratio, "resizes": resizes},
    }))
}

/// Lines a window's last pass drew along its frame, of one kind.
fn edge_lines(h: &Harness, window: WindowId, line: EdgeLine) -> Vec<(Edge, f32, f32)> {
    h.ws.observation()
        .window(window)
        .map(|w| {
            w.edges
                .iter()
                .filter(|e| e.line == line)
                .map(|e| (e.edge, e.start, e.end))
                .collect()
        })
        .unwrap_or_default()
}

/// A group moves as one: the Window tab's Group button previews the windows it
/// would add; dragged past the work area the group stops with every member still
/// reachable; Escape puts it back; it snaps to the work area while dragged and
/// stays put after the release; a group left off every monitor comes back as one.
fn w11() -> Result<Value, String> {
    let (mut ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let b = split(&mut ws, a, Side::Right)?;
    let c = split(&mut ws, b, Side::Below)?;
    let mut h = Harness::new(ws);
    h.frame(vec![]);
    let detach = |h: &mut Harness, instance_id| -> Result<WindowId, String> {
        let window = new_window(action(
            &mut h.ws,
            WorkspaceAction::DetachModule { instance_id },
        )?)?;
        h.frame_all()?;
        Ok(window)
    };
    let right = detach(&mut h, b)?;
    let below = detach(&mut h, c)?;
    // The root (900×420) at (100, 100); `right` (450×210) touches its right edge,
    // `below` stands apart.
    h.window_mut(right)?.position = pos2(1000.0, 100.0);
    h.window_mut(below)?.position = pos2(100.0, 700.0);
    h.frame_all()?;
    h.frame_all()?;
    let positions = |h: &Harness| -> Result<[Pos2; 3], String> {
        Ok([
            h.window(FIRST_WINDOW)?.position,
            h.window(right)?.position,
            h.window(below)?.position,
        ])
    };

    // Hovering Group previews the window it would add; grouping keeps the outline.
    h.focus = FIRST_WINDOW;
    h.settings(ControlKey::LeafBody(a), &[SettingsItem::Tab(Tab::Window)])?;
    h.focus = SETTINGS;
    h.point_at(ControlKey::Settings(SettingsItem::Group))?;
    h.frame_all()?;
    let previewed = [
        !edge_lines(&h, FIRST_WINDOW, EdgeLine::Outline).is_empty(),
        !edge_lines(&h, right, EdgeLine::Candidate).is_empty(),
        edge_lines(&h, below, EdgeLine::Candidate).is_empty()
            && edge_lines(&h, below, EdgeLine::Outline).is_empty(),
    ];
    check(
        previewed == [true, true, true],
        &format!("Group hover preview (root outlined, right previewed, below not): {previewed:?}"),
    )?;
    h.in_settings(&[SettingsItem::Group])?;
    h.frame_all()?;
    let o = h.ws.observation();
    check(
        o.window(right).and_then(|w| w.group).is_some()
            && o.window(right).and_then(|w| w.group)
                == o.window(FIRST_WINDOW).and_then(|w| w.group)
            && o.window(below).and_then(|w| w.group).is_none()
            && !edge_lines(&h, right, EdgeLine::Outline).is_empty(),
        "Group did not join the touching window, or its outline is missing",
    )?;

    // Far past the right edge of the work area: the group stops with `right`
    // 32 px on screen, the members exactly as far apart as before.
    h.focus = FIRST_WINDOW;
    let grab = h.grab(ControlKey::LeafBody(a))?;
    h.move_pointer(grab + vec2(3000.0, 0.0), false)?;
    let held = positions(&h)?;
    check(
        held[0] == pos2(988.0, 100.0) && held[1] == pos2(1888.0, 100.0),
        &format!("group past the edge: {held:?}"),
    )?;
    // Escape puts the group back.
    h.cancel_move()?;
    h.release()?;
    check(
        positions(&h)? == [pos2(100.0, 100.0), pos2(1000.0, 100.0), pos2(100.0, 700.0)],
        &format!("Escape did not put the group back: {:?}", positions(&h)?),
    )?;

    // 10 px below the work area's top: snapped there while held, and nothing moves
    // after the release.
    let grab = h.grab(ControlKey::LeafBody(a))?;
    h.move_pointer(grab + vec2(0.0, -90.0), false)?;
    let snapped = positions(&h)?;
    h.release()?;
    for _ in 0..5 {
        h.frame_all()?;
    }
    let settled = positions(&h)?;
    check(
        snapped == [pos2(100.0, 0.0), pos2(1000.0, 0.0), pos2(100.0, 700.0)] && settled == snapped,
        &format!("snap to the work area: held {snapped:?}, settled {settled:?}"),
    )?;

    // The group's monitor is gone: it comes back as one.
    h.window_mut(FIRST_WINDOW)?.position = pos2(-3000.0, 0.0);
    h.window_mut(right)?.position = pos2(-2100.0, 0.0);
    h.frame_all()?;
    h.frame_all()?;
    let rescued = positions(&h)?;
    check(
        rescued == [pos2(0.0, 0.0), pos2(900.0, 0.0), pos2(100.0, 700.0)],
        &format!("group not brought back as one: {rescued:?}"),
    )?;
    Ok(json!({"preview":"Group hover outlines the touching window",
        "past_edge":[[held[0].x,held[0].y],[held[1].x,held[1].y]],"escape":"back to start",
        "snap_top":[[snapped[0].x,snapped[0].y],[snapped[1].x,snapped[1].y]],
        "rescued":[[rescued[0].x,rescued[0].y],[rescued[1].x,rescued[1].y]]}))
}

/// Window tab New: an empty window of the source's size, offset by the detach
/// offset, on top like the source but neither locked nor grouped; the settings
/// window moves on to it (Add module there fills it) and Close closes only it.
fn w12() -> Result<Value, String> {
    let (ws, _driver) = workspace()?;
    let a = first_leaf(&ws)?;
    let mut h = Harness::new(ws);
    h.set_size(vec2(640.0, 300.0));
    h.frame(vec![]);
    h.settings(
        ControlKey::LeafBody(a),
        &[
            SettingsItem::Tab(Tab::Window),
            SettingsItem::AlwaysOnTop,
            SettingsItem::Lock,
            SettingsItem::NewWindow,
        ],
    )?;
    h.frame_all()?;
    let created: Vec<WindowId> =
        h.ws.observation()
            .windows
            .iter()
            .map(|w| w.id)
            .filter(|id| *id != FIRST_WINDOW)
            .collect();
    let [new] = created[..] else {
        return Err(format!("expected one new window, got {created:?}"));
    };
    let source = h.window(FIRST_WINDOW)?;
    let sim = h.window(new)?;
    let (offset, size) = (sim.position - source.position, sim.size);
    let o = h.ws.observation();
    let observed = o.window(new).ok_or("new window missing")?;
    check(
        offset == vec2(32.0, 32.0)
            && size == vec2(640.0, 300.0)
            && observed.leaves.is_empty()
            && observed.always_on_top
            && !observed.locked
            && observed.group.is_none(),
        &format!("new window: offset {offset:?}, size {size:?}, {observed:?}"),
    )?;
    // The settings window now edits the new window: adding fills it, not the source.
    h.in_settings(&[SettingsItem::AddModule(WAVEFORM_MODULE_ID)])?;
    let filled = leaves(&h.ws, new).len();
    let source_leaves = leaves(&h.ws, FIRST_WINDOW);
    check(
        filled == 1 && source_leaves == vec![a],
        &format!("Add module after New: new window {filled} leaves, source {source_leaves:?}"),
    )?;
    h.in_settings(&[SettingsItem::CloseWindow])?;
    check(
        !h.ws.has_window(new) && h.ws.has_window(FIRST_WINDOW),
        "Close did not close only the new window",
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"offset":[offset.x,offset.y],"size":[size.x,size.y],
        "inherits":"always on top","not_inherited":["lock","group"],
        "settings_target":"new window","close":"new window only"}))
}
