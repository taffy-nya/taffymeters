//! Building blocks shared by the settings tabs: titled cards, rows whose controls
//! start on one column, segmented choices, sliders with readable values and color
//! rows.

use eframe::egui::{self, Align, Layout, RichText, Stroke, vec2};

use super::{SettingsItem, swatches};
use crate::{
    dataflow::ChannelSelection,
    render::Color,
    theme::ColorRole,
    workspace::{
        ColorScope, WorkspaceAction,
        window::{ControlKey, Controls, color32},
    },
};

/// Width of a slider's value readout; fits the widest value ("-160.0 dBFS").
const VALUE_WIDTH: f32 = 86.0;
/// Width kept for a row's ↺, shown or not, so the slider never resizes.
const RESET_WIDTH: f32 = 22.0;
/// Side of a color swatch.
const SWATCH: f32 = 14.0;

pub(super) fn channel_label(index: u16, channel: ChannelSelection) -> String {
    match channel {
        ChannelSelection::Left => "Left".to_owned(),
        ChannelSelection::Right => "Right".to_owned(),
        ChannelSelection::MonoAverage => "Mono".to_owned(),
        ChannelSelection::Discrete(_) => format!("Ch {}", index + 1),
    }
}

pub(super) fn record(controls: &mut Controls, item: SettingsItem, response: &egui::Response) {
    controls.record(ControlKey::Settings(item), response);
}

/// Title line of a tab: what it edits, with a muted detail.
pub(super) fn header(ui: &mut egui::Ui, title: &str, detail: &str) {
    ui.label(RichText::new(title).size(17.0).strong());
    if !detail.is_empty() {
        ui.label(RichText::new(detail).weak());
    }
}

fn card_frame<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let visuals = ui.visuals();
    egui::Frame::new()
        .fill(visuals.faint_bg_color)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(6)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn card_title(ui: &mut egui::Ui, title: &str) {
    ui.label(RichText::new(title).small().strong().weak());
}

/// A titled card holding one group of settings.
pub(super) fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_frame(ui, |ui| {
        if !title.is_empty() {
            card_title(ui, title);
        }
        add(ui)
    })
}

/// A titled card whose title row offers ↺ (`reset`) while `changed`; returns what
/// `add` returns and whether ↺ was clicked.
pub(super) fn card_with_reset<R>(
    ui: &mut egui::Ui,
    title: &str,
    reset: SettingsItem,
    changed: bool,
    controls: &mut Controls,
    add: impl FnOnce(&mut egui::Ui, &mut Controls) -> R,
) -> (R, bool) {
    card_frame(ui, |ui| {
        let mut clicked = false;
        ui.horizontal(|ui| {
            // The title row keeps its height whether ↺ shows or not.
            ui.set_min_height(ui.spacing().interact_size.y);
            card_title(ui, title);
            if changed {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    clicked = reset_button(
                        ui,
                        ControlKey::Settings(reset),
                        "Reset this group to its defaults",
                        controls,
                    );
                });
            }
        });
        (add(ui, controls), clicked)
    })
}

/// A frameless ↺ in the muted text color; it shows a frame while hovered.
pub(super) fn reset_button(
    ui: &mut egui::Ui,
    key: ControlKey,
    hint: &str,
    controls: &mut Controls,
) -> bool {
    let muted = ui.visuals().weak_text_color();
    let response = ui
        .add(
            egui::Button::new(RichText::new("↺").color(muted))
                .frame_when_inactive(false)
                .min_size(vec2(RESET_WIDTH, 0.0)),
        )
        .on_hover_text(hint);
    controls.record(key, &response);
    response.clicked()
}

/// The label column: one width for every row of every card, so controls line up.
fn label_width(ui: &egui::Ui) -> f32 {
    (ui.available_width() * 0.34).clamp(64.0, 128.0)
}

/// A labelled row; `add` lays its controls out left to right, wrapping when narrow.
pub(super) fn row<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    row_with(ui, |ui| ui.add(egui::Label::new(label).truncate()), add)
}

fn row_with<R>(
    ui: &mut egui::Ui,
    label: impl FnOnce(&mut egui::Ui) -> egui::Response,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let width = label_width(ui);
    let height = ui.spacing().interact_size.y;
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(width, height),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_width(width);
                label(ui);
            },
        );
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), height),
            Layout::left_to_right(Align::Center).with_main_wrap(true),
            |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                add(ui)
            },
        )
        .inner
    })
    .inner
}

/// A labelled row whose label column ends in ↺ while `reset` is set: the right
/// edge stays free for the controls. Returns what `add` returns and whether ↺
/// was clicked.
fn row_with_reset<R>(
    ui: &mut egui::Ui,
    label: &str,
    reset: Option<(ControlKey, &str)>,
    controls: &mut Controls,
    add: impl FnOnce(&mut egui::Ui, &mut Controls) -> R,
) -> (R, bool) {
    let width = label_width(ui);
    let height = ui.spacing().interact_size.y;
    ui.horizontal_top(|ui| {
        let clicked = ui
            .allocate_ui_with_layout(
                vec2(width, height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(width);
                    // ↺ takes the end of the label column only while shown; the
                    // controls never move, a long label just truncates sooner.
                    let room = match reset {
                        Some(_) => width - RESET_WIDTH - ui.spacing().item_spacing.x,
                        None => width,
                    };
                    ui.allocate_ui_with_layout(
                        vec2(room, height),
                        Layout::left_to_right(Align::Center),
                        |ui| ui.add(egui::Label::new(label).truncate()),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        reset.is_some_and(|(key, hint)| reset_button(ui, key, hint, controls))
                    })
                    .inner
                },
            )
            .inner;
        let inner = ui
            .allocate_ui_with_layout(
                vec2(ui.available_width(), height),
                Layout::left_to_right(Align::Center).with_main_wrap(true),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    add(ui, controls)
                },
            )
            .inner;
        (inner, clicked)
    })
    .inner
}

/// Controls aligned to the right edge (footer actions), one row high.
pub(super) fn trailing(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let size = vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, Layout::right_to_left(Align::Center), add);
}

/// One option of a segmented choice: framed, filled with the accent when selected.
pub(super) fn segment(ui: &mut egui::Ui, selected: bool, label: &str) -> egui::Response {
    ui.add(
        egui::Button::new(label)
            .selected(selected)
            .frame_when_inactive(true),
    )
}

/// A recorded segment; true when an unselected option is clicked.
pub(super) fn choice(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    item: SettingsItem,
    controls: &mut Controls,
) -> bool {
    let response = segment(ui, selected, label);
    record(controls, item, &response);
    response.clicked() && !selected
}

pub(super) fn button(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
    item: SettingsItem,
    controls: &mut Controls,
) -> bool {
    let response = ui.add_enabled(enabled, egui::Button::new(label));
    record(controls, item, &response);
    response.clicked()
}

/// A button whose action closes, discards or quits.
pub(super) fn danger_button(
    ui: &mut egui::Ui,
    label: &str,
    item: SettingsItem,
    controls: &mut Controls,
) -> bool {
    let color = ui.visuals().error_fg_color;
    let response = ui.add(
        egui::Button::new(RichText::new(label).color(color)).stroke(Stroke::new(1.0_f32, color)),
    );
    record(controls, item, &response);
    response.clicked()
}

/// A hint under a control, in the warning color.
pub(super) fn warning(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let color = ui.visuals().warn_fg_color;
    ui.label(RichText::new(text).color(color))
}

/// `value` with a precision that suits its unit: kHz above 1000 Hz, whole
/// milliseconds, and never "-0".
pub(super) fn format_value(value: f32, unit: &str) -> String {
    let (value, unit, decimals) = match unit {
        "Hz" if value.abs() >= 1_000.0 => (value / 1_000.0, "kHz", 1),
        "Hz" | "ms" => (value, unit, 0),
        "s" if value.abs() >= 10.0 => (value, unit, 0),
        "×" if value.abs() < 10.0 => (value, unit, 2),
        _ => (value, unit, 1),
    };
    let scale = 10_f32.powi(decimals);
    let rounded = (value * scale).round() / scale;
    // Values that round to zero print as "0", not "-0".
    let rounded = if rounded == 0.0 { 0.0 } else { rounded };
    let separator = if unit.is_empty() || unit == "×" {
        ""
    } else {
        " "
    };
    format!(
        "{rounded:.precision$}{separator}{unit}",
        precision = decimals as usize
    )
}

/// A typed value in `unit`: the number alone, or followed by the unit as shown
/// (`-6`, `-6 dB`); Hz also takes kilohertz (`20k`, `20 kHz`). `None` when it
/// is not a finite number in that unit.
pub(super) fn parse_value(text: &str, unit: &str) -> Option<f32> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .unwrap_or(text.len());
    let (number, suffix) = text.split_at(split);
    let number: f32 = number.trim().parse().ok().filter(|n: &f32| n.is_finite())?;
    let suffix = suffix.trim();
    let scale = match suffix {
        "" => 1.0,
        "k" | "kHz" | "khz" | "KHz" if unit == "Hz" => 1_000.0,
        _ if suffix.eq_ignore_ascii_case(unit) => 1.0,
        _ => return None,
    };
    Some(number * scale)
}

/// Whether two setting values differ beyond float noise.
pub(super) fn differs(a: f32, b: f32) -> bool {
    (a - b).abs() > 1e-4 * a.abs().max(b.abs()).max(1.0)
}

/// One numeric setting: slider, typed value and, away from `default`, ↺.
pub(super) struct Number<'a> {
    pub label: &'a str,
    pub key: SettingsItem,
    pub current: f32,
    pub range: (f32, f32),
    pub unit: &'a str,
    pub logarithmic: bool,
    /// What ↺ restores (kept inside the range); `None` offers no ↺.
    pub default: Option<f32>,
}

/// One slider row; returns the new value whenever it changes, so dragging
/// applies continuously. Clicking the value types one (Enter or clicking
/// elsewhere applies it, Escape keeps the old one).
pub(super) fn number_row(
    ui: &mut egui::Ui,
    number: Number<'_>,
    controls: &mut Controls,
) -> Option<f32> {
    let Number {
        label,
        key,
        current,
        range: (min, max),
        unit,
        logarithmic,
        default,
    } = number;
    let default = default.map(|default| default.clamp(min, max));
    let hint = default.map(|default| format!("Reset to {}", format_value(default, unit)));
    let reset = default
        .filter(|default| differs(current, *default))
        .zip(hint.as_deref())
        .map(|(_, hint)| (ControlKey::SettingsReset(key), hint));
    let mut value = current;
    let (_, reset_clicked) = row_with_reset(ui, label, reset, controls, |ui, controls| {
        // The slider takes what the value leaves, so the row stays one line down
        // to the narrowest window.
        let spacing = ui.spacing().item_spacing.x;
        ui.spacing_mut().slider_width = (ui.available_width() - VALUE_WIDTH - spacing).max(32.0);
        let response = ui.add(
            egui::Slider::new(&mut value, min..=max)
                .logarithmic(logarithmic)
                .show_value(false)
                .trailing_fill(true),
        );
        record(controls, key, &response);
        // The value reads as plain text; hovering frames it as a field to type
        // in. It and its edit field both take the interact width.
        ui.spacing_mut().interact_size.x = VALUE_WIDTH;
        let resting = &mut ui.visuals_mut().widgets.inactive;
        resting.weak_bg_fill = egui::Color32::TRANSPARENT;
        resting.bg_stroke = Stroke::NONE;
        let typed = ui
            .add(
                egui::DragValue::new(&mut value)
                    .range(min..=max)
                    // Typed, not dragged: a linear drag fights the log sliders.
                    .speed(0.0)
                    .update_while_editing(false)
                    .custom_formatter(|value, _| format_value(value as f32, unit))
                    .custom_parser(|text| parse_value(text, unit).map(f64::from)),
            )
            .on_hover_cursor(egui::CursorIcon::Text);
        controls.record(ControlKey::SettingsValue(key), &typed);
    });
    if reset_clicked && let Some(default) = default {
        value = default;
    }
    differs(value, current).then_some(value)
}

/// One color role: quick swatches (clicking one sets this scope's color) and the
/// full picker; while this scope sets its own color, ↺ in the label column returns
/// it to the inherited one.
pub(super) fn color_row(
    ui: &mut egui::Ui,
    scope: ColorScope,
    role: ColorRole,
    current: Color,
    overridden: bool,
    controls: &mut Controls,
    actions: &mut Vec<WorkspaceAction>,
) {
    let accent = ui.visuals().selection.bg_fill;
    let reset = overridden.then_some((
        ControlKey::Settings(SettingsItem::ResetColor { scope, role }),
        "Use the inherited color",
    ));
    let (_, reset_clicked) = row_with_reset(ui, role.label(), reset, controls, |ui, controls| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let ring = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let edge = ui.visuals().widgets.noninteractive.bg_stroke;
        for (index, swatch) in swatches(ui.visuals().dark_mode).iter().enumerate() {
            let active = overridden && *swatch == current;
            // Painted at a fixed size: the selection ring is drawn inside, so it
            // never moves its neighbors.
            let (rect, response) =
                ui.allocate_exact_size(vec2(SWATCH, SWATCH), egui::Sense::click());
            let stroke = if active {
                Stroke::new(2.0_f32, ring)
            } else if response.hovered() {
                Stroke::new(1.0_f32, accent)
            } else {
                edge
            };
            let painter = ui.painter();
            painter.rect_filled(rect, 2.0, color32(*swatch));
            painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
            record(
                controls,
                SettingsItem::Swatch {
                    scope,
                    role,
                    index: index as u8,
                },
                &response,
            );
            if response.clicked() && !active {
                actions.push(WorkspaceAction::SetColor {
                    scope,
                    role,
                    color: Some(*swatch),
                });
            }
        }
        ui.add_space(4.0);
        let mut picked = color32(current);
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut picked,
            egui::color_picker::Alpha::OnlyBlend,
        )
        .changed()
        {
            let [red, green, blue, alpha] = picked.to_srgba_unmultiplied();
            actions.push(WorkspaceAction::SetColor {
                scope,
                role,
                color: Some(Color::rgba(red, green, blue, alpha)),
            });
        }
    });
    if reset_clicked {
        actions.push(WorkspaceAction::SetColor {
            scope,
            role,
            color: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{format_value, parse_value};

    #[test]
    fn values_read_in_their_unit_without_negative_zero() {
        assert_eq!(format_value(20_000.0, "Hz"), "20.0 kHz");
        assert_eq!(format_value(440.4, "Hz"), "440 Hz");
        assert_eq!(format_value(1_000.0, "ms"), "1000 ms");
        assert_eq!(format_value(-0.04, "dB"), "0.0 dB");
        assert_eq!(format_value(-160.0, "dBFS"), "-160.0 dBFS");
        assert_eq!(format_value(0.5, "s"), "0.5 s");
        assert_eq!(format_value(30.0, "s"), "30 s");
        assert_eq!(format_value(1.5, "×"), "1.50×");
        assert_eq!(format_value(3.0, ""), "3.0");
    }

    #[test]
    fn typed_values_take_the_number_with_or_without_its_unit() {
        assert_eq!(parse_value(" -6 ", "dB"), Some(-6.0));
        assert_eq!(parse_value("-6 dB", "dB"), Some(-6.0));
        assert_eq!(parse_value("-160dbfs", "dBFS"), Some(-160.0));
        assert_eq!(parse_value("440", "Hz"), Some(440.0));
        assert_eq!(parse_value("20k", "Hz"), Some(20_000.0));
        assert_eq!(parse_value("1.5 kHz", "Hz"), Some(1_500.0));
        assert_eq!(parse_value("+3", "dB/oct"), Some(3.0));
        assert_eq!(parse_value("2×", "×"), Some(2.0));
        // A kilo prefix only means something for frequencies; other units and
        // text are not numbers in this unit.
        assert_eq!(parse_value("20k", "ms"), None);
        assert_eq!(parse_value("5 s", "ms"), None);
        assert_eq!(parse_value("abc", "dB"), None);
        assert_eq!(parse_value("", "dB"), None);
        assert_eq!(parse_value("1e40", "dB"), None);
        assert_eq!(parse_value("--1", "dB"), None);
    }
}
