//! The Workspace tab: loading, saving and deleting presets, the source status,
//! notices and quitting. The session itself saves on exit.

use eframe::egui::{self, Align, Layout, RichText};

use super::{
    SettingsItem, SettingsWindow,
    widgets::{
        Number, button, card, danger_button, differs, header, number_row, record, trailing, warning,
    },
};
use crate::{
    audio::SourceState,
    persistence::{PresetRef, builtin_presets, same_name, valid_name},
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
                    // The name fills the row like the built-in ones; Delete on the right.
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
            // Windows names files case-insensitively.
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
            let status = self.source_status();
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
            // Where the input usually peaks: every Module draws it at the same
            // height, so one setting adapts all views to the input's loudness.
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
                    // Last in the row: "Copied" is wider and moves nothing.
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
}
