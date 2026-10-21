//! The Theme tab: preset, text size, global colors and Module type styles.

use eframe::egui;

use super::{
    SettingsItem,
    widgets::{Number, card, card_with_reset, choice, color_row, differs, number_row, row},
};
use crate::{
    theme::{ColorRole, RoleOverrides, TEXT_SIZE_RANGE, ThemePreset},
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
                for option in ThemePreset::ALL {
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
            let preset_size = preset.palette().text_size;
            if let Some(size) = number_row(
                ui,
                Number {
                    label: "Text size",
                    key: SettingsItem::TextSize,
                    current: self.chrome.text_size,
                    range: TEXT_SIZE_RANGE,
                    unit: "pt",
                    logarithmic: false,
                    default: Some(preset_size),
                },
                controls,
            ) {
                // The preset's own size follows the preset again.
                let size = differs(size, preset_size).then_some(size);
                actions.push(WorkspaceAction::SetTextSize { size });
            }
        });
        let global_scope = ColorScope::Global;
        let (_, reset) = card_with_reset(
            ui,
            "Global colors",
            SettingsItem::ResetColors(global_scope),
            !self.theme.colors.is_empty(),
            controls,
            |ui, controls| {
                for role in ColorRole::ALL {
                    color_row(
                        ui,
                        global_scope,
                        role,
                        self.chrome.color(role),
                        self.theme.colors.contains_key(&role),
                        controls,
                        actions,
                    );
                }
            },
        );
        if reset {
            clear_colors(global_scope, self.theme.colors.keys().copied(), actions);
        }
        let style = selected_type.and_then(|type_id| self.theme.type_styles.get(type_id));
        let type_scope = selected_type.map(ColorScope::Type);
        let (_, reset) = card_with_reset(
            ui,
            "Module type style",
            SettingsItem::ResetColors(type_scope.unwrap_or(global_scope)),
            style.is_some_and(|style| !style.is_empty()),
            controls,
            |ui, controls| {
                // Types that draw with no theme color (Empty) have nothing to style.
                row(ui, "Type", |ui| {
                    for (type_id, label) in self.registry.module_types() {
                        if self.registry.color_roles(type_id).is_empty() {
                            continue;
                        }
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
                if let (Some(type_id), Some(scope)) = (*selected_type, type_scope) {
                    let resolved = self.theme.resolve(type_id, &RoleOverrides::new());
                    for &role in self.registry.color_roles(type_id) {
                        color_row(
                            ui,
                            scope,
                            role,
                            resolved.color(role),
                            style.is_some_and(|style| style.contains_key(&role)),
                            controls,
                            actions,
                        );
                    }
                }
            },
        );
        if reset && let (Some(scope), Some(style)) = (type_scope, style) {
            clear_colors(scope, style.keys().copied(), actions);
        }
    }
}

/// Return every role of `roles` in `scope` to its inherited color.
pub(super) fn clear_colors(
    scope: ColorScope,
    roles: impl Iterator<Item = ColorRole>,
    actions: &mut Vec<WorkspaceAction>,
) {
    actions.extend(roles.map(|role| WorkspaceAction::SetColor {
        scope,
        role,
        color: None,
    }));
}
