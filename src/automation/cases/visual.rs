use std::{path::Path, time::Duration};

use serde_json::{Value, json};

use crate::{
    automation::{
        harness::Harness,
        report::{self, Case, Report, Status},
        support::{
            action, check, created, deadline, first_leaf, replace_first, rgba_view, workspace,
        },
        visual,
    },
    modules::{EMPTY_MODULE_ID, LEVEL_METER_MODULE_ID, SPECTRUM_MODULE_ID, WAVEFORM_MODULE_ID},
    workspace::{FIRST_WINDOW, Side, Workspace, WorkspaceAction},
};

pub(in crate::automation) fn compare_case(expected: &Path, actual: &Path, output: &Path) -> Case {
    match visual::compare_png_paths(expected, actual) {
        Ok(metrics) if metrics.dimensions_match && metrics.changed_pixels == Some(0) => {
            Case::pass("compare", json!({"comparison":metrics}))
        }
        Ok(metrics) => {
            let mut case = Case::fail(
                "compare",
                "pixel comparison",
                format!("different pixels or dimensions: {metrics:?}"),
            );
            case.expected = json!({"path":expected});
            case.observed = json!({"comparison":metrics,"path":actual});
            if metrics.dimensions_match
                && let (Ok(a), Ok(b)) = (image::open(expected), image::open(actual))
            {
                let diff = output.join("diff.png");
                let a = a.to_rgba8();
                let b = b.to_rgba8();
                if visual::save_diff_png(&diff, rgba_view(&a), rgba_view(&b)).is_ok() {
                    case.artifacts.push("diff.png".into());
                }
            }
            case
        }
        Err(e) => Case::fail("compare", "PNG decode", e),
    }
}

fn capture_frame(
    renderer: &mut visual::OffscreenRenderer,
    h: &mut Harness,
) -> Result<visual::Rendered, String> {
    let first = h.frame(vec![]);
    let context = h.context();
    let size = h.window(h.focus)?.size;
    renderer
        .render(&context, first, size.x as u32, size.y as u32)
        .map_err(|e| e.to_string())
}

/// Setup for an image of a single full-window Module with an optional style.
fn single_module(
    type_id: &'static str,
    style: Option<&'static str>,
) -> impl FnOnce(&mut Workspace) -> Result<(), String> {
    move |ws| {
        let id = if type_id == SPECTRUM_MODULE_ID {
            first_leaf(ws)?
        } else {
            replace_first(ws, type_id)?
        };
        if let Some(style_id) = style {
            action(
                ws,
                WorkspaceAction::SetStyle {
                    instance_id: id,
                    style_id,
                },
            )?;
        }
        Ok(())
    }
}

fn workspace_layout(ws: &mut Workspace) -> Result<(), String> {
    let spectrum = first_leaf(ws)?;
    let waveform = created(action(
        ws,
        WorkspaceAction::SplitModule {
            instance_id: spectrum,
            side: Side::Right,
        },
    )?)?;
    let waveform = created(action(
        ws,
        WorkspaceAction::ReplaceModule {
            instance_id: waveform,
            type_id: WAVEFORM_MODULE_ID,
        },
    )?)?;
    let levels = created(action(
        ws,
        WorkspaceAction::SplitModule {
            instance_id: waveform,
            side: Side::Below,
        },
    )?)?;
    action(
        ws,
        WorkspaceAction::ReplaceModule {
            instance_id: levels,
            type_id: LEVEL_METER_MODULE_ID,
        },
    )?;
    Ok(())
}

fn themed_layout(ws: &mut Workspace) -> Result<(), String> {
    workspace_layout(ws)?;
    let spectrum = first_leaf(ws)?;
    action(
        ws,
        WorkspaceAction::SetThemePreset {
            preset: crate::theme::Preset::Light,
        },
    )?;
    action(
        ws,
        WorkspaceAction::SetColor {
            scope: crate::workspace::ColorScope::Instance(spectrum),
            role: crate::theme::ColorRole::Primary,
            color: Some(crate::render::Color::rgb(220, 40, 90)),
        },
    )?;
    Ok(())
}

fn image_case(
    renderer: &mut visual::OffscreenRenderer,
    id: &str,
    setup: impl FnOnce(&mut Workspace) -> Result<(), String>,
    dpi: f32,
    output: &Path,
    baseline: Option<&Path>,
) -> Result<(Case, Value), String> {
    let (mut ws, mut driver) = workspace()?;
    setup(&mut ws)?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.window_mut(FIRST_WINDOW)?.ppp = dpi;
    capture_frame(renderer, &mut h)?;
    driver
        .advance_frames(48_000, deadline())
        .map_err(|e| e.to_string())?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            >= 48_000,
        "input not processed",
    )?;
    let frame = capture_frame(renderer, &mut h)?;
    let image = image::RgbaImage::from_raw(frame.pixel_size[0], frame.pixel_size[1], frame.rgba8)
        .ok_or("invalid readback length")?;
    let hash = report::digest(image.as_raw());
    let manifest = json!({"case_id":id,"scene_version":1,"frame_token":h.frame,
        "egui_frame_nr":frame.egui_frame_nr,"egui_pass_nr":frame.egui_pass_nr,
        "gpu_submission_id":frame.submission_id,"readback_submission_id":frame.submission_id,
        "source": {"id":h.ws.source_descriptor_for_automation().source_id.0,
        "generation":h.ws.source_descriptor_for_automation().generation.0}, "delivered_through_frame":48_000,
        "processed_through_frame":handle.processed_through_frame().map_err(|e|e.to_string())?,
        "result_revision":null,"result_version":null,
        "result_marker_note":"Module trait does not expose subscription snapshots; no extra analysis probe is created",
        "display_time_seconds":h.display_time.as_secs_f64(),"logical_size":frame.logical_size,
        "pixel_size":frame.pixel_size,"pixels_per_point":frame.pixels_per_point,
        "signal":"48k stereo left 3000Hz/right 6000Hz peak 0.08 block 160",
        "renderer":frame.renderer,"font":"egui bundled defaults","theme":h.ws.observation().theme.preset,"os":std::env::consts::OS,
        "dependency":{"egui":"0.34.1","egui-wgpu":"0.34.1","wgpu":"29.0.1"},"pixel_sha256":hash});
    let candidate = output.join(format!("{id}.png"));
    let manifest_path = output.join(format!("{id}.json"));
    let mut case = judge_baseline(id, &manifest, &image, baseline, output);
    // Retain the actual image whenever it is not an approved pixel match.
    if case.status != Status::Pass {
        visual::save_png(
            &candidate,
            frame.pixel_size[0],
            frame.pixel_size[1],
            image.as_raw(),
        )
        .map_err(|e| e.to_string())?;
        case.artifacts.push(format!("images/{id}.png"));
    }
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    case.artifacts.push(format!("images/{id}.json"));
    case.observed = json!({"pixel_sha256":hash,"frame_token":h.frame,"processed_through_frame":48_000,"comparison":case.observed});
    Ok((case, manifest))
}

/// Judge one rendered image against a reviewed baseline directory. A missing or
/// unapproved baseline is never auto-accepted; integrity problems are failures.
fn judge_baseline(
    id: &str,
    manifest: &Value,
    image: &image::RgbaImage,
    baseline: Option<&Path>,
    output: &Path,
) -> Case {
    let Some(base) = baseline else {
        return Case::incomplete(
            id,
            Status::NeedsReview,
            "candidate requires human baseline review",
        );
    };
    if !base.join("approval.json").is_file() {
        return Case::incomplete(
            id,
            Status::NeedsReview,
            "baseline exists but approval.json is missing",
        );
    }
    compare_with_baseline(id, manifest, image, base, output)
        .unwrap_or_else(|error| Case::fail(id, "baseline integrity", error))
}

fn read_json(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn compare_with_baseline(
    id: &str,
    manifest: &Value,
    image: &image::RgbaImage,
    base: &Path,
    output: &Path,
) -> Result<Case, String> {
    const COMPARABLE: [&str; 11] = [
        "case_id",
        "scene_version",
        "logical_size",
        "pixel_size",
        "pixels_per_point",
        "signal",
        "renderer",
        "font",
        "theme",
        "os",
        "dependency",
    ];
    let approved = read_json(&base.join("approval.json"))?;
    check(
        approved.get("reviewer").and_then(Value::as_str).is_some()
            && approved.get("date").and_then(Value::as_str).is_some()
            && approved.get("reason").and_then(Value::as_str).is_some(),
        "baseline lacks reviewer/date/reason",
    )?;
    let previous = read_json(&base.join(format!("{id}.json")))?;
    check(
        approved["candidate_hashes"][id] == previous["pixel_sha256"]
            && previous["pixel_sha256"].as_str().is_some(),
        "baseline approval hash mismatches manifest",
    )?;
    if COMPARABLE
        .iter()
        .any(|field| previous[*field] != manifest[*field])
    {
        return Ok(Case::incomplete(
            id,
            Status::NotComparable,
            "baseline profile differs",
        ));
    }
    let base_image = base.join(format!("{id}.png"));
    let expected = visual::load_png(&base_image).map_err(|e| e.to_string())?;
    check(
        report::digest(&expected.rgba8) == previous["pixel_sha256"],
        "baseline PNG hash mismatches manifest",
    )?;
    let metrics =
        visual::compare_rgba(expected.as_view(), rgba_view(image)).map_err(|e| e.to_string())?;
    if metrics.dimensions_match && metrics.changed_pixels == Some(0) {
        return Ok(Case::pass(id, json!({"comparison":metrics})));
    }
    let mut fail = Case::fail(id, "pixel comparison", format!("{metrics:?}"));
    fail.expected = json!({"path":base_image});
    fail.observed = json!({"comparison":metrics});
    if metrics.dimensions_match {
        let diff = format!("{id}-diff.png");
        visual::save_diff_png(&output.join(&diff), expected.as_view(), rgba_view(image))
            .map_err(|e| e.to_string())?;
        fail.artifacts.push(format!("images/{diff}"));
    }
    Ok(fail)
}

fn repeat_frame(renderer: &mut visual::OffscreenRenderer) -> Result<image::RgbaImage, String> {
    let (ws, mut driver) = workspace()?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    capture_frame(renderer, &mut h)?;
    driver
        .advance_frames(48_000, deadline())
        .map_err(|error| error.to_string())?;
    check(
        handle
            .processed_through_frame()
            .map_err(|error| error.to_string())?
            >= 48_000,
        "repeat input not processed",
    )?;
    let frame = capture_frame(renderer, &mut h)?;
    image::RgbaImage::from_raw(frame.pixel_size[0], frame.pixel_size[1], frame.rgba8)
        .ok_or_else(|| "invalid repeat readback length".to_owned())
}

fn persistence_visual(
    renderer: &mut visual::OffscreenRenderer,
    output: &Path,
    baseline: Option<&Path>,
) -> Result<Case, String> {
    let (mut ws, mut driver) = workspace()?;
    let osc = replace_first(&mut ws, crate::modules::OSCILLOSCOPE_MODULE_ID)?;
    action(
        &mut ws,
        WorkspaceAction::SetStyle {
            instance_id: osc,
            style_id: crate::modules::PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        },
    )?;
    let handle = ws.dataflow_handle_for_automation();
    let mut h = Harness::new(ws);
    h.display_time = Duration::ZERO;
    capture_frame(renderer, &mut h)?;
    driver
        .advance_frames(1600, deadline())
        .map_err(|e| e.to_string())?;
    h.display_time = Duration::from_millis(300);
    let fresh = capture_frame(renderer, &mut h)?;
    h.display_time = Duration::from_millis(1300);
    let expired = capture_frame(renderer, &mut h)?;
    check(
        fresh.rgba8 != expired.rgba8,
        "Persistence did not change when its trace expired",
    )?;
    check(
        handle
            .processed_through_frame()
            .map_err(|e| e.to_string())?
            == 1600,
        "display-only frames changed audio frontier",
    )?;
    let approved = baseline.filter(|path| path.join("approval.json").is_file());
    let mut case = if approved.is_some() {
        Case::pass("V03-persistence", Value::Null)
    } else {
        Case::incomplete(
            "V03-persistence",
            Status::NeedsReview,
            "fixed-time candidate frames require visual review",
        )
    };
    for (name, rendered, time) in [("fresh", &fresh, 0.3), ("expired", &expired, 1.3)] {
        let id = format!("V03-persistence-{name}");
        let hash = report::digest(&rendered.rgba8);
        if let Some(base) = approved {
            let approval: Value = serde_json::from_slice(
                &std::fs::read(base.join("approval.json")).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let previous: Value = serde_json::from_slice(
                &std::fs::read(base.join(format!("{id}.json"))).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            if approval["candidate_hashes"][&id] != previous["pixel_sha256"] {
                return Err(format!("{id} baseline approval hash mismatch"));
            }
            let previous_image =
                visual::load_png(&base.join(format!("{id}.png"))).map_err(|e| e.to_string())?;
            if report::digest(&previous_image.rgba8) != previous["pixel_sha256"] {
                return Err(format!("{id} baseline file hash mismatch"));
            }
            if previous["renderer"] != json!(rendered.renderer)
                || previous["pixel_size"] != json!(rendered.pixel_size)
                || previous["display_time_seconds"] != json!(time)
            {
                case.status = Status::NotComparable;
                case.error = Some(format!("{id} profile differs"));
            } else if visual::compare_rgba(
                previous_image.as_view(),
                visual::Rgba8View {
                    width: rendered.pixel_size[0],
                    height: rendered.pixel_size[1],
                    rgba8: &rendered.rgba8,
                },
            )
            .map_err(|e| e.to_string())?
            .changed_pixels
                != Some(0)
            {
                case = Case::fail(
                    "V03-persistence",
                    "baseline pixels",
                    format!("{id} differs"),
                );
            }
        }
        let file = format!("V03-persistence-{name}.png");
        if case.status != Status::Pass {
            visual::save_png(
                &output.join(&file),
                rendered.pixel_size[0],
                rendered.pixel_size[1],
                &rendered.rgba8,
            )
            .map_err(|e| e.to_string())?;
        }
        let manifest = format!("V03-persistence-{name}.json");
        std::fs::write(output.join(&manifest),serde_json::to_vec_pretty(&json!({
            "case_id":"V03-persistence","scene_version":1,"frame_token":h.frame,
            "egui_frame_nr":rendered.egui_frame_nr,"egui_pass_nr":rendered.egui_pass_nr,
            "gpu_submission_id":rendered.submission_id,"readback_submission_id":rendered.submission_id,
            "source":h.ws.source_descriptor_for_automation().source_id.0,"generation":h.ws.source_descriptor_for_automation().generation.0,
            "delivered_through_frame":1600,"processed_through_frame":1600,
            "result_version":null,"result_marker_note":"no extra analysis probe",
            "display_time_seconds":time,"logical_size":rendered.logical_size,"pixel_size":rendered.pixel_size,
            "pixels_per_point":rendered.pixels_per_point,"renderer":rendered.renderer,
            "font":"egui bundled defaults","theme":"dark","pixel_sha256":hash
        })).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
        if case.status != Status::Pass {
            case.artifacts.push(format!("images/{file}"));
        }
        case.artifacts.push(format!("images/{manifest}"));
    }
    case.observed = json!({"fresh_sha256":report::digest(&fresh.rgba8),"expired_sha256":report::digest(&expired.rgba8)});
    Ok(case)
}

pub(in crate::automation) fn visual(report: &mut Report, output: &Path, baseline: Option<&Path>) {
    // V05 checks that the run never modifies a supplied baseline.
    let baseline_before = baseline.map(tree_digest);
    let mut renderer = match visual::OffscreenRenderer::new() {
        Ok(renderer) => renderer,
        Err(visual::VisualError::Initialization { stage, message }) => {
            report.cases.push(Case::incomplete(
                "V01",
                Status::Unavailable,
                format!("{stage}: {message}"),
            ));
            for id in ["V02", "V03", "V04", "V05", "V06", "V07"] {
                report.cases.push(Case::incomplete(
                    id,
                    Status::NotRun,
                    "V01 adapter/device unavailable",
                ));
            }
            return;
        }
        Err(e) => {
            report
                .cases
                .push(Case::fail("V01", "renderer initialization", e));
            for id in ["V02", "V03", "V04", "V05", "V06", "V07"] {
                report
                    .cases
                    .push(Case::incomplete(id, Status::NotRun, "V01 failed"));
            }
            return;
        }
    };
    if let Err(e) = std::fs::create_dir_all(output.join("images")) {
        report.cases.push(Case::fail("V01", "output images", e));
        return;
    }
    let cases = [
        ("spectrum-curve", SPECTRUM_MODULE_ID, None),
        (
            "spectrum-bars",
            SPECTRUM_MODULE_ID,
            Some(crate::modules::BAR_SPECTRUM_STYLE_ID),
        ),
        (
            "spectrum-pixel",
            SPECTRUM_MODULE_ID,
            Some(crate::modules::PIXEL_SPECTRUM_STYLE_ID),
        ),
        (
            "oscilloscope-line",
            crate::modules::OSCILLOSCOPE_MODULE_ID,
            None,
        ),
        (
            "oscilloscope-persistence",
            crate::modules::OSCILLOSCOPE_MODULE_ID,
            Some(crate::modules::PERSISTENCE_OSCILLOSCOPE_STYLE_ID),
        ),
        ("spectrogram", crate::modules::SPECTROGRAM_MODULE_ID, None),
        ("waveform", WAVEFORM_MODULE_ID, None),
        ("stereometer", crate::modules::STEREOMETER_MODULE_ID, None),
        ("level-meter", LEVEL_METER_MODULE_ID, None),
        ("empty", EMPTY_MODULE_ID, None),
    ];
    let images = output.join("images");
    match image_case(
        &mut renderer,
        "spectrum-curve",
        single_module(SPECTRUM_MODULE_ID, None),
        1.0,
        &images,
        baseline,
    ) {
        Ok((case, manifest)) => {
            report.environment["renderer"] = manifest["renderer"].clone();
            report.cases.push(Case::pass(
                "V01",
                json!({"renderer":manifest["renderer"],"frame_token":manifest["frame_token"]}),
            ));
            let mut results = vec![case];
            for (id, type_id, style) in cases.into_iter().skip(1) {
                match image_case(
                    &mut renderer,
                    id,
                    single_module(type_id, style),
                    1.0,
                    &images,
                    baseline,
                ) {
                    Ok((case, _)) => results.push(case),
                    Err(e) => results.push(Case::fail(id, "real frame", e)),
                }
            }
            let mut summary = Case::pass("V02", json!({"image_cases":results.len()}));
            if let Some(failure) = results.iter().find(|case| case.status == Status::Fail) {
                summary = Case::fail(
                    "V02",
                    "image",
                    format!("{} failed: {:?}", failure.id, failure.error),
                );
            } else if results
                .iter()
                .any(|case| case.status == Status::NotComparable)
            {
                summary = Case::incomplete("V02", Status::NotComparable, "profile differs");
            } else if results
                .iter()
                .any(|case| case.status == Status::NeedsReview)
            {
                summary =
                    Case::incomplete("V02", Status::NeedsReview, "candidate images need review");
            }
            report.cases.push(summary);
            report.cases.extend(results);
        }
        Err(e) => {
            report.cases.push(Case::fail("V01", "real frame", e));
            for id in ["V02", "V03", "V04", "V05", "V06", "V07"] {
                report
                    .cases
                    .push(Case::incomplete(id, Status::NotRun, "V01 failed"));
            }
            return;
        }
    }
    report.cases.push(
        match image_case(
            &mut renderer,
            "spectrum-2x",
            single_module(SPECTRUM_MODULE_ID, None),
            2.0,
            &images,
            baseline,
        ) {
            Ok((case, _)) => {
                let mut result = Case::pass(
                    "V03",
                    json!({"two_x_case":case.status,"candidate":case.observed}),
                );
                result.status = case.status;
                result.error = case.error;
                result.artifacts = case.artifacts;
                result
            }
            Err(e) => Case::fail("V03", "2x DPI frame", e),
        },
    );
    // V06: a three-leaf BSP layout (Spectrum | Waveform over Level Meter) at 1x and
    // 2x; layout, dividers and per-leaf clipping come from the production UI body.
    for (id, dpi) in [("workspace-layout", 1.0), ("workspace-layout-2x", 2.0)] {
        report.cases.push(
            match image_case(&mut renderer, id, workspace_layout, dpi, &images, baseline) {
                Ok((mut case, _)) => {
                    case.id = format!("V06-{id}");
                    case
                }
                Err(e) => Case::fail(&format!("V06-{id}"), "workspace layout frame", e),
            },
        );
    }
    // V07: the same layout under the Light preset with one instance override; the
    // chrome, egui visuals and every Module follow the theme, the override stays.
    report.cases.push(
        match image_case(
            &mut renderer,
            "theme-light",
            themed_layout,
            1.0,
            &images,
            baseline,
        ) {
            Ok((mut case, _)) => {
                case.id = "V07-theme-light".to_owned();
                case
            }
            Err(e) => Case::fail("V07-theme-light", "themed layout frame", e),
        },
    );
    report
        .cases
        .push(match persistence_visual(&mut renderer, &images, baseline) {
            Ok(case) => case,
            Err(error) => Case::fail("V03-persistence", "fixed-time render", error),
        });
    report.cases.push(
        match (repeat_frame(&mut renderer), repeat_frame(&mut renderer)) {
            (Ok(a), Ok(b)) => repeat_case(&a, &b),
            (a, b) => Case::fail(
                "V04",
                "repeat capture",
                format!("first: {:?}, second: {:?}", a.err(), b.err()),
            ),
        },
    );
    let guards = baseline_guards(&output.join("v05-guards"));
    let baseline_after = baseline.map(tree_digest);
    report
        .cases
        .push(match (guards, baseline_before, baseline_after) {
            (Err(error), _, _) => Case::fail("V05", "baseline guard", error),
            (Ok(_), Some(Err(error)), _) | (Ok(_), _, Some(Err(error))) => {
                Case::fail("V05", "baseline digest", error)
            }
            (Ok(_), Some(Ok(before)), Some(Ok(after))) if before != after => Case::fail(
                "V05",
                "baseline read-only",
                "visual run modified the baseline directory",
            ),
            (Ok(mut observed), before, _) => {
                observed["real_baseline"] = match before {
                    Some(Ok(files)) => json!({"files":files.len(),"modified":false}),
                    _ => json!("none supplied"),
                };
                let mut case = Case::pass("V05", observed);
                case.artifacts.push("v05-guards".into());
                case
            }
        });
}

/// V04: two independent scene runs must match, and a one-pixel change to the
/// comparator input must be detected and located.
fn repeat_case(a: &image::RgbaImage, b: &image::RgbaImage) -> Case {
    let repeat = match visual::compare_rgba(rgba_view(a), rgba_view(b)) {
        Ok(metrics) if metrics.dimensions_match && metrics.changed_pixels == Some(0) => metrics,
        Ok(metrics) => {
            return Case::fail("V04", "independent scene repeat", format!("{metrics:?}"));
        }
        Err(error) => return Case::fail("V04", "compare", error),
    };
    let mut negative = b.clone();
    let (x, y) = (b.width() / 2, b.height() / 2);
    let pixel = negative.get_pixel_mut(x, y);
    pixel.0[0] ^= 0x01;
    match visual::compare_rgba(rgba_view(a), rgba_view(&negative)) {
        Ok(metrics)
            if metrics.changed_pixels == Some(1)
                && metrics.diff_bbox.is_some_and(|bbox| {
                    bbox.min_x == x && bbox.max_x == x && bbox.min_y == y && bbox.max_y == y
                }) =>
        {
            Case::pass(
                "V04",
                json!({"repeat":repeat,"pixel_sha256":report::digest(a.as_raw()),
                    "negative":{"pixel":[x,y],"comparison":metrics,"result":"FAIL detected"}}),
            )
        }
        Ok(metrics) => Case::fail(
            "V04",
            "one-pixel negative",
            format!("changed pixel not detected/located: {metrics:?}"),
        ),
        Err(error) => Case::fail("V04", "one-pixel negative", error),
    }
}

/// Relative path and SHA-256 of every file below `root`, sorted.
fn tree_digest(root: &Path) -> Result<Vec<(String, String)>, String> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                let relative = path.strip_prefix(root).unwrap_or(&path);
                files.push((relative.display().to_string(), report::digest(&bytes)));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// V05: drive `judge_baseline` through every protection path with small local
/// baselines and require the documented status for each; none may be modified.
fn baseline_guards(root: &Path) -> Result<Value, String> {
    const ID: &str = "V05-probe";
    let io = |e: std::io::Error| e.to_string();
    let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([10, 20, 30, 255]));
    let hash = report::digest(image.as_raw());
    let manifest = |renderer: &str| {
        json!({"case_id":ID,"scene_version":1,"logical_size":[4.0,4.0],"pixel_size":[4,4],
            "pixels_per_point":1.0,"signal":"constant probe","renderer":renderer,
            "font":"egui bundled defaults","theme":"dark","os":std::env::consts::OS,
            "dependency":{},"pixel_sha256":hash})
    };
    let current = manifest("probe-renderer");
    let write_baseline =
        |name: &str, stored: &Value, png: Option<&[u8]>, approved: bool| -> Result<_, String> {
            let dir = root.join(name);
            std::fs::create_dir_all(&dir).map_err(io)?;
            std::fs::write(
                dir.join(format!("{ID}.json")),
                serde_json::to_vec_pretty(stored).map_err(|e| e.to_string())?,
            )
            .map_err(io)?;
            match png {
                Some(bytes) => std::fs::write(dir.join(format!("{ID}.png")), bytes).map_err(io)?,
                None => visual::save_png(&dir.join(format!("{ID}.png")), 4, 4, image.as_raw())
                    .map_err(|e| e.to_string())?,
            }
            if approved {
                std::fs::write(
                    dir.join("approval.json"),
                    serde_json::to_vec_pretty(&json!({"reviewer":"V05 guard probe",
                        "date":"1970-01-01","reason":"synthetic guard fixture",
                        "candidate_hashes":{ID:hash}}))
                    .map_err(|e| e.to_string())?,
                )
                .map_err(io)?;
            }
            Ok(dir)
        };
    let mut changed = image.clone();
    changed.put_pixel(2, 1, image::Rgba([11, 20, 30, 255]));
    let out = root.join("out");
    std::fs::create_dir_all(&out).map_err(io)?;
    let fixtures = [
        ("missing", None, &image, Status::NeedsReview),
        (
            "unapproved",
            Some(write_baseline("unapproved", &current, None, false)?),
            &image,
            Status::NeedsReview,
        ),
        (
            "profile",
            Some(write_baseline(
                "profile",
                &manifest("other-renderer"),
                None,
                true,
            )?),
            &image,
            Status::NotComparable,
        ),
        (
            "corrupt",
            Some(write_baseline(
                "corrupt",
                &current,
                Some(b"not a png"),
                true,
            )?),
            &image,
            Status::Fail,
        ),
        (
            "match",
            Some(write_baseline("match", &current, None, true)?),
            &image,
            Status::Pass,
        ),
        (
            "pixel",
            Some(write_baseline("pixel", &current, None, true)?),
            &changed,
            Status::Fail,
        ),
    ];
    let mut observed = serde_json::Map::new();
    for (name, dir, actual, expected) in fixtures {
        let before = dir.as_deref().map(tree_digest).transpose()?;
        let base = dir.as_deref();
        let case = judge_baseline(ID, &current, actual, base, &out);
        if case.status != expected {
            return Err(format!(
                "{name}: expected {expected:?}, got {:?} ({:?})",
                case.status, case.error
            ));
        }
        if name == "pixel" && !out.join(format!("{ID}-diff.png")).is_file() {
            return Err("pixel failure did not save a diff image".into());
        }
        if name == "pixel" && case.expected["path"].as_str().is_none() {
            return Err("pixel failure did not record the baseline path".into());
        }
        if dir.as_deref().map(tree_digest).transpose()? != before {
            return Err(format!("{name}: judging modified the baseline"));
        }
        observed.insert(
            name.into(),
            json!({"status":case.status,"error":case.error}),
        );
    }
    Ok(Value::Object(observed))
}
