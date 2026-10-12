//! The Workspace tab: saving, switching, resetting, source status and quitting.

use eframe::egui;

use super::{
    SettingsItem, SettingsWindow,
    widgets::{button, choice, record},
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
        ui.heading(format!("Workspace: {}", self.name()));
        let has_store = self.store.is_some();
        if button(ui, self.can_save(), "Save", SettingsItem::Save, controls) {
            actions.push(WorkspaceAction::SaveWorkspace);
        }
        ui.horizontal(|ui| {
            ui.label("Name");
            let response = ui.add_enabled(
                has_store,
                egui::TextEdit::singleline(&mut settings.name).desired_width(160.0),
            );
            record(controls, SettingsItem::Name, &response);
            settings.typing = response.has_focus();
        });
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
            ui.label("Not a valid name");
        } else if exists {
            ui.label("A workspace with this name exists");
        } else if current {
            ui.label("This is the current workspace's name");
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
        let saved = self.saved_workspaces();
        if !saved.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Switch to");
                for (index, saved_name) in saved.iter().enumerate() {
                    if choice(
                        ui,
                        *saved_name == self.name,
                        saved_name,
                        SettingsItem::Switch(index as u16),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SwitchWorkspace {
                            name: saved_name.clone(),
                        });
                    }
                }
            });
        }
        if settings.confirm_reset {
            ui.label("Replace this workspace with one default Spectrum and the Dark theme?");
            ui.horizontal(|ui| {
                if button(ui, true, "Reset", SettingsItem::ConfirmReset, controls) {
                    actions.push(WorkspaceAction::ResetWorkspace);
                    settings.confirm_reset = false;
                }
                if button(ui, true, "Cancel", SettingsItem::CancelReset, controls) {
                    settings.confirm_reset = false;
                }
            });
        } else if button(ui, true, "Reset workspace…", SettingsItem::Reset, controls) {
            settings.confirm_reset = true;
        }
        ui.separator();
        ui.strong("Source");
        let status = self.source_status();
        ui.label(crate::workspace::window::source_status_text(&status));
        let label = match status.state {
            SourceState::Paused => "Resume source (Space)",
            _ => "Pause source (Space)",
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
        if !self.notices.is_empty() {
            ui.separator();
            for notice in &self.notices {
                ui.label(notice);
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
        }
        ui.separator();
        let quit = if self.can_save() {
            "Quit (save workspace)"
        } else {
            "Quit"
        };
        if button(ui, true, quit, SettingsItem::Quit, controls) {
            actions.push(WorkspaceAction::Quit);
        }
    }
}
