use std::collections::BTreeMap;

use eframe::egui::{self, Align, Layout, RichText};
use serde_json::Value;

use super::{
    SettingsItem, SettingsWindow,
    widgets::{
        Number, button, card, danger_button, differs, header, number_row, record, row, trailing,
        warning,
    },
};
use crate::{
    audio::{Endpoint, EndpointInfo, SourceState, list_endpoints},
    persistence::{
        CAPTURE_INPUT, DEFAULT_CAPTURE_INPUT, DEFAULT_PLAYBACK_INPUT, DEVICE_OPTION, InputConfig,
        NAME_OPTION, PLAYBACK_INPUT, PresetRef, builtin_presets, same_name, valid_name,
    },
    settings::unit,
    workspace::{Workspace, WorkspaceAction, window::Controls},
};

impl Workspace {
    pub(super) fn workspace_tab(
        &self,
        ui: &mut egui::Ui,
        settings: &mut SettingsWindow,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let detail = match self.preset() {
            Some(PresetRef::Builtin(name)) => format!("from built-in preset {name}"),
            Some(PresetRef::Saved(name)) => format!("from preset {name}"),
            None => "not saved as a preset".to_owned(),
        };
        ui.horizontal_wrapped(|ui| header(ui, "Workspace", &detail));
        let has_store = self.store.is_some();
        let saved = self.saved_presets();
        card(ui, "Presets", |ui| {
            ui.label(RichText::new("Built-in").weak());
            ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                for (index, name) in builtin_presets().enumerate() {
                    let current = self.preset() == Some(&PresetRef::Builtin(name.to_owned()));
                    let response = ui.selectable_label(current, name);
                    record(
                        controls,
                        SettingsItem::BuiltinPreset(index as u16),
                        &response,
                    );
                    if response.clicked() {
                        actions.push(WorkspaceAction::LoadPreset {
                            preset: PresetRef::Builtin(name.to_owned()),
                        });
                    }
                }
            });
            if saved.is_empty() {
                return;
            }
            ui.add_space(4.0);
            ui.label(RichText::new("Saved").weak());
            for (index, name) in saved.iter().enumerate() {
                let index = index as u16;
                if settings.confirm_delete.as_ref() == Some(name) {
                    warning(ui, &format!("Delete preset {name}?"));
                    ui.horizontal(|ui| {
                        if danger_button(ui, "Delete", SettingsItem::ConfirmDelete, controls) {
                            actions.push(WorkspaceAction::DeletePreset { name: name.clone() });
                            settings.confirm_delete = None;
                        }
                        if button(ui, true, "Cancel", SettingsItem::CancelDelete, controls) {
                            settings.confirm_delete = None;
                        }
                    });
                    continue;
                }
                ui.horizontal(|ui| {
                    const DELETE_WIDTH: f32 = 74.0;
                    let size = egui::vec2(
                        (ui.available_width() - DELETE_WIDTH).max(40.0),
                        ui.spacing().interact_size.y,
                    );
                    let current = self.preset() == Some(&PresetRef::Saved(name.clone()));
                    let response = ui
                        .allocate_ui_with_layout(
                            size,
                            Layout::top_down_justified(Align::LEFT),
                            |ui| ui.selectable_label(current, name),
                        )
                        .inner;
                    record(controls, SettingsItem::SavedPreset(index), &response);
                    if response.clicked() {
                        actions.push(WorkspaceAction::LoadPreset {
                            preset: PresetRef::Saved(name.clone()),
                        });
                    }
                    if button(
                        ui,
                        true,
                        "Delete…",
                        SettingsItem::DeletePreset(index),
                        controls,
                    ) {
                        settings.confirm_delete = Some(name.clone());
                    }
                });
            }
        });
        card(ui, "Save as preset", |ui| {
            let response = ui.add_enabled(
                has_store,
                egui::TextEdit::singleline(&mut settings.name)
                    .hint_text("Preset name")
                    .desired_width(f32::INFINITY),
            );
            record(controls, SettingsItem::PresetName, &response);
            let name = settings.name.trim().to_owned();
            let valid = valid_name(&name);
            // Preset names must not collide on Windows' case-insensitive filesystem.
            let exists = saved.iter().any(|saved| same_name(saved, &name));
            let builtin = builtin_presets().any(|builtin| same_name(builtin, &name));
            if !name.is_empty() && !valid {
                warning(ui, "Not a valid name");
            } else if exists || builtin {
                warning(ui, "Name already used");
            }
            ui.horizontal(|ui| {
                let allowed = has_store && valid && !exists && !builtin;
                if button(ui, allowed, "Save as", SettingsItem::SavePresetAs, controls) {
                    actions.push(WorkspaceAction::SavePresetAs { name: name.clone() });
                    settings.name.clear();
                }
                if let Some(PresetRef::Saved(origin)) = self.preset() {
                    let label = format!("Save to {origin}");
                    if button(ui, has_store, &label, SettingsItem::SavePreset, controls) {
                        actions.push(WorkspaceAction::SavePreset);
                    }
                }
            });
        });
        card(ui, "Audio source", |ui| {
            self.input_row(ui, settings, controls, actions);
            let status = self.source_status();
            let descriptor = self.services.source.descriptor();
            ui.label(
                RichText::new(format!(
                    "{} · {:.1} kHz · {} ch",
                    self.source_name(),
                    f64::from(descriptor.sample_rate_hz) / 1000.0,
                    descriptor.channel_count
                ))
                .weak(),
            );
            ui.label(crate::workspace::window::source_status_text(&status));
            let label = match status.state {
                SourceState::Paused => "Resume (Space)",
                _ => "Pause (Space)",
            };
            if button(
                ui,
                matches!(status.state, SourceState::Running | SourceState::Paused),
                label,
                SettingsItem::Pause,
                controls,
            ) {
                actions.push(WorkspaceAction::TogglePause);
            }
            let default = crate::module::DEFAULT_REFERENCE_DBFS;
            if let Some(dbfs) = number_row(
                ui,
                Number {
                    label: "Reference level",
                    key: SettingsItem::ReferenceLevel,
                    current: self.reference_dbfs(),
                    range: crate::module::REFERENCE_DBFS_RANGE,
                    unit: unit::DBFS,
                    logarithmic: false,
                    default: Some(default),
                },
                controls,
            ) {
                let dbfs = differs(dbfs, default).then_some(dbfs);
                actions.push(WorkspaceAction::SetReferenceLevel { dbfs });
            }
        });
        if self.notices.is_empty() {
            settings.notices_copied = false;
        } else {
            card(ui, "Notices", |ui| {
                for (index, notice) in self.notices.iter().enumerate() {
                    let response = warning(ui, notice);
                    record(controls, SettingsItem::Notice(index as u16), &response);
                }
                ui.horizontal(|ui| {
                    if button(
                        ui,
                        true,
                        "Clear notices",
                        SettingsItem::ClearNotices,
                        controls,
                    ) {
                        actions.push(WorkspaceAction::ClearNotices);
                    }
                    // Keep this last: the wider "Copied" label must not move other controls.
                    let label = if settings.notices_copied {
                        "Copied"
                    } else {
                        "Copy"
                    };
                    let response = ui.button(label);
                    record(controls, SettingsItem::CopyNotices, &response);
                    if response.clicked() {
                        ui.ctx().copy_text(self.notices.join("\n"));
                        settings.notices_copied = true;
                    } else if !response.hovered() {
                        settings.notices_copied = false;
                    }
                });
            });
        }
        ui.add_space(4.0);
        trailing(ui, |ui| {
            if danger_button(ui, "Quit", SettingsItem::Quit, controls) {
                actions.push(WorkspaceAction::Quit);
            }
        });
    }

    /// Lists devices only while the picker is open: each listing queries the system.
    fn input_row(
        &self,
        ui: &mut egui::Ui,
        settings: &mut SettingsWindow,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let chosen = self.input();
        let lost = self.source_status().state == SourceState::DeviceLost;
        row(ui, "Input", |ui| {
            let response = egui::ComboBox::from_id_salt("taffymeters.settings.input")
                .selected_text(input_label(chosen))
                .width(ui.available_width())
                .truncate()
                .show_ui(ui, |ui| {
                    let endpoints = settings.endpoints.get_or_insert_with(list_endpoints);
                    let mut index = 0_u16;
                    let mut choice = |ui: &mut egui::Ui, label: &str, input: InputConfig| {
                        let selected = same_input(&input, chosen);
                        let response = ui.selectable_label(selected, label);
                        record(controls, SettingsItem::InputChoice(index), &response);
                        index += 1;
                        // Choosing the running input again reopens a lost device.
                        if response.clicked() && (!selected || lost) {
                            actions.push(WorkspaceAction::SetInput { input });
                        }
                    };
                    let groups = [
                        (
                            "Playback",
                            DEFAULT_PLAYBACK_INPUT,
                            &endpoints.default_playback,
                            &endpoints.playback,
                        ),
                        (
                            "Microphones",
                            DEFAULT_CAPTURE_INPUT,
                            &endpoints.default_capture,
                            &endpoints.capture,
                        ),
                    ];
                    for (title, default_kind, default_name, fixed) in groups {
                        ui.label(RichText::new(title).small().weak());
                        let label = match default_name {
                            Some(name) => format!("Default ({name})"),
                            None => "Default (none)".to_owned(),
                        };
                        choice(ui, &label, input_config(default_kind, None));
                        for info in fixed {
                            choice(ui, &info.name, endpoint_input(info));
                        }
                    }
                });
            if response.inner.is_none() {
                settings.endpoints = None;
            }
            record(controls, SettingsItem::InputPicker, &response.response);
        });
    }
}

fn input_config(kind: &str, device: Option<(&str, &str)>) -> InputConfig {
    let mut options = BTreeMap::new();
    if let Some((id, name)) = device {
        options.insert(DEVICE_OPTION.to_owned(), Value::from(id));
        options.insert(NAME_OPTION.to_owned(), Value::from(name));
    }
    InputConfig {
        kind: kind.to_owned(),
        options,
    }
}

fn endpoint_input(info: &EndpointInfo) -> InputConfig {
    match &info.endpoint {
        Endpoint::Playback(id) => input_config(PLAYBACK_INPUT, Some((id, &info.name))),
        Endpoint::Capture(id) => input_config(CAPTURE_INPUT, Some((id, &info.name))),
        Endpoint::DefaultPlayback => input_config(DEFAULT_PLAYBACK_INPUT, None),
        Endpoint::DefaultCapture => input_config(DEFAULT_CAPTURE_INPUT, None),
    }
}

/// Names can change with drivers; the kind and device ID identify an input.
fn same_input(a: &InputConfig, b: &InputConfig) -> bool {
    a.kind == b.kind && a.option(DEVICE_OPTION) == b.option(DEVICE_OPTION)
}

/// A headset shows one name for its playback and microphone endpoints.
fn input_label(input: &InputConfig) -> String {
    let name = || {
        input
            .option(NAME_OPTION)
            .or_else(|| input.option(DEVICE_OPTION))
            .unwrap_or(&input.kind)
    };
    match input.kind.as_str() {
        DEFAULT_PLAYBACK_INPUT => "Default playback".to_owned(),
        DEFAULT_CAPTURE_INPUT => "Default microphone".to_owned(),
        CAPTURE_INPUT => format!("Microphone: {}", name()),
        _ => name().to_owned(),
    }
}
