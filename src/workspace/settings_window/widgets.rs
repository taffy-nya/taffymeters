//! Row widgets shared by the settings tabs.

use eframe::egui::{self, vec2};

use super::{Pending, SWATCHES, SettingsItem};
use crate::{
    dataflow::ChannelSelection,
    render::Color,
    theme::ColorRole,
    workspace::{
        ColorScope, WorkspaceAction,
        window::{ControlKey, Controls, color32},
    },
};

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

/// One slider; returns the value to commit when the drag ends (or on a keyboard
/// change).
#[allow(clippy::too_many_arguments)]
pub(super) fn number_row(
    ui: &mut egui::Ui,
    label: &str,
    key: SettingsItem,
    current: f32,
    (min, max): (f32, f32),
    unit: &str,
    logarithmic: bool,
    pending: &mut Pending,
    controls: &mut Controls,
) -> Option<f32> {
    let mut value = match pending {
        Some((control, value)) if *control == key => *value,
        _ => current,
    };
    let mut commit = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui.add(
            egui::Slider::new(&mut value, min..=max)
                .logarithmic(logarithmic)
                .show_value(false),
        );
        record(controls, key, &response);
        ui.label(format!("{value:.1} {unit}"));
        if response.changed() {
            *pending = Some((key, value));
        }
        if response.drag_stopped() || (response.changed() && !response.is_pointer_button_down_on())
        {
            *pending = None;
            if (value - current).abs() > f32::EPSILON {
                commit = Some(value);
            }
        }
    });
    commit
}

pub(super) fn choice(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    item: SettingsItem,
    controls: &mut Controls,
) -> bool {
    let response = ui.selectable_label(selected, label);
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

pub(super) fn color_row(
    ui: &mut egui::Ui,
    scope: ColorScope,
    role: ColorRole,
    current: Color,
    overridden: bool,
    controls: &mut Controls,
    actions: &mut Vec<WorkspaceAction>,
) {
    ui.horizontal(|ui| {
        let mut on = overridden;
        let response = ui.checkbox(&mut on, role.label());
        record(controls, SettingsItem::Override { scope, role }, &response);
        if response.changed() {
            actions.push(WorkspaceAction::SetColor {
                scope,
                role,
                color: on.then_some(current),
            });
        }
        if !overridden {
            let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color32(current));
            return;
        }
        for (index, swatch) in SWATCHES.iter().enumerate() {
            let response = ui.add(
                egui::Button::new("")
                    .fill(color32(*swatch))
                    .min_size(vec2(14.0, 14.0)),
            );
            record(
                controls,
                SettingsItem::Swatch {
                    scope,
                    role,
                    index: index as u8,
                },
                &response,
            );
            if response.clicked() {
                actions.push(WorkspaceAction::SetColor {
                    scope,
                    role,
                    color: Some(*swatch),
                });
            }
        }
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
}
