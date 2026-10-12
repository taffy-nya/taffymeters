//! The Theme tab: preset, text size, global colors and Module type styles.

use eframe::egui;

use super::{
    SettingsItem, TYPE_ROLES,
    widgets::{card, choice, color_row, number_row, row},
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
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        card(ui, "Appearance", |ui| {
            let preset = self.theme.preset();
            row(ui, "Preset", |ui| {
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
                controls,
            ) {
                actions.push(WorkspaceAction::SetTextSize { size });
            }
        });
        card(ui, "Global colors · dot: changed in this workspace", |ui| {
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
        });
        card(ui, "Module type style", |ui| {
            row(ui, "Type", |ui| {
                for (type_id, label) in self.registry.module_types() {
                    let selected = *selected_type == Some(type_id);
                    if choice(
                        ui,
                        selected,
                        label,
                        SettingsItem::TypeStyle(type_id),
                        controls,
                    ) {
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
        });
    }
}
