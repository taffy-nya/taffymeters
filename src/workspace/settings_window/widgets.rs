//! Building blocks shared by the settings tabs: titled cards, rows whose controls
//! start on one column, segmented choices, sliders with readable values and color
//! rows.

use eframe::egui::{self, Align, Layout, RichText, Stroke, vec2};

use super::{SWATCHES, SettingsItem};
use crate::{
    dataflow::ChannelSelection,
    render::Color,
    theme::ColorRole,
    workspace::{
        ColorScope, WorkspaceAction,
        window::{ControlKey, Controls, color32},
    },
};

/// Width of a slider's value readout.
const VALUE_WIDTH: f32 = 80.0;
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

/// A titled card holding one group of settings.
pub(super) fn card<R>(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let visuals = ui.visuals();
    egui::Frame::new()
        .fill(visuals.faint_bg_color)
        .stroke(visuals.widgets.noninteractive.bg_stroke)
        .corner_radius(6)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).small().strong().weak());
            }
            add(ui)
        })
        .inner
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
pub(super) fn warning(ui: &mut egui::Ui, text: &str) {
    let color = ui.visuals().warn_fg_color;
    ui.label(RichText::new(text).color(color));
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
    let scale = 10f32.powi(decimals);
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

/// One slider row; returns the new value whenever it changes, so dragging
/// applies continuously.
#[allow(clippy::too_many_arguments)]
pub(super) fn number_row(
    ui: &mut egui::Ui,
    label: &str,
    key: SettingsItem,
    current: f32,
    (min, max): (f32, f32),
    unit: &str,
    logarithmic: bool,
    controls: &mut Controls,
) -> Option<f32> {
    let mut value = current;
    row(ui, label, |ui| {
        let spacing = ui.spacing().item_spacing.x;
        ui.spacing_mut().slider_width = (ui.available_width() - VALUE_WIDTH - spacing).max(48.0);
        let response = ui.add(
            egui::Slider::new(&mut value, min..=max)
                .logarithmic(logarithmic)
                .show_value(false)
                .trailing_fill(true),
        );
        record(controls, key, &response);
        ui.allocate_ui_with_layout(
            vec2(VALUE_WIDTH, ui.spacing().interact_size.y),
            Layout::right_to_left(Align::Center),
            |ui| ui.label(format_value(value, unit)),
        );
    });
    ((value - current).abs() > f32::EPSILON).then_some(value)
}

/// One color role: quick swatches (clicking one sets this scope's color), the full
/// picker, and Reset back to the inherited color while this scope sets its own.
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
    let label = |ui: &mut egui::Ui| {
        // A dot marks the roles this scope sets itself.
        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
        if overridden {
            ui.painter().circle_filled(dot.center(), 3.0, accent);
        }
        ui.add(egui::Label::new(role.label()).truncate())
    };
    row_with(ui, label, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let ring = ui.visuals().widgets.noninteractive.fg_stroke.color;
        let edge = ui.visuals().widgets.noninteractive.bg_stroke;
        for (index, swatch) in SWATCHES.iter().enumerate() {
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
        if overridden {
            let response = ui
                .add(egui::Button::new("Reset").small())
                .on_hover_text("Use the inherited color");
            record(
                controls,
                SettingsItem::ResetColor { scope, role },
                &response,
            );
            if response.clicked() {
                actions.push(WorkspaceAction::SetColor {
                    scope,
                    role,
                    color: None,
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::format_value;

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
}
