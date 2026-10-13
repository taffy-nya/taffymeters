//! The Workspace tab: saving, resetting, naming and switching workspaces, the
//! source status, notices and quitting.

use eframe::egui::{self, Align, Layout};

use super::{
    SettingsItem, SettingsWindow,
    widgets::{button, card, danger_button, header, record, trailing, warning},
};
use crate::{
    audio::SourceState,
    persistence::{same_name, valid_name},
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
        ui.horizontal_wrapped(|ui| header(ui, self.name(), "workspace"));
        let has_store = self.store.is_some();
        card(ui, "Current", |ui| {
            if settings.confirm_reset {
                warning(
                    ui,
                    "Replace this workspace with one default Spectrum and the Dark theme?",
                );
                ui.horizontal(|ui| {
                    if danger_button(ui, "Reset", SettingsItem::ConfirmReset, controls) {
                        actions.push(WorkspaceAction::ResetWorkspace);
                        settings.confirm_reset = false;
                    }
                    if button(ui, true, "Cancel", SettingsItem::CancelReset, controls) {
                        settings.confirm_reset = false;
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if button(ui, self.can_save(), "Save", SettingsItem::Save, controls) {
                        actions.push(WorkspaceAction::SaveWorkspace);
                    }
                    if button(ui, true, "Reset workspace…", SettingsItem::Reset, controls) {
                        settings.confirm_reset = true;
                    }
                });
            }
        });
        card(ui, "Save as / new", |ui| {
            let response = ui.add_enabled(
                has_store,
                egui::TextEdit::singleline(&mut settings.name)
                    .hint_text("Workspace name")
                    .desired_width(f32::INFINITY),
            );
            record(controls, SettingsItem::Name, &response);
            let name = settings.name.trim().to_owned();
            let valid = valid_name(&name);
            // Windows names files case-insensitively.
            let exists = self
                .saved_workspaces()
                .iter()
                .any(|saved| same_name(saved, &name));
            // New saves the current workspace first: its own name cannot be the new one.
            let current = same_name(&name, &self.name);
            if !name.is_empty() && !valid {
                warning(ui, "Not a valid name");
            } else if exists {
                warning(ui, "A workspace with this name exists");
            } else if current {
                warning(ui, "This is the current workspace's name");
            }
            ui.horizontal(|ui| {
                let allowed = has_store && valid && !exists;
                if button(ui, allowed, "Save as", SettingsItem::SaveAs, controls) {
                    actions.push(WorkspaceAction::SaveWorkspaceAs { name: name.clone() });
                    settings.name.clear();
                }
                if button(
                    ui,
                    allowed && !current,
                    "New",
                    SettingsItem::NewWorkspace,
                    controls,
                ) {
                    actions.push(WorkspaceAction::NewWorkspace { name: name.clone() });
                    settings.name.clear();
                }
            });
        });
        let saved = self.saved_workspaces();
        if !saved.is_empty() {
            card(ui, "Saved workspaces", |ui| {
                ui.with_layout(Layout::top_down_justified(Align::LEFT), |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (index, saved_name) in saved.iter().enumerate() {
                        let current = *saved_name == self.name;
                        let response = ui.selectable_label(current, saved_name);
                        record(controls, SettingsItem::Switch(index as u16), &response);
                        if response.clicked() && !current {
                            actions.push(WorkspaceAction::SwitchWorkspace {
                                name: saved_name.clone(),
                            });
                        }
                    }
                });
            });
        }
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
        });
        if !self.notices.is_empty() {
            card(ui, "Notices", |ui| {
                for notice in &self.notices {
                    warning(ui, notice);
                }
                if button(
                    ui,
                    true,
                    "Clear notices",
                    SettingsItem::ClearNotices,
                    controls,
                ) {
                    actions.push(WorkspaceAction::ClearNotices);
                }
            });
        }
        ui.add_space(4.0);
        trailing(ui, |ui| {
            let quit = if self.can_save() {
                "Quit (save workspace)"
            } else {
                "Quit"
            };
            if danger_button(ui, quit, SettingsItem::Quit, controls) {
                actions.push(WorkspaceAction::Quit);
            }
        });
    }
}
