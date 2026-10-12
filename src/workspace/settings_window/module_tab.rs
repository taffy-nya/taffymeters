//! The Module tab: instance type, style, analysis nodes, view settings, colors, layout.

use eframe::egui;

use super::{
    Pending, SettingsItem,
    widgets::{button, channel_label, choice, color_row, number_row},
};
use crate::{
    dataflow::{ChannelSelection, FftConfig, Keep, NodeSpec},
    settings::{
        DataSettings, FFT_HOP_DIVISORS, FFT_WINDOWS, NodeEntry, NodeParam, SettingKind,
        SettingValue, node_label, with_fft_window,
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
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let entry = &self.modules[&instance_id];
        ui.heading(format!("{} #{}", entry.label, instance_id.0));
        ui.horizontal_wrapped(|ui| {
            ui.label("Type");
            for (type_id, label) in self.registry.module_types() {
                if choice(
                    ui,
                    type_id == entry.type_id,
                    label,
                    SettingsItem::ModuleType(type_id),
                    controls,
                ) {
                    actions.push(WorkspaceAction::ReplaceModule {
                        instance_id,
                        type_id,
                    });
                }
            }
        });
        if let Some(file) = &entry.preserved {
            ui.label(format!(
                "Module type {} cannot be used here; its saved configuration is kept unchanged.",
                file.type_id
            ));
        } else {
            self.module_settings(ui, instance_id, pending, controls, actions);
        }
        ui.separator();
        ui.strong("Layout");
        ui.horizontal_wrapped(|ui| {
            let installed = entry.preserved.is_none();
            for (item, side, label) in [
                (SettingsItem::SplitRight, Side::Right, "Split right"),
                (SettingsItem::SplitDown, Side::Below, "Split down"),
            ] {
                if button(
                    ui,
                    installed && self.can_split(instance_id, side),
                    label,
                    item,
                    controls,
                ) {
                    actions.push(WorkspaceAction::SplitModule { instance_id, side });
                }
            }
            if button(
                ui,
                installed && self.can_split(instance_id, Side::Right),
                "Duplicate",
                SettingsItem::Duplicate,
                controls,
            ) {
                actions.push(WorkspaceAction::DuplicateModule { instance_id });
            }
            let leaves = self
                .window_of(instance_id)
                .map_or(0, |window| self.windows[&window].layout.leaves().len());
            if button(
                ui,
                leaves > 1,
                "Detach to new window",
                SettingsItem::Detach,
                controls,
            ) {
                actions.push(WorkspaceAction::DetachModule { instance_id });
            }
            if button(ui, true, "Move to…", SettingsItem::MoveTo, controls) {
                actions.push(WorkspaceAction::BeginMove { instance_id });
            }
            if button(
                ui,
                true,
                "Close module",
                SettingsItem::CloseModule,
                controls,
            ) {
                actions.push(WorkspaceAction::CloseModule { instance_id });
            }
        });
        if self.move_source == Some(instance_id) {
            ui.label("Click an edge target in any window (Esc or right click there cancels).");
        }
    }

    fn module_settings(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        pending: &mut Pending,
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
            if let Some(kept) = &entry.preserved_style {
                ui.label(format!(
                    "Saved style {kept} is not available; showing the default"
                ));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Style");
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
                if button(
                    ui,
                    true,
                    "Clear visual state",
                    SettingsItem::ClearVisual,
                    controls,
                ) {
                    actions.push(WorkspaceAction::ClearVisualState { instance_id });
                }
            });
        }
        // The channel is one row however the Module implements it (an analysis
        // Channel node or a display choice), matching the 1 / 2 / M shortcuts.
        let channel = module.channel();
        if let Some(selected) = channel {
            let descriptor = self.services.source.descriptor().clone();
            ui.horizontal_wrapped(|ui| {
                ui.label("Channel");
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
            if !nodes.is_empty() || histories > 0 {
                ui.separator();
                ui.strong("Analysis");
            }
            for node in nodes {
                self.node_rows(ui, instance_id, node, pending, controls, actions);
            }
            for output in &settings.outputs {
                let label = module.output_label(output.output).unwrap_or("Output");
                if let Keep::Seconds(seconds) = output.keep
                    && let Some(seconds) = number_row(
                        ui,
                        &format!("{label} length"),
                        SettingsItem::Keep(output.output),
                        seconds,
                        (0.5, 60.0),
                        "s",
                        false,
                        pending,
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
        }
        let mut section = "";
        for setting in module.view_settings() {
            if matches!(setting.kind, SettingKind::Channel) && channel.is_some() {
                continue;
            }
            if setting.section != section {
                section = setting.section;
                ui.separator();
                ui.strong(section);
            }
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
                        setting.label,
                        SettingsItem::ViewNumber(setting.key),
                        current,
                        (min, max),
                        unit,
                        logarithmic,
                        pending,
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
                    ui.horizontal_wrapped(|ui| {
                        ui.label(setting.label);
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
                // Modules without the channel capability have no display channel.
                SettingKind::Channel => {}
            }
        }
        let roles = module.color_roles();
        if !roles.is_empty() {
            ui.separator();
            ui.strong("Colors (checked: this module only)");
            for role in roles {
                color_row(
                    ui,
                    ColorScope::Instance(instance_id),
                    *role,
                    entry.palette.color(*role),
                    entry.colors.contains_key(role),
                    controls,
                    actions,
                );
            }
        }
    }

    fn node_rows(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        entry: &NodeEntry,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let node = entry.node;
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
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
                        let options = channel_options(&descriptor)
                            .chain(std::iter::once((0, ChannelSelection::MonoAverage)));
                        for (index, option) in options {
                            let response = ui
                                .selectable_label(current == option, channel_label(index, option));
                            if response.clicked() && current != option {
                                edit(NodeSpec::Channel(option));
                            }
                        }
                    });
                }
                (NodeParam::FftWindow, NodeSpec::Fft(fft)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
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
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
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
                        &label,
                        SettingsItem::Number {
                            node,
                            param: *param,
                        },
                        current,
                        (min, max),
                        unit,
                        false,
                        pending,
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
