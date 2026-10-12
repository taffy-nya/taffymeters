//! The Window tab: always-on-top, maximize, grouping and closing the targeted window.

use eframe::egui;

use super::{
    SettingsItem,
    widgets::{button, record},
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
        ui.heading(format!("Window #{}", window_id.0));
        if window.layout.is_empty() && window.pending_detach.is_none() {
            ui.horizontal_wrapped(|ui| {
                ui.label("Add module");
                for (type_id, label) in self.registry.module_types() {
                    if button(ui, true, label, SettingsItem::AddModule(type_id), controls) {
                        actions.push(WorkspaceAction::AddModule { window_id, type_id });
                    }
                }
            });
        }
        let mut on_top = window.always_on_top;
        let response = ui.checkbox(&mut on_top, "Always on top");
        record(controls, SettingsItem::AlwaysOnTop, &response);
        if response.changed() {
            actions.push(WorkspaceAction::SetAlwaysOnTop {
                window_id,
                enabled: on_top,
            });
        }
        ui.horizontal_wrapped(|ui| {
            let label = if window.maximized {
                "Restore"
            } else {
                "Maximize"
            };
            if button(ui, true, label, SettingsItem::Maximize, controls) {
                actions.push(WorkspaceAction::ToggleMaximized { window_id });
            }
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
            if button(
                ui,
                true,
                "Close window",
                SettingsItem::CloseWindow,
                controls,
            ) {
                actions.push(WorkspaceAction::CloseWindow { window_id });
            }
        });
    }
}
