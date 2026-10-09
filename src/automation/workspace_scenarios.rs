//! W01–W08: Workspace / BSP / multi-window behavior through the real Workspace,
//! Module factories, manual source and egui events. Native window management, real
//! DPI changes and the OS move loop are verified separately on a desktop.

use std::time::Duration;

use eframe::egui::{self, Event, PointerButton, ViewportCommand, pos2, vec2};
use serde_json::{Value, json};

use crate::{
    builtins::{PIXEL_SPECTRUM_STYLE_ID, WAVEFORM_MODULE_ID},
    dataflow::ChannelSelection,
    workspace::{
        ActionOutcome, ControlKey, MenuItem, ModuleInstanceId, MoveTarget, ROOT_WINDOW, ResizeEdge,
        Side, SplitId, WindowId, Workspace, WorkspaceAction,
    },
};

use super::{
    deadline,
    harness::Harness,
    scenarios::{action, add_module, check, created, first_leaf, workspace},
    wait_until,
};

type Scenario = fn() -> Result<Value, String>;

pub(super) const CASES: [(&str, Scenario); 8] = [
    ("W01", w01),
    ("W02", w02),
    ("W03", w03),
    ("W04", w04),
    ("W05", w05),
    ("W06", w06),
    ("W07", w07),
    ("W08", w08),
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
        .window(ROOT_WINDOW)
        .and_then(|w| w.splits.get(index).map(|s| (s.id, s.ratio, s.manual)))
        .ok_or_else(|| "missing split".to_owned())
}

fn subscriptions_settle(ws: &Workspace, expected: u64) -> Result<(), String> {
    let handle = ws.dataflow_handle_for_automation();
    wait_until(deadline(), &format!("{expected} subscriptions"), || {
        (handle.metrics().subscriptions == expected).then_some(())
    })
}

fn count_start_drag(commands: &[ViewportCommand]) -> usize {
    commands
        .iter()
        .filter(|command| matches!(command, ViewportCommand::StartDrag))
        .count()
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
    let root = o.window(ROOT_WINDOW).ok_or("root closed")?;
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
                && module.active_style == Some(crate::builtins::DEFAULT_SPECTRUM_STYLE_ID)
                && module.type_id == crate::builtins::SPECTRUM_MODULE_ID,
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
            window_id: ROOT_WINDOW,
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
        leaves(&h.ws, ROOT_WINDOW) == vec![a, c, d] && after == objects,
        "closing a leaf disturbed the remaining instances",
    )?;
    subscriptions_settle(&h.ws, 3)?;
    // Shrink the window: the minimum width (three Spectrum minimums) is enforced and a
    // further split is rejected without creating anything.
    h.frame(vec![]);
    let min_size =
        h.ws.observation()
            .window(ROOT_WINDOW)
            .ok_or("root closed")?
            .min_size;
    h.set_size(vec2(300.0, 200.0));
    h.frame(vec![]);
    let size = h.window(ROOT_WINDOW)?.size;
    check(
        min_size == vec2(480.0, 100.0)
            && h.window(ROOT_WINDOW)?.min_size == min_size
            && size == vec2(480.0, 200.0),
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
    let rects = &after.window(ROOT_WINDOW).ok_or("root closed")?.leaf_rects;
    check(
        rects.iter().all(|(_, rect)| rect.width() >= 160.0 - 0.01),
        &format!("leaf below its minimum width: {rects:?}"),
    )?;
    h.ws.check_ownership()?;
    Ok(json!({"equal_widths":widths,"manual_ratio_kept":kept_ratio,
        "min_inner_size":[480,100],"remaining":[a.0,c.0,d.0]}))
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
    h.take_commands(ROOT_WINDOW);
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
        count_start_drag(&h.take_commands(ROOT_WINDOW)) == 0,
        "divider drag also started a window drag",
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
    h.drag(
        divider,
        &[pos2(500.0, y), pos2(5.0, y)],
        egui::Modifiers::NONE,
        None,
    )?;
    let (_, clamped, _) = root_split(&h.ws, 0)?;
    check(
        (clamped * 900.0 - 160.0).abs() < 0.01,
        &format!("extreme drag broke the minimum: {}", clamped * 900.0),
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
                .window(ROOT_WINDOW)
                .is_some_and(|w| w.divider_drag.is_none()),
        &format!(
            "Escape did not restore the drag start: {} vs {}",
            after_escape * 900.0,
            clamped * 900.0
        ),
    )?;
    check(
        h.ws.has_window(ROOT_WINDOW),
        "Escape during a drag closed the window",
    )?;
    h.double_click(divider)?;
    let (_, reset, reset_manual) = root_split(&h.ws, 0)?;
    check(
        (reset - 0.5).abs() < 1e-6 && !reset_manual,
        "double click did not reset the split",
    )?;
    h.take_commands(ROOT_WINDOW);
    h.drag(
        ControlKey::LeafBody(a),
        &[pos2(120.0, 120.0), pos2(160.0, 140.0)],
        egui::Modifiers::NONE,
        None,
    )?;
    let commands = h.take_commands(ROOT_WINDOW);
    check(
        count_start_drag(&commands) == 1 && (root_split(&h.ws, 0)?.1 - 0.5).abs() < 1e-6,
        &format!("body drag should start exactly one native move: {commands:?}"),
    )?;
    Ok(
        json!({"grid_position":528,"alt_position":601,"clamped_position":160,
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
    let before = leaves(&h.ws, ROOT_WINDOW);
    h.take_commands(ROOT_WINDOW);
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
    let commands = h.take_commands(ROOT_WINDOW);
    check(
        leaves(&h.ws, ROOT_WINDOW) == before && count_start_drag(&commands) == 1,
        &format!("drag on a split entry split or did not move the window: {commands:?}"),
    )?;
    let mut resize = Vec::new();
    for edge in [
        ResizeEdge::SouthEast,
        ResizeEdge::East,
        ResizeEdge::NorthWest,
    ] {
        let rect = h.locate(&ControlKey::Resize(edge))?.rect;
        let from = rect.center();
        h.drag(
            ControlKey::Resize(edge),
            &[from + vec2(-15.0, -15.0), from + vec2(-30.0, -30.0)],
            egui::Modifiers::NONE,
            None,
        )?;
        let commands = h.take_commands(ROOT_WINDOW);
        let resizes: Vec<_> = commands
            .iter()
            .filter(|command| matches!(command, ViewportCommand::BeginResize(_)))
            .collect();
        check(
            resizes.len() == 1 && count_start_drag(&commands) == 0,
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
        leaves(&h.ws, ROOT_WINDOW) == before,
        "a click on the divider over a split entry split the leaf",
    )?;
    Ok(json!({"split_entry_drag":"StartDrag only","resize":resize,"divider_over_entry":"divider"}))
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
    h.menu(
        ControlKey::LeafBody(b),
        &[MenuItem::LayoutOpen, MenuItem::Detach],
    )?;
    let o = h.ws.observation();
    let pending = o
        .windows
        .iter()
        .find(|w| w.pending_detach == Some(b))
        .map(|w| w.id)
        .ok_or("Detach did not open a window")?;
    check(
        leaves(&h.ws, ROOT_WINDOW) == vec![a, b] && leaves(&h.ws, pending).is_empty(),
        "instance left its layout before the new window existed",
    )?;
    h.ws.check_ownership()?;
    h.frame_window(pending, vec![])?;
    let o = h.ws.observation();
    let module = o.module(b).ok_or("detached instance dropped")?;
    check(
        leaves(&h.ws, ROOT_WINDOW) == vec![a]
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
        !h.ws.has_window(blocked) && leaves(&h.ws, ROOT_WINDOW) == vec![a, c],
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
            && h.ws.has_window(ROOT_WINDOW)
            && leaves(&h.ws, ROOT_WINDOW) == vec![a, c],
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
    h.menu(
        ControlKey::LeafBody(b),
        &[MenuItem::LayoutOpen, MenuItem::MoveTo],
    )?;
    check(
        h.ws.observation().move_source == Some(b),
        "Move to… not armed",
    )?;
    check(
        h.locate(&ControlKey::MoveTarget(MoveTarget::EmptyWindow(window)))
            .is_err(),
        "source window offered itself",
    )?;
    h.focus = ROOT_WINDOW;
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
        leaves(&h.ws, ROOT_WINDOW) == vec![a, b]
            && !h.ws.has_window(window)
            && o.module(b)
                .is_some_and(|m| m.object == b_object && m.channel == Some(ChannelSelection::Left))
            && o.modules_created == created_before
            && o.move_source.is_none(),
        "merge did not move the original object into the target or keep the source window",
    )?;
    check(
        o.window(ROOT_WINDOW).is_some_and(|w| {
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
        h.ws.observation().move_source.is_none() && h.ws.has_window(ROOT_WINDOW),
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
        leaves(&h.ws, empty) == vec![c] && !leaves(&h.ws, ROOT_WINDOW).contains(&c),
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
    check(both.time_pcm_frames > 0, "waveform window not recording")?;
    h.focus = second;
    h.menu(
        ControlKey::LeafBody(w),
        &[MenuItem::WindowOpen, MenuItem::CloseWindow],
    )?;
    check(
        !h.ws.has_window(second) && h.ws.observation().module(w).is_none(),
        "Close window did not release its module",
    )?;
    h.focus = ROOT_WINDOW;
    let before = handle.metrics();
    driver
        .advance_frames(3200, deadline())
        .map_err(|e| e.to_string())?;
    let after = handle.metrics();
    check(
        after.time_pcm_frames == before.time_pcm_frames
            && after.fft_evaluations > before.fft_evaluations,
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
            window_id: ROOT_WINDOW,
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
    h.key(egui::Key::Escape);
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

/// Desktop placement in physical pixels across scales: snapping, grouping and group
/// movement; logical hit testing and grid at 2x; scale change cancels a divider drag.
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
    // Native move of the 2x window close to the root's right edge, then release.
    h.desktop.button_down.set(true);
    h.window_mut(second)?.position = pos2(1007.0, 120.0);
    h.frame_window(second, vec![])?;
    check(
        h.window(second)?.position == pos2(1007.0, 120.0),
        "snapped while the button was still held",
    )?;
    h.desktop.button_down.set(false);
    h.frame_window(second, vec![])?;
    let snapped = h.window(second)?.position;
    check(
        snapped == pos2(1000.0, 100.0),
        &format!("2x window did not snap to the 1x neighbor in physical pixels: {snapped:?}"),
    )?;
    h.focus = second;
    h.menu(
        ControlKey::LeafBody(b),
        &[MenuItem::WindowOpen, MenuItem::Group],
    )?;
    let o = h.ws.observation();
    let group = o.window(second).and_then(|w| w.group);
    check(
        group.is_some() && o.window(ROOT_WINDOW).and_then(|w| w.group) == group,
        "touching windows were not grouped",
    )?;
    // Moving the root moves the member by the same physical delta.
    h.desktop.button_down.set(true);
    h.window_mut(ROOT_WINDOW)?.position = pos2(150.0, 130.0);
    h.frame_window(ROOT_WINDOW, vec![])?;
    h.frame_window(second, vec![])?;
    h.desktop.button_down.set(false);
    h.frame_window(ROOT_WINDOW, vec![])?;
    h.frame_window(second, vec![])?;
    check(
        h.window(second)?.position == pos2(1050.0, 130.0)
            && h.window(ROOT_WINDOW)?.position == pos2(150.0, 130.0),
        &format!(
            "group did not move together: root {:?}, member {:?}",
            h.window(ROOT_WINDOW)?.position,
            h.window(second)?.position
        ),
    )?;
    h.menu(
        ControlKey::LeafBody(b),
        &[MenuItem::WindowOpen, MenuItem::LeaveGroup],
    )?;
    check(
        h.ws.observation().windows.iter().all(|w| w.group.is_none()),
        "Leave group left a one-member group",
    )?;
    h.desktop.button_down.set(true);
    h.window_mut(ROOT_WINDOW)?.position = pos2(400.0, 300.0);
    h.frame_window(ROOT_WINDOW, vec![])?;
    h.desktop.button_down.set(false);
    h.frame_window(ROOT_WINDOW, vec![])?;
    h.frame_window(second, vec![])?;
    check(
        h.window(second)?.position == pos2(1050.0, 130.0),
        "ungrouped window followed",
    )?;
    // Logical hit testing and grid on a 2x window.
    h.focus = ROOT_WINDOW;
    h.window_mut(ROOT_WINDOW)?.ppp = 2.0;
    h.frame(vec![]);
    h.click(ControlKey::SplitZone {
        instance_id: a,
        side: Side::Right,
    })?;
    check(
        leaves(&h.ws, ROOT_WINDOW).len() == 2,
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
    let width = h.window(ROOT_WINDOW)?.size.x;
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
    h.window_mut(ROOT_WINDOW)?.ppp = 1.0;
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
        json!({"snap_physical":[1000,100],"group_follow":[1050,130],"ungrouped":"independent",
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
            !h.ws.has_window(window) && leaves(&h.ws, ROOT_WINDOW) == vec![a, x],
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
            && h.windows.len() == 1
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
            && after.pipelines == baseline.pipelines
            && after.time_pcm_frames == baseline.time_pcm_frames,
        &format!("cycles changed analysis demand: {baseline:?} -> {after:?}"),
    )?;
    Ok(json!({"rounds":10,"windows":1,"modules":1,"subscriptions":after.subscriptions}))
}
