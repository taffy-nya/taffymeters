//! The Module tab: layout actions, type, style and channel, analysis nodes, the
//! Module's own view sections, colors and closing the instance.

use eframe::egui::{self, RichText};

use super::{
    SettingsItem,
    theme_tab::clear_colors,
    widgets::{
        Number, button, card, card_with_reset, channel_label, choice, color_row, danger_button,
        differs, header, number_row, record, row, segment, warning,
    },
};
use crate::{
    dataflow::{ChannelSelection, DataSpec, FftConfig, Keep, NodeSpec},
    settings::{
        DataSettings, FFT_HOP_DIVISORS, FFT_WINDOWS, NodeEntry, NodeParam, SettingKind,
        SettingValue, ViewSetting, node_label, with_fft_window,
    },
    workspace::{
        ColorScope, ModuleInstanceId, Side, Workspace, WorkspaceAction,
        window::{Controls, channel_options},
    },
};

impl Workspace {
    pub(super) fn module_tab(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        ui.horizontal_wrapped(|ui| header(ui, entry.label, ""));
        self.layout_actions(ui, instance_id, controls, actions);
        card(ui, "Module", |ui| {
            self.type_row(ui, instance_id, controls, actions);
            if let Some(preserved) = &entry.preserved {
                let name = self.saved_type_name(&preserved.file.type_id);
                warning(
                    ui,
                    &format!(
                        "{name} unavailable: {}; saved settings kept",
                        preserved.reason
                    ),
                );
            } else {
                self.style_rows(ui, instance_id, controls, actions);
            }
        });
        if entry.preserved.is_none() {
            self.module_settings(ui, instance_id, controls, actions);
        }
    }

    /// Split (copies), detach, move and close the instance: first on the page,
    /// where they stay in reach.
    fn layout_actions(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (item, side, label) in [
                (SettingsItem::SplitRight, Side::Right, "Split right"),
                (SettingsItem::SplitDown, Side::Below, "Split down"),
            ] {
                if button(ui, self.can_split(instance_id, side), label, item, controls) {
                    actions.push(WorkspaceAction::SplitModule { instance_id, side });
                }
            }
            let leaves = self
                .window_of(instance_id)
                .map_or(0, |window| self.windows[&window].layout.leaves().len());
            if button(ui, leaves > 1, "Detach", SettingsItem::Detach, controls) {
                actions.push(WorkspaceAction::DetachModule { instance_id });
            }
            if button(ui, true, "Move to…", SettingsItem::MoveTo, controls) {
                actions.push(WorkspaceAction::BeginMove { instance_id });
            }
            if danger_button(ui, "Close", SettingsItem::CloseModule, controls) {
                actions.push(WorkspaceAction::CloseModule { instance_id });
            }
        });
        if self.move_source == Some(instance_id) {
            ui.label(
                RichText::new("Click an edge in any window · Esc or right-click cancels").weak(),
            );
        }
    }

    fn type_row(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        let types = self.registry.module_types();
        let current = types
            .iter()
            .find(|(type_id, _)| *type_id == entry.type_id)
            .map_or(entry.label, |(_, label)| label);
        row(ui, "Type", |ui| {
            let response = egui::ComboBox::from_id_salt("taffymeters.settings.module_type")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for &(type_id, label) in &types {
                        let selected = type_id == entry.type_id;
                        let response = ui.selectable_label(selected, label);
                        record(controls, SettingsItem::ModuleType(type_id), &response);
                        if response.clicked() && !selected {
                            actions.push(WorkspaceAction::ReplaceModule {
                                instance_id,
                                type_id,
                            });
                        }
                    }
                });
            record(controls, SettingsItem::TypePicker, &response.response);
        });
    }

    /// Style and the instance's channel.
    fn style_rows(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        let module = &entry.module;
        let styles = module.style_options();
        if !styles.is_empty() {
            // While a saved style this build lacks is kept, the fallback is not a
            // choice the user made: every option stays selectable.
            let active = entry
                .preserved_style
                .is_none()
                .then(|| module.active_style())
                .flatten();
            row(ui, "Style", |ui| {
                for option in styles {
                    if choice(
                        ui,
                        active == Some(option.id),
                        option.label,
                        SettingsItem::Style(option.id),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetStyle {
                            instance_id,
                            style_id: option.id,
                        });
                    }
                }
            });
            if let Some(kept) = &entry.preserved_style {
                warning(ui, &format!("Style {kept} unavailable; showing default"));
            }
        }
        // The channel is one row however the Module implements it (an analysis
        // Channel node or a display choice), matching the 1 / 2 / M shortcuts.
        if let Some(selected) = module.channel() {
            let descriptor = self.services.source.descriptor().clone();
            row(ui, "Channel", |ui| {
                let options = channel_options(&descriptor)
                    .chain(std::iter::once((0, ChannelSelection::MonoAverage)));
                for (index, option) in options {
                    if choice(
                        ui,
                        selected == option,
                        &channel_label(index, option),
                        SettingsItem::Channel(option),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetChannel {
                            instance_id,
                            channel: option,
                        });
                    }
                }
            });
        }
    }

    /// Analysis nodes, the Module's view sections and the instance's colors, each
    /// card with ↺ while something in it differs from the type's defaults.
    fn module_settings(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        let module = &entry.module;
        let channel = module.channel();
        let defaults = self.registry.default_data(entry.type_id);
        if let Some(controller) = module.data() {
            let spec = controller.spec();
            let settings = DataSettings::describe(&spec);
            let channel_nodes = settings
                .nodes
                .iter()
                .filter(|node| matches!(node.spec, NodeSpec::Channel(_)))
                .count();
            // The channel row already shows a single Channel node.
            let skip_channel = channel.is_some() && channel_nodes == 1;
            let nodes: Vec<&NodeEntry> = settings
                .nodes
                .iter()
                .filter(|node| !(skip_channel && matches!(node.spec, NodeSpec::Channel(_))))
                .collect();
            let histories = settings
                .outputs
                .iter()
                .filter(|output| matches!(output.keep, Keep::Seconds(_)))
                .count();
            // What resetting the card changes: shown nodes and History lengths that
            // differ from the type's default analysis (same shape, same positions).
            let mut resets = Vec::new();
            if let Some(defaults) = defaults {
                for node in &nodes {
                    if let Some(default) = defaults.node(node.node)
                        && default != node.spec
                    {
                        resets.push(WorkspaceAction::EditNode {
                            instance_id,
                            node: node.node,
                            spec: default,
                        });
                    }
                }
                for output in &settings.outputs {
                    if let (Keep::Seconds(_), Some(default)) =
                        (output.keep, defaults.outputs().get(output.output.index()))
                        && let Keep::Seconds(seconds) = default.keep
                        && default.keep != output.keep
                    {
                        resets.push(WorkspaceAction::SetKeep {
                            instance_id,
                            output: output.output,
                            seconds,
                        });
                    }
                }
            }
            if !nodes.is_empty() || histories > 0 {
                let (_, reset) = card_with_reset(
                    ui,
                    "Analysis",
                    SettingsItem::ResetAnalysis,
                    !resets.is_empty(),
                    controls,
                    |ui, controls| {
                        for node in nodes {
                            self.node_rows(ui, instance_id, node, defaults, controls, actions);
                        }
                        for output in &settings.outputs {
                            let label = module.output_label(output.output).unwrap_or("Output");
                            let default = defaults
                                .and_then(|defaults| defaults.outputs().get(output.output.index()))
                                .and_then(|default| match default.keep {
                                    Keep::Seconds(seconds) => Some(seconds),
                                    Keep::Latest => None,
                                });
                            if let Keep::Seconds(seconds) = output.keep
                                && let Some(seconds) = number_row(
                                    ui,
                                    Number {
                                        label: &format!("{label} length"),
                                        key: SettingsItem::Keep(output.output),
                                        current: seconds,
                                        range: (0.5, 60.0),
                                        unit: "s",
                                        logarithmic: false,
                                        default,
                                    },
                                    controls,
                                )
                            {
                                actions.push(WorkspaceAction::SetKeep {
                                    instance_id,
                                    output: output.output,
                                    seconds,
                                });
                            }
                        }
                    },
                );
                if reset {
                    actions.extend(resets);
                }
            }
        }
        // One card per section the Module declares, in its order. Channel settings
        // never show here: with the capability they are the channel row, without it
        // there is no display channel.
        let view = module.view_settings();
        let mut rest = view.as_slice();
        while let Some(first) = rest.first() {
            let length = rest
                .iter()
                .take_while(|setting| setting.section == first.section)
                .count();
            let (section, tail) = rest.split_at(length);
            rest = tail;
            if section
                .iter()
                .all(|setting| matches!(setting.kind, SettingKind::Channel))
            {
                continue;
            }
            let changed: Vec<&ViewSetting> = section
                .iter()
                .filter(|setting| view_default(setting).is_some_and(|d| view_differs(setting, &d)))
                .collect();
            let (_, reset) = card_with_reset(
                ui,
                first.section,
                SettingsItem::ResetSection(first.section),
                !changed.is_empty(),
                controls,
                |ui, controls| {
                    for setting in section {
                        view_setting_row(ui, instance_id, setting, controls, actions);
                    }
                },
            );
            if reset {
                actions.extend(changed.into_iter().filter_map(|setting| {
                    Some(WorkspaceAction::SetViewSetting {
                        instance_id,
                        key: setting.key,
                        value: view_default(setting)?,
                    })
                }));
            }
        }
        let roles = module.color_roles();
        if !roles.is_empty() {
            let scope = ColorScope::Instance(instance_id);
            let (_, reset) = card_with_reset(
                ui,
                "Colors · this module",
                SettingsItem::ResetColors(scope),
                !entry.colors.is_empty(),
                controls,
                |ui, controls| {
                    for role in roles {
                        color_row(
                            ui,
                            scope,
                            *role,
                            entry.palette.color(*role),
                            entry.colors.contains_key(role),
                            controls,
                            actions,
                        );
                    }
                },
            );
            if reset {
                clear_colors(scope, entry.colors.keys().copied(), actions);
            }
        }
    }

    fn node_rows(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        entry: &NodeEntry,
        defaults: Option<&DataSpec>,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let node = entry.node;
        let default_spec = defaults.and_then(|defaults| defaults.node(node));
        let mut edit = |spec: NodeSpec| {
            actions.push(WorkspaceAction::EditNode {
                instance_id,
                node,
                spec,
            });
        };
        for param in NodeParam::of(&entry.spec) {
            match (param, entry.spec) {
                (NodeParam::Channel, NodeSpec::Channel(current)) => {
                    let descriptor = self.services.source.descriptor().clone();
                    row(ui, param.label(), |ui| {
                        let options = channel_options(&descriptor)
                            .chain(std::iter::once((0, ChannelSelection::MonoAverage)));
                        for (index, option) in options {
                            let response =
                                segment(ui, current == option, &channel_label(index, option));
                            if response.clicked() && current != option {
                                edit(NodeSpec::Channel(option));
                            }
                        }
                    });
                }
                (NodeParam::FftWindow, NodeSpec::Fft(fft)) => {
                    row(ui, param.label(), |ui| {
                        for size in FFT_WINDOWS {
                            if choice(
                                ui,
                                fft.window_size == size,
                                &size.to_string(),
                                SettingsItem::FftWindow { node, size },
                                controls,
                            ) {
                                edit(NodeSpec::Fft(with_fft_window(fft, size)));
                            }
                        }
                    });
                }
                (NodeParam::FftHop, NodeSpec::Fft(fft)) => {
                    row(ui, param.label(), |ui| {
                        for divisor in FFT_HOP_DIVISORS {
                            let hop = (fft.window_size / divisor).max(1);
                            if choice(
                                ui,
                                fft.hop_size == hop,
                                &format!("1/{divisor}"),
                                SettingsItem::FftHop { node, divisor },
                                controls,
                            ) {
                                edit(NodeSpec::Fft(FftConfig {
                                    hop_size: hop,
                                    ..fft
                                }));
                            }
                        }
                    });
                }
                (param, spec) => {
                    let (Some((min, max, unit)), Some(current)) =
                        (param.range(), param.number(&spec))
                    else {
                        continue;
                    };
                    let label = if param.label().starts_with(node_label(&spec)) {
                        param.label().to_owned()
                    } else {
                        format!("{} {}", node_label(&spec), param.label())
                    };
                    if let Some(value) = number_row(
                        ui,
                        Number {
                            label: &label,
                            key: SettingsItem::Number {
                                node,
                                param: *param,
                            },
                            current,
                            range: (min, max),
                            unit,
                            logarithmic: false,
                            default: default_spec.and_then(|default| param.number(&default)),
                        },
                        controls,
                    ) && let Some(spec) = param.with_number(spec, value)
                    {
                        edit(spec);
                    }
                }
            }
        }
    }
}

/// What a view setting returns to: its default, numbers kept inside the current
/// range (a short History narrows the visible time). Channels reset with the
/// channel row, not here.
fn view_default(setting: &ViewSetting) -> Option<SettingValue> {
    match (setting.kind, &setting.default) {
        (SettingKind::Number { min, max, .. }, SettingValue::Number(default)) => {
            Some(SettingValue::Number(default.clamp(min, max)))
        }
        (SettingKind::Choice(_), default) | (SettingKind::Toggle, default) => Some(default.clone()),
        _ => None,
    }
}

fn view_differs(setting: &ViewSetting, default: &SettingValue) -> bool {
    match (&setting.value, default) {
        (SettingValue::Number(value), SettingValue::Number(default)) => differs(*value, *default),
        (value, default) => value != default,
    }
}

fn view_setting_row(
    ui: &mut egui::Ui,
    instance_id: ModuleInstanceId,
    setting: &ViewSetting,
    controls: &mut Controls,
    actions: &mut Vec<WorkspaceAction>,
) {
    match setting.kind {
        SettingKind::Number {
            min,
            max,
            unit,
            logarithmic,
        } => {
            let current = setting.value.number().unwrap_or(min);
            if let Some(value) = number_row(
                ui,
                Number {
                    label: setting.label,
                    key: SettingsItem::ViewNumber(setting.key),
                    current,
                    range: (min, max),
                    unit,
                    logarithmic,
                    default: setting.default.number(),
                },
                controls,
            ) {
                actions.push(WorkspaceAction::SetViewSetting {
                    instance_id,
                    key: setting.key,
                    value: SettingValue::Number(value),
                });
            }
        }
        SettingKind::Choice(choices) => {
            row(ui, setting.label, |ui| {
                for option in choices {
                    if choice(
                        ui,
                        setting.value == SettingValue::Choice(option.id.into()),
                        option.label,
                        SettingsItem::ViewChoice {
                            key: setting.key,
                            id: option.id,
                        },
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetViewSetting {
                            instance_id,
                            key: setting.key,
                            value: SettingValue::Choice(option.id.to_owned()),
                        });
                    }
                }
            });
        }
        SettingKind::Toggle => {
            let current = setting.value.flag().unwrap_or(false);
            row(ui, setting.label, |ui| {
                let mut shown = current;
                let response = ui.checkbox(&mut shown, "");
                record(controls, SettingsItem::ViewToggle(setting.key), &response);
                if response.changed() {
                    actions.push(WorkspaceAction::SetViewSetting {
                        instance_id,
                        key: setting.key,
                        value: SettingValue::Bool(shown),
                    });
                }
            });
        }
        // The channel row or nothing (see `module_settings`).
        SettingKind::Channel => {}
    }
}
