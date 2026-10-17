//! The Window tab: adding to an empty window, always-on-top, pin, maximize,
//! grouping and closing the targeted window.

use eframe::egui::{self, RichText};

use super::{
    SettingsItem,
    widgets::{button, card, danger_button, header, record, trailing},
};
use crate::workspace::{WindowId, Workspace, WorkspaceAction, window::Controls};

impl Workspace {
    pub(super) fn window_tab(
        &self,
        ui: &mut egui::Ui,
        window_id: WindowId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let window = &self.windows[&window_id];
        let leaves = window.layout.leaves().len();
        let detail = match leaves {
            0 => "empty".to_owned(),
            1 => "1 module".to_owned(),
            count => format!("{count} modules"),
        };
        ui.horizontal_wrapped(|ui| header(ui, &format!("Window #{}", window_id.0), &detail));
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
            let mut pinned = window.pinned;
            let response = ui
                .checkbox(&mut pinned, "Pin")
                .on_hover_text("Dragging does not move the window or its group");
            record(controls, SettingsItem::Pin, &response);
            if response.changed() {
                actions.push(WorkspaceAction::SetPinned {
                    window_id,
                    enabled: pinned,
                });
            }
            let label = if window.maximized {
                "Restore"
            } else {
                "Maximize"
            };
            if button(ui, true, label, SettingsItem::Maximize, controls) {
                actions.push(WorkspaceAction::ToggleMaximized { window_id });
            }
        });
        card(ui, "Group", |ui| {
            let status = if window.group.is_none() {
                "Not grouped. Windows that touch can move as one."
            } else if self.drag_pinned(window_id) {
                "Pinned with its group: dragging moves none of them."
            } else {
                "Moves together with the windows of its group."
            };
            ui.label(RichText::new(status).weak());
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if button(
                    ui,
                    true,
                    "Group with touching windows",
                    SettingsItem::Group,
                    controls,
                ) {
                    actions.push(WorkspaceAction::GroupTouching { window_id });
                }
                if window.group.is_some()
                    && button(ui, true, "Leave group", SettingsItem::LeaveGroup, controls)
                {
                    actions.push(WorkspaceAction::LeaveGroup { window_id });
                }
            });
        });
        ui.add_space(4.0);
        trailing(ui, |ui| {
            if danger_button(ui, "Close window", SettingsItem::CloseWindow, controls) {
                actions.push(WorkspaceAction::CloseWindow { window_id });
            }
        });
    }
}
