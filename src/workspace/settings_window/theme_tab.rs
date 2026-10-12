//! The Theme tab: preset, text size, global colors and Module type styles.

use eframe::egui;

use super::{
    Pending, SettingsItem, TYPE_ROLES,
    widgets::{choice, color_row, number_row, record},
};
use crate::{
    theme::{ColorRole, Preset, RoleOverrides, TEXT_SIZE_RANGE},
    workspace::{ColorScope, Workspace, WorkspaceAction, window::Controls},
};

impl Workspace {
    pub(super) fn theme_tab(
        &self,
        ui: &mut egui::Ui,
        selected_type: &mut Option<&'static str>,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let preset = self.theme.preset();
        ui.horizontal(|ui| {
            ui.label("Preset");
            for option in Preset::ALL {
                if choice(
                    ui,
                    preset == option,
                    option.label(),
                    SettingsItem::Preset(option),
                    controls,
                ) {
                    actions.push(WorkspaceAction::SetThemePreset { preset: option });
                }
            }
        });
        if let Some(size) = number_row(
            ui,
            "Text size",
            SettingsItem::TextSize,
            self.chrome.text_size,
            TEXT_SIZE_RANGE,
            "pt",
            false,
            pending,
            controls,
        ) {
            actions.push(WorkspaceAction::SetTextSize { size });
        }
        ui.separator();
        ui.strong("Global colors (checked: changed in this workspace)");
        for role in ColorRole::ALL {
            color_row(
                ui,
                ColorScope::Global,
                role,
                self.chrome.color(role),
                self.theme.colors.contains_key(&role),
                controls,
                actions,
            );
        }
        ui.separator();
        ui.strong("Module type style");
        ui.horizontal_wrapped(|ui| {
            for (type_id, label) in self.registry.module_types() {
                let response = ui.selectable_label(*selected_type == Some(type_id), label);
                record(controls, SettingsItem::TypeStyle(type_id), &response);
                if response.clicked() {
                    *selected_type = Some(type_id);
                }
            }
        });
        if let Some(type_id) = *selected_type {
            let style = self.theme.type_styles.get(type_id);
            let resolved = self.theme.resolve(type_id, &RoleOverrides::new());
            for role in TYPE_ROLES {
                color_row(
                    ui,
                    ColorScope::Type(type_id),
                    role,
                    resolved.color(role),
                    style.is_some_and(|style| style.contains_key(&role)),
                    controls,
                    actions,
                );
            }
        }
    }
}
