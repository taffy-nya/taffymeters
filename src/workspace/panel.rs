//! Floating panels inside a window: an instance's settings, the workspace theme,
//! and naming a workspace. Every change goes through `WorkspaceAction`.

use eframe::egui::{self, Id, Rect, vec2};

use super::{
    ColorScope, ModuleInstanceId, WindowId, Workspace, WorkspaceAction,
    window::{ControlKey, Controls, channel_options, color32},
};
use crate::{
    dataflow::{ChannelSelection, Keep, NodeRef, NodeSpec, OutputId, SharedAnalysisId},
    persistence::valid_name,
    render::Color,
    settings::{
        DataScope, DataSettings, FFT_HOP_DIVISORS, FFT_WINDOWS, NodeEntry, NodeParam, SettingKind,
        SettingValue, node_label, with_fft_window,
    },
    theme::{ColorRole, Preset, RoleOverrides, TEXT_SIZE_RANGE},
};

/// What a name entered in the naming panel is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamePurpose {
    SaveAs,
    New,
}

/// A slider value being dragged; committed once on release.
type Pending = Option<(PanelControl, f32)>;

#[derive(Clone, Debug)]
pub(crate) enum Panel {
    Settings {
        instance_id: ModuleInstanceId,
        pending: Pending,
    },
    Theme {
        type_id: Option<&'static str>,
        pending: Pending,
    },
    Name {
        purpose: NamePurpose,
        text: String,
        focused: bool,
    },
}

impl Panel {
    pub(crate) fn settings(instance_id: ModuleInstanceId) -> Self {
        Self::Settings {
            instance_id,
            pending: None,
        }
    }

    pub(crate) fn theme() -> Self {
        Self::Theme {
            type_id: None,
            pending: None,
        }
    }

    pub(crate) fn name(purpose: NamePurpose) -> Self {
        Self::Name {
            purpose,
            text: String::new(),
            focused: false,
        }
    }
}

/// Probe-visible controls of the panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PanelControl {
    Close,
    Style(&'static str),
    Channel {
        node: NodeRef,
        channel: ChannelSelection,
    },
    FftWindow {
        node: NodeRef,
        size: usize,
    },
    FftHop {
        node: NodeRef,
        divisor: usize,
    },
    Number {
        node: NodeRef,
        param: NodeParam,
    },
    Keep(OutputId),
    Share(OutputId),
    Unshare(OutputId),
    ViewChoice {
        key: &'static str,
        id: &'static str,
    },
    ViewChannel {
        key: &'static str,
        channel: ChannelSelection,
    },
    ViewNumber(&'static str),
    Override {
        scope: ColorScope,
        role: ColorRole,
    },
    Swatch {
        scope: ColorScope,
        role: ColorRole,
        index: u8,
    },
    Preset(Preset),
    TextSize,
    TypeStyle(&'static str),
    Name,
    Confirm,
}

/// Quick colors offered next to the full picker.
pub(crate) const SWATCHES: [Color; 8] = [
    Color::rgb(92, 230, 199),
    Color::rgb(100, 190, 255),
    Color::rgb(165, 90, 245),
    Color::rgb(255, 70, 120),
    Color::rgb(255, 170, 60),
    Color::rgb(250, 230, 90),
    Color::rgb(120, 220, 100),
    Color::rgb(240, 240, 240),
];

/// Roles a Module type style can set.
const TYPE_ROLES: [ColorRole; 9] = [
    ColorRole::Surface,
    ColorRole::Text,
    ColorRole::Muted,
    ColorRole::Guide,
    ColorRole::Primary,
    ColorRole::Secondary,
    ColorRole::Low,
    ColorRole::Mid,
    ColorRole::High,
];

fn channel_label(index: u16, channel: ChannelSelection) -> String {
    match channel {
        ChannelSelection::Left => "Left".to_owned(),
        ChannelSelection::Right => "Right".to_owned(),
        ChannelSelection::MonoAverage => "Mono".to_owned(),
        ChannelSelection::Discrete(_) => format!("Ch {}", index + 1),
    }
}

/// One slider; returns the value to commit when the drag ends (or on a keyboard
/// change).
#[allow(clippy::too_many_arguments)]
fn number_row(
    ui: &mut egui::Ui,
    label: &str,
    key: PanelControl,
    current: f32,
    (min, max): (f32, f32),
    unit: &str,
    logarithmic: bool,
    pending: &mut Pending,
    controls: &mut Controls,
) -> Option<f32> {
    let mut value = match pending {
        Some((control, value)) if *control == key => *value,
        _ => current,
    };
    let mut commit = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let response = ui.add(
            egui::Slider::new(&mut value, min..=max)
                .logarithmic(logarithmic)
                .show_value(false),
        );
        controls.record(ControlKey::Panel(key), &response);
        ui.label(format!("{value:.1} {unit}"));
        if response.changed() {
            *pending = Some((key, value));
        }
        if response.drag_stopped() || (response.changed() && !response.is_pointer_button_down_on())
        {
            *pending = None;
            if (value - current).abs() > f32::EPSILON {
                commit = Some(value);
            }
        }
    });
    commit
}

fn choice_button(
    ui: &mut egui::Ui,
    selected: bool,
    label: &str,
    key: PanelControl,
    controls: &mut Controls,
) -> bool {
    let response = ui.selectable_label(selected, label);
    controls.record(ControlKey::Panel(key), &response);
    response.clicked() && !selected
}

fn color_row(
    ui: &mut egui::Ui,
    scope: ColorScope,
    role: ColorRole,
    current: Color,
    overridden: bool,
    controls: &mut Controls,
    actions: &mut Vec<WorkspaceAction>,
) {
    ui.horizontal(|ui| {
        let mut on = overridden;
        let response = ui.checkbox(&mut on, role.label());
        controls.record(
            ControlKey::Panel(PanelControl::Override { scope, role }),
            &response,
        );
        if response.changed() {
            actions.push(WorkspaceAction::SetColor {
                scope,
                role,
                color: on.then_some(current),
            });
        }
        if !overridden {
            let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, color32(current));
            return;
        }
        for (index, swatch) in SWATCHES.iter().enumerate() {
            let response = ui.add(
                egui::Button::new("")
                    .fill(color32(*swatch))
                    .min_size(vec2(14.0, 14.0)),
            );
            controls.record(
                ControlKey::Panel(PanelControl::Swatch {
                    scope,
                    role,
                    index: index as u8,
                }),
                &response,
            );
            if response.clicked() {
                actions.push(WorkspaceAction::SetColor {
                    scope,
                    role,
                    color: Some(*swatch),
                });
            }
        }
        let mut picked = color32(current);
        if egui::color_picker::color_edit_button_srgba(
            ui,
            &mut picked,
            egui::color_picker::Alpha::OnlyBlend,
        )
        .changed()
        {
            let [red, green, blue, alpha] = picked.to_srgba_unmultiplied();
            actions.push(WorkspaceAction::SetColor {
                scope,
                role,
                color: Some(Color::rgba(red, green, blue, alpha)),
            });
        }
    });
}

impl Workspace {
    /// Instances whose analysis is a member of `binding`.
    fn binding_members(&self, binding: SharedAnalysisId) -> usize {
        self.modules
            .values()
            .filter_map(|entry| entry.module.data())
            .filter(|controller| {
                controller.with_spec(|spec| {
                    spec.outputs()
                        .iter()
                        .any(|output| output.binding == Some(binding))
                })
            })
            .count()
    }

    /// Draw the window's panel, if any.
    pub(super) fn panel_ui(
        &mut self,
        window_id: WindowId,
        ui: &mut egui::Ui,
        rect: Rect,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let Some(mut panel) = self
            .windows
            .get_mut(&window_id)
            .and_then(|window| window.ui.panel.take())
        else {
            if let Some(window) = self.windows.get_mut(&window_id) {
                window.ui.typing = false;
            }
            return;
        };
        let title = match &panel {
            Panel::Settings { instance_id, .. } => match self.modules.get(instance_id) {
                Some(entry) => format!("{} #{} settings", entry.label, instance_id.0),
                None => return,
            },
            Panel::Theme { .. } => "Theme".to_owned(),
            Panel::Name {
                purpose: NamePurpose::SaveAs,
                ..
            } => "Save workspace as".to_owned(),
            Panel::Name {
                purpose: NamePurpose::New,
                ..
            } => "New workspace".to_owned(),
        };
        let ctx = ui.ctx().clone();
        let area = rect.shrink(4.0);
        let mut keep_open = true;
        let mut typing = false;
        egui::Window::new(title)
            .id(Id::new(("taffymeters.panel", window_id.0)))
            .collapsible(false)
            .resizable(true)
            .constrain_to(area)
            .default_pos(area.min + vec2(16.0, 16.0))
            .default_size(
                (area.size() - vec2(32.0, 48.0))
                    .min(vec2(380.0, 900.0))
                    .max(vec2(160.0, 80.0)),
            )
            .vscroll(true)
            .show(&ctx, |ui| {
                let response = ui.button("Close");
                controls.record(ControlKey::Panel(PanelControl::Close), &response);
                if response.clicked() {
                    keep_open = false;
                }
                ui.separator();
                match &mut panel {
                    Panel::Settings {
                        instance_id,
                        pending,
                    } => self.settings_content(ui, *instance_id, pending, controls, actions),
                    Panel::Theme { type_id, pending } => {
                        self.theme_content(ui, type_id, pending, controls, actions);
                    }
                    Panel::Name {
                        purpose,
                        text,
                        focused,
                    } => {
                        let response = ui.text_edit_singleline(text);
                        controls.record(ControlKey::Panel(PanelControl::Name), &response);
                        if !*focused {
                            response.request_focus();
                            *focused = true;
                        }
                        typing = response.has_focus();
                        let valid = valid_name(text);
                        let exists = self.saved_workspaces().iter().any(|name| name == text);
                        if !text.is_empty() && !valid {
                            ui.label("Not a valid name");
                        } else if exists {
                            ui.label("A workspace with this name exists");
                        }
                        let entered = response.lost_focus()
                            && ui.input(|input| input.key_pressed(egui::Key::Enter));
                        let confirm = ui.add_enabled(valid && !exists, egui::Button::new("OK"));
                        controls.record(ControlKey::Panel(PanelControl::Confirm), &confirm);
                        if confirm.clicked() || (entered && valid && !exists) {
                            let name = text.clone();
                            actions.push(match purpose {
                                NamePurpose::SaveAs => WorkspaceAction::SaveWorkspaceAs { name },
                                NamePurpose::New => WorkspaceAction::NewWorkspace { name },
                            });
                            keep_open = false;
                        }
                    }
                }
            });
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.ui.typing = typing && keep_open;
            if keep_open && window.ui.panel.is_none() {
                window.ui.panel = Some(panel);
            }
        }
    }

    fn settings_content(
        &self,
        ui: &mut egui::Ui,
        instance_id: ModuleInstanceId,
        pending: &mut Pending,
        controls: &mut Controls,
        actions: &mut Vec<WorkspaceAction>,
    ) {
        let Some(entry) = self.modules.get(&instance_id) else {
            return;
        };
        if let Some(file) = &entry.preserved {
            ui.label(format!(
                "Module type {} is not available; its saved configuration is kept unchanged.",
                file.type_id
            ));
            return;
        }
        let styles = entry.module.style_options();
        if !styles.is_empty() {
            // While a saved style this build lacks is kept, the fallback is not a
            // choice the user made: every option stays selectable.
            let active = entry
                .preserved_style
                .is_none()
                .then(|| entry.module.active_style())
                .flatten();
            if let Some(kept) = &entry.preserved_style {
                ui.label(format!(
                    "Saved style {kept} is not available; showing the default"
                ));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Style");
                for option in styles {
                    if choice_button(
                        ui,
                        active == Some(option.id),
                        option.label,
                        PanelControl::Style(option.id),
                        controls,
                    ) {
                        actions.push(WorkspaceAction::SetStyle {
                            instance_id,
                            style_id: option.id,
                        });
                    }
                }
            });
        }
        if let Some(controller) = entry.module.data() {
            let spec = controller.spec();
            let settings = DataSettings::describe(&spec);
            ui.separator();
            ui.strong("Analysis");
            for binding in settings.bindings() {
                let members = self.binding_members(binding);
                ui.group(|ui| {
                    ui.label(format!(
                        "Shared group · {members} instance{} · edits apply to all",
                        if members == 1 { "" } else { "s" }
                    ));
                    for node in settings
                        .nodes
                        .iter()
                        .filter(|node| node.scope == DataScope::Shared(binding))
                    {
                        self.node_rows(ui, instance_id, node, pending, controls, actions);
                    }
                    for output in settings
                        .outputs
                        .iter()
                        .filter(|output| output.binding == Some(binding))
                    {
                        let label = entry.module.output_label(output.output).unwrap_or("Output");
                        let response = ui.button(format!("Unlink {label} (continue privately)"));
                        controls.record(
                            ControlKey::Panel(PanelControl::Unshare(output.output)),
                            &response,
                        );
                        if response.clicked() {
                            actions.push(WorkspaceAction::UnshareOutput {
                                instance_id,
                                output: output.output,
                            });
                        }
                    }
                });
            }
            let private: Vec<&NodeEntry> = settings
                .nodes
                .iter()
                .filter(|node| node.scope == DataScope::Private)
                .collect();
            if !private.is_empty() && !settings.bindings().is_empty() {
                ui.label("This instance only");
            }
            for node in private {
                self.node_rows(ui, instance_id, node, pending, controls, actions);
            }
            for output in &settings.outputs {
                let label = entry.module.output_label(output.output).unwrap_or("Output");
                if let Keep::Seconds(seconds) = output.keep
                    && let Some(seconds) = number_row(
                        ui,
                        &format!("{label} length"),
                        PanelControl::Keep(output.output),
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
                if output.shareable && output.binding.is_none() {
                    let response = ui.button(format!("Share {label} (for copies)"));
                    controls.record(
                        ControlKey::Panel(PanelControl::Share(output.output)),
                        &response,
                    );
                    if response.clicked() {
                        actions.push(WorkspaceAction::ShareOutput {
                            instance_id,
                            output: output.output,
                        });
                    }
                }
            }
        }
        let view = entry.module.view_settings();
        let mut section = "";
        for setting in &view {
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
                        PanelControl::ViewNumber(setting.key),
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
                        for choice in choices {
                            let selected = setting.value == SettingValue::Choice(choice.id.into());
                            if choice_button(
                                ui,
                                selected,
                                choice.label,
                                PanelControl::ViewChoice {
                                    key: setting.key,
                                    id: choice.id,
                                },
                                controls,
                            ) {
                                actions.push(WorkspaceAction::SetViewSetting {
                                    instance_id,
                                    key: setting.key,
                                    value: SettingValue::Choice(choice.id.to_owned()),
                                });
                            }
                        }
                    });
                }
                SettingKind::Channel => {
                    let descriptor = self.services.source.descriptor().clone();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(setting.label);
                        let options = channel_options(&descriptor)
                            .chain(std::iter::once((0, ChannelSelection::MonoAverage)));
                        for (index, channel) in options {
                            if choice_button(
                                ui,
                                setting.value.channel() == Some(channel),
                                &channel_label(index, channel),
                                PanelControl::ViewChannel {
                                    key: setting.key,
                                    channel,
                                },
                                controls,
                            ) {
                                actions.push(WorkspaceAction::SetViewSetting {
                                    instance_id,
                                    key: setting.key,
                                    value: SettingValue::Channel(channel),
                                });
                            }
                        }
                    });
                }
            }
        }
        let roles = entry.module.color_roles();
        if !roles.is_empty() {
            ui.separator();
            ui.strong("Colors (checked: this instance only)");
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
                        for (index, channel) in options {
                            if choice_button(
                                ui,
                                current == channel,
                                &channel_label(index, channel),
                                PanelControl::Channel { node, channel },
                                controls,
                            ) {
                                edit(NodeSpec::Channel(channel));
                            }
                        }
                    });
                }
                (NodeParam::FftWindow, NodeSpec::Fft(fft)) => {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(param.label());
                        for size in FFT_WINDOWS {
                            if choice_button(
                                ui,
                                fft.window_size == size,
                                &size.to_string(),
                                PanelControl::FftWindow { node, size },
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
                            if choice_button(
                                ui,
                                fft.hop_size == hop,
                                &format!("1/{divisor}"),
                                PanelControl::FftHop { node, divisor },
                                controls,
                            ) {
                                edit(NodeSpec::Fft(crate::dataflow::FftConfig {
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
                    let label = format!("{} {}", node_label(&spec), param.label());
                    let label = if node_label(&spec) == param.label() {
                        param.label().to_owned()
                    } else {
                        label
                    };
                    if let Some(value) = number_row(
                        ui,
                        &label,
                        PanelControl::Number {
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

    fn theme_content(
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
                if choice_button(
                    ui,
                    preset == option,
                    option.label(),
                    PanelControl::Preset(option),
                    controls,
                ) {
                    actions.push(WorkspaceAction::SetThemePreset { preset: option });
                }
            }
        });
        if let Some(size) = number_row(
            ui,
            "Text size",
            PanelControl::TextSize,
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
                controls.record(
                    ControlKey::Panel(PanelControl::TypeStyle(type_id)),
                    &response,
                );
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
