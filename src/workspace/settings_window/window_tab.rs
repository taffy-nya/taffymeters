use eframe::egui::{self, RichText};

use super::{
    SettingsItem,
    widgets::{button, card, danger_button, header, record},
};
use crate::workspace::{WindowId, Workspace, WorkspaceAction, window::Controls};

impl Workspace {
    pub(super) fn window_tab(
        &self,
        ui: &mut egui::Ui,
        window_id: WindowId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) -> bool {
        let window = &self.windows[&window_id];
        let leaves = window.layout.leaves().len();
        let detail = match leaves {
            0 => "empty".to_owned(),
            1 => "1 module".to_owned(),
            count => format!("{count} modules"),
        };
        ui.horizontal_wrapped(|ui| header(ui, "Window", &detail));
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if button(ui, true, "New", SettingsItem::NewWindow, controls) {
                actions.push(WorkspaceAction::NewWindow { window_id });
            }
            if danger_button(ui, "Close", SettingsItem::CloseWindow, controls) {
                actions.push(WorkspaceAction::CloseWindow { window_id });
            }
        });
        if window.layout.is_empty() && window.pending_detach.is_none() {
            card(ui, "Add module", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (type_id, label) in self.registry.module_types() {
                        if button(ui, true, label, SettingsItem::AddModule(type_id), controls) {
                            actions.push(WorkspaceAction::AddModule { window_id, type_id });
                        }
                    }
                });
            });
        }
        card(ui, "Window", |ui| {
            let mut on_top = window.always_on_top;
            let response = ui.checkbox(&mut on_top, "Always on top");
            record(controls, SettingsItem::AlwaysOnTop, &response);
            if response.changed() {
                actions.push(WorkspaceAction::SetAlwaysOnTop {
                    window_id,
                    enabled: on_top,
                });
            }
            let mut locked = window.locked;
            let response = ui
                .checkbox(&mut locked, "Lock")
                .on_hover_text("Right click only: no moving, resizing or splitting");
            record(controls, SettingsItem::Lock, &response);
            if response.changed() {
                actions.push(WorkspaceAction::SetLocked {
                    window_id,
                    enabled: locked,
                });
            }
            let label = if window.maximized {
                "Restore"
            } else {
                "Maximize"
            };
            if button(ui, !window.locked, label, SettingsItem::Maximize, controls) {
                actions.push(WorkspaceAction::ToggleMaximized { window_id });
            }
        });
        let mut group_hovered = false;
        card(ui, "Group", |ui| {
            let status = if window.group.is_none() {
                "Not grouped"
            } else if self.drag_locked(window_id) {
                "Grouped · locked"
            } else {
                "Grouped"
            };
            ui.label(RichText::new(status).weak());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let unavailable = match self.touching_windows(window_id) {
                    Ok(touching) if touching.is_empty() => Some("No window touches this one"),
                    Ok(_) => None,
                    Err(_) => Some("Window position not known yet"),
                };
                let response = ui
                    .add_enabled(
                        unavailable.is_none(),
                        egui::Button::new("Group with touching windows"),
                    )
                    .on_disabled_hover_text(unavailable.unwrap_or_default());
                record(controls, SettingsItem::Group, &response);
                group_hovered = response.hovered();
                if response.clicked() {
                    actions.push(WorkspaceAction::GroupTouching { window_id });
                }
                if window.group.is_some()
                    && button(ui, true, "Leave group", SettingsItem::LeaveGroup, controls)
                {
                    actions.push(WorkspaceAction::LeaveGroup { window_id });
                }
            });
        });
        group_hovered
    }
}
