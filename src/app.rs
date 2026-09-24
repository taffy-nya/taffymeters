use std::time::Instant;

use crate::{
    LiveAudio,
    audio::{AudioDescriptor, ChannelLayout, SourceState, SourceStatus},
    builtins::{self, SPECTRUM_MODULE_ID},
    dataflow::ChannelSelection,
    extensions::{Registry, RegistryError},
    module::{Module, ModuleFrame, ModuleServices},
    render::{EguiRenderTarget, Point, Rect},
};
use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ModuleInstanceId(pub(crate) u64);

#[derive(Clone, Debug)]
pub(crate) enum AppAction {
    AddModule {
        type_id: &'static str,
    },
    SelectModule {
        instance_id: ModuleInstanceId,
    },
    CloseModule {
        instance_id: ModuleInstanceId,
    },
    SetChannel {
        instance_id: ModuleInstanceId,
        channel: ChannelSelection,
    },
    SetStyle {
        instance_id: ModuleInstanceId,
        style_id: &'static str,
    },
    ClearVisualState {
        instance_id: ModuleInstanceId,
    },
    TogglePause,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ActionOutcome {
    Created(ModuleInstanceId),
    Applied,
}

#[derive(Debug)]
pub(crate) enum AppActionError {
    StaleInstance(ModuleInstanceId),
    Unsupported(&'static str),
    InstanceIdExhausted,
    Registry(RegistryError),
    Module(crate::module::ModuleError),
    LiveAudio(crate::live::LiveAudioError),
}

impl std::fmt::Display for AppActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleInstance(instance_id) => {
                write!(
                    f,
                    "module instance {} is no longer available",
                    instance_id.0
                )
            }
            Self::Unsupported(action) => write!(f, "operation is not supported: {action}"),
            Self::InstanceIdExhausted => f.write_str("module instance IDs are exhausted"),
            Self::Registry(error) => std::fmt::Display::fmt(error, f),
            Self::Module(error) => std::fmt::Display::fmt(error, f),
            Self::LiveAudio(error) => std::fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for AppActionError {}

impl From<RegistryError> for AppActionError {
    fn from(error: RegistryError) -> Self {
        Self::Registry(error)
    }
}

impl From<crate::module::ModuleError> for AppActionError {
    fn from(error: crate::module::ModuleError) -> Self {
        Self::Module(error)
    }
}

impl From<crate::live::LiveAudioError> for AppActionError {
    fn from(error: crate::live::LiveAudioError) -> Self {
        Self::LiveAudio(error)
    }
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedModule {
    pub(crate) instance_id: ModuleInstanceId,
    pub(crate) type_id: &'static str,
    pub(crate) channel: Option<ChannelSelection>,
    pub(crate) active_style: Option<&'static str>,
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppObservation {
    pub(crate) modules: Vec<ObservedModule>,
    pub(crate) active_module: Option<ModuleInstanceId>,
    pub(crate) source_status: SourceStatus,
    pub(crate) control_error: Option<String>,
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ControlKey {
    ModuleSelectOpen,
    ModuleSelectInstance(ModuleInstanceId),
    ModuleAddOpen,
    ModuleAddItem(&'static str),
    ModuleClose,
    Channel(ChannelSelection),
    StyleOpen(ModuleInstanceId),
    StyleOption {
        instance_id: ModuleInstanceId,
        style_id: &'static str,
    },
    VisualClear(ModuleInstanceId),
    SourcePause,
}

#[cfg(any(test, feature = "automation"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ControlTarget {
    pub(crate) rect: egui::Rect,
    pub(crate) enabled: bool,
    pub(crate) hovered: bool,
    pub(crate) frame_token: u64,
    pub(crate) pass_token: u64,
}

struct ModuleEntry {
    instance_id: ModuleInstanceId,
    type_id: &'static str,
    label: &'static str,
    module: Box<dyn Module>,
}

pub struct App {
    modules: Vec<ModuleEntry>,
    live: LiveAudio,
    services: ModuleServices,
    registry: Registry,
    active_module: Option<ModuleInstanceId>,
    next_instance_id: u64,
    started_at: Instant,
    control_error: Option<String>,
    #[cfg(any(test, feature = "automation"))]
    control_targets: std::collections::HashMap<ControlKey, ControlTarget>,
    #[cfg(any(test, feature = "automation"))]
    probe_frame_token: u64,
    #[cfg(any(test, feature = "automation"))]
    probe_pass_token: u64,
}

impl App {
    pub fn new(live: LiveAudio) -> Result<Self, RegistryError> {
        let mut registry = Registry::default();
        builtins::register(&mut registry)?;
        let services = ModuleServices {
            source: live.reader(),
            dataflow: live.dataflow_handle(),
        };
        let mut app = Self {
            modules: Vec::new(),
            live,
            services,
            registry,
            active_module: None,
            next_instance_id: 1,
            started_at: Instant::now(),
            control_error: None,
            #[cfg(any(test, feature = "automation"))]
            control_targets: std::collections::HashMap::new(),
            #[cfg(any(test, feature = "automation"))]
            probe_frame_token: 0,
            #[cfg(any(test, feature = "automation"))]
            probe_pass_token: 0,
        };
        let initial_module = app.create_module_entry(SPECTRUM_MODULE_ID, ModuleInstanceId(1))?;
        app.modules.push(initial_module);
        app.active_module = Some(ModuleInstanceId(1));
        app.next_instance_id = 2;
        Ok(app)
    }

    fn create_module_entry(
        &self,
        type_id: &'static str,
        instance_id: ModuleInstanceId,
    ) -> Result<ModuleEntry, RegistryError> {
        let module = self.registry.create_module(type_id, &self.services)?;
        let label = self
            .registry
            .module_types()
            .into_iter()
            .find(|(id, _)| *id == type_id)
            .map(|(_, label)| label)
            .ok_or_else(|| RegistryError::UnknownModule(type_id.to_owned()))?;
        Ok(ModuleEntry {
            instance_id,
            type_id,
            label,
            module,
        })
    }

    fn insert_module(&mut self, type_id: &'static str) -> Result<ModuleInstanceId, AppActionError> {
        let instance_id = ModuleInstanceId(self.next_instance_id);
        let next_instance_id = self
            .next_instance_id
            .checked_add(1)
            .ok_or(AppActionError::InstanceIdExhausted)?;
        let entry = self.create_module_entry(type_id, instance_id)?;
        self.modules.push(entry);
        self.active_module = Some(instance_id);
        self.next_instance_id = next_instance_id;
        Ok(instance_id)
    }

    fn active_index(&self) -> Option<usize> {
        self.modules
            .iter()
            .position(|entry| Some(entry.instance_id) == self.active_module)
    }

    fn module_index(&self, instance_id: ModuleInstanceId) -> Result<usize, AppActionError> {
        self.modules
            .iter()
            .position(|entry| entry.instance_id == instance_id)
            .ok_or(AppActionError::StaleInstance(instance_id))
    }

    pub(crate) fn apply_action(
        &mut self,
        action: AppAction,
    ) -> Result<ActionOutcome, AppActionError> {
        let result = match action {
            AppAction::AddModule { type_id } => {
                self.insert_module(type_id).map(ActionOutcome::Created)
            }
            AppAction::SelectModule { instance_id } => self.module_index(instance_id).map(|_| {
                self.active_module = Some(instance_id);
                ActionOutcome::Applied
            }),
            AppAction::CloseModule { instance_id } => self.module_index(instance_id).map(|index| {
                self.modules.remove(index);
                if self.active_module == Some(instance_id) {
                    self.active_module = self
                        .modules
                        .get(index.min(self.modules.len().saturating_sub(1)))
                        .map(|entry| entry.instance_id);
                }
                ActionOutcome::Applied
            }),
            AppAction::SetChannel {
                instance_id,
                channel,
            } => self.module_index(instance_id).and_then(|index| {
                let module = &mut self.modules[index].module;
                let Some(current_channel) = module.channel() else {
                    return Err(AppActionError::Unsupported("channel selection"));
                };
                if current_channel != channel {
                    module.set_channel(channel)?;
                }
                Ok(ActionOutcome::Applied)
            }),
            AppAction::SetStyle {
                instance_id,
                style_id,
            } => self.module_index(instance_id).and_then(|index| {
                let options = self.modules[index].module.style_options();
                if options.is_empty() {
                    return Err(AppActionError::Unsupported("style selection"));
                }
                if !options.iter().any(|option| option.id == style_id) {
                    return Err(AppActionError::Registry(RegistryError::UnknownStyle(
                        style_id.to_owned(),
                    )));
                }
                self.modules[index]
                    .module
                    .set_style(style_id, &self.registry)?;
                Ok(ActionOutcome::Applied)
            }),
            AppAction::ClearVisualState { instance_id } => {
                self.module_index(instance_id).and_then(|index| {
                    if self.modules[index].module.style_options().is_empty() {
                        return Err(AppActionError::Unsupported("visual state clearing"));
                    }
                    self.modules[index].module.clear_visual_state();
                    Ok(ActionOutcome::Applied)
                })
            }
            AppAction::TogglePause => {
                if !matches!(
                    self.services.source.source_status().state,
                    SourceState::Running | SourceState::Paused
                ) {
                    Err(AppActionError::Unsupported(
                        "source pause or resume in its current state",
                    ))
                } else {
                    self.live
                        .toggle_pause()
                        .map(|_| ActionOutcome::Applied)
                        .map_err(AppActionError::LiveAudio)
                }
            }
        };
        match &result {
            Ok(_) => self.control_error = None,
            Err(error) => self.control_error = Some(error.to_string()),
        }
        result
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn observation(&self) -> AppObservation {
        AppObservation {
            modules: self
                .modules
                .iter()
                .map(|entry| ObservedModule {
                    instance_id: entry.instance_id,
                    type_id: entry.type_id,
                    channel: entry.module.channel(),
                    active_style: entry.module.active_style(),
                })
                .collect(),
            active_module: self.active_module,
            source_status: self.services.source.source_status(),
            control_error: self.control_error.clone(),
        }
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn control_targets(&self) -> &std::collections::HashMap<ControlKey, ControlTarget> {
        &self.control_targets
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn dataflow_handle_for_automation(&self) -> crate::dataflow::DataflowHandle {
        self.services.dataflow.clone()
    }

    #[cfg(any(test, feature = "automation"))]
    pub(crate) fn source_descriptor_for_automation(&self) -> &AudioDescriptor {
        self.services.source.descriptor().as_ref()
    }

    #[cfg(any(test, feature = "automation"))]
    fn begin_probe_pass(&mut self, ui: &egui::Ui) {
        if ui.ctx().current_pass_index() == 0 {
            self.probe_frame_token = self
                .probe_frame_token
                .checked_add(1)
                .expect("UI frame token exhausted");
        }
        self.probe_pass_token = self
            .probe_pass_token
            .checked_add(1)
            .expect("UI pass token exhausted");
        self.control_targets.clear();
    }

    fn handle_shortcuts(&mut self, ui: &egui::Ui) {
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ui.input(|input| input.key_pressed(egui::Key::Space))
            && matches!(
                self.services.source.source_status().state,
                SourceState::Running | SourceState::Paused
            )
        {
            let _ = self.apply_action(AppAction::TogglePause);
        }
        let active_channel_target = self
            .active_index()
            .filter(|&index| self.modules[index].module.channel().is_some())
            .map(|index| self.modules[index].instance_id);
        if let Some(instance_id) = active_channel_target {
            if ui.input(|input| input.key_pressed(egui::Key::M)) {
                let _ = self.apply_action(AppAction::SetChannel {
                    instance_id,
                    channel: ChannelSelection::MonoAverage,
                });
            }
            if ui.input(|input| input.key_pressed(egui::Key::Num1)) {
                let _ = self.apply_action(AppAction::SetChannel {
                    instance_id,
                    channel: self.numbered_channel(0),
                });
            }
            if ui.input(|input| input.key_pressed(egui::Key::Num2))
                && self.services.source.descriptor().channel_count > 1
            {
                let _ = self.apply_action(AppAction::SetChannel {
                    instance_id,
                    channel: self.numbered_channel(1),
                });
            }
        }
    }

    fn numbered_channel(&self, index: u16) -> ChannelSelection {
        channel_for_index(self.services.source.descriptor().channel_layout, index)
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        #[cfg(any(test, feature = "automation"))]
        let mut pending_controls = Vec::new();

        ui.horizontal_wrapped(|ui| {
            ui.label("Module:");
            let selected_text = self
                .active_index()
                .map(|index| {
                    let entry = &self.modules[index];
                    format!("{} #{}", entry.label, entry.instance_id.0)
                })
                .unwrap_or_else(|| "None".to_owned());
            let mut selected_instance = None;
            #[cfg_attr(not(any(test, feature = "automation")), allow(unused_variables))]
            let selector = egui::ComboBox::from_id_salt("module-selector")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for entry in &self.modules {
                        let instance_id = entry.instance_id;
                        let response = ui
                            .selectable_label(
                                self.active_module == Some(instance_id),
                                format!("{} #{}", entry.label, instance_id.0),
                            )
                            .on_hover_text(entry.type_id);
                        #[cfg(any(test, feature = "automation"))]
                        pending_controls.push((
                            ControlKey::ModuleSelectInstance(instance_id),
                            response.clone(),
                        ));
                        if response.clicked() {
                            selected_instance = Some(instance_id);
                        }
                    }
                });
            #[cfg(any(test, feature = "automation"))]
            pending_controls.push((ControlKey::ModuleSelectOpen, selector.response.clone()));
            if let Some(instance_id) = selected_instance {
                let _ = self.apply_action(AppAction::SelectModule { instance_id });
            }

            let mut added_type = None;
            #[cfg_attr(not(any(test, feature = "automation")), allow(unused_variables))]
            let add = egui::ComboBox::from_id_salt("module-add")
                .selected_text("Add module")
                .show_ui(ui, |ui| {
                    for (type_id, label) in self.registry.module_types() {
                        let response = ui.button(label);
                        #[cfg(any(test, feature = "automation"))]
                        pending_controls
                            .push((ControlKey::ModuleAddItem(type_id), response.clone()));
                        if response.clicked() {
                            added_type = Some(type_id);
                        }
                    }
                });
            #[cfg(any(test, feature = "automation"))]
            pending_controls.push((ControlKey::ModuleAddOpen, add.response.clone()));
            if let Some(type_id) = added_type {
                let _ = self.apply_action(AppAction::AddModule { type_id });
            }

            let close_target = self
                .active_index()
                .map(|index| self.modules[index].instance_id);
            let close_response =
                ui.add_enabled(close_target.is_some(), egui::Button::new("Close module"));
            #[cfg(any(test, feature = "automation"))]
            pending_controls.push((ControlKey::ModuleClose, close_response.clone()));
            if close_response.clicked()
                && let Some(instance_id) = close_target
            {
                let _ = self.apply_action(AppAction::CloseModule { instance_id });
            }

            let style_controls = self.active_index().map(|index| {
                (
                    self.modules[index].instance_id,
                    self.modules[index].module.style_options(),
                    self.modules[index].module.active_style(),
                )
            });
            if let Some((instance_id, style_options, active_style)) = style_controls
                && !style_options.is_empty()
            {
                ui.separator();
                ui.label("Style:");
                let active_label = style_options
                    .iter()
                    .find(|option| Some(option.id) == active_style)
                    .map_or("Select", |option| option.label);
                let mut selected_style = None;
                #[cfg_attr(not(any(test, feature = "automation")), allow(unused_variables))]
                let style = egui::ComboBox::from_id_salt("style-selector")
                    .selected_text(active_label)
                    .show_ui(ui, |ui| {
                        for option in style_options {
                            let response =
                                ui.selectable_label(active_style == Some(option.id), option.label);
                            #[cfg(any(test, feature = "automation"))]
                            pending_controls.push((
                                ControlKey::StyleOption {
                                    instance_id,
                                    style_id: option.id,
                                },
                                response.clone(),
                            ));
                            if response.clicked() {
                                selected_style = Some(option.id);
                            }
                        }
                    });
                #[cfg(any(test, feature = "automation"))]
                pending_controls.push((ControlKey::StyleOpen(instance_id), style.response.clone()));
                if let Some(style_id) = selected_style {
                    let _ = self.apply_action(AppAction::SetStyle {
                        instance_id,
                        style_id,
                    });
                }
                let clear_response = ui.button("Clear visual state");
                #[cfg(any(test, feature = "automation"))]
                pending_controls
                    .push((ControlKey::VisualClear(instance_id), clear_response.clone()));
                if clear_response.clicked() {
                    let _ = self.apply_action(AppAction::ClearVisualState { instance_id });
                }
            }
        });

        ui.horizontal_wrapped(|ui| {
            let channel_target = self.active_index().and_then(|index| {
                self.modules[index]
                    .module
                    .channel()
                    .map(|channel| (self.modules[index].instance_id, channel))
            });
            if let Some((instance_id, selected_channel)) = channel_target {
                ui.label("Channel:");
                let descriptor = self.services.source.descriptor().clone();
                for (channel_index, channel) in channel_options(&descriptor) {
                    let label = match channel {
                        ChannelSelection::Left => "1: Left".to_owned(),
                        ChannelSelection::Right => "2: Right".to_owned(),
                        ChannelSelection::Discrete(_) => {
                            format!("{}: Ch {}", channel_index + 1, channel_index + 1)
                        }
                        ChannelSelection::MonoAverage => unreachable!(),
                    };
                    let response = ui.selectable_label(selected_channel == channel, label);
                    #[cfg(any(test, feature = "automation"))]
                    pending_controls.push((ControlKey::Channel(channel), response.clone()));
                    if response.clicked() {
                        let _ = self.apply_action(AppAction::SetChannel {
                            instance_id,
                            channel,
                        });
                    }
                }
                let response = ui.selectable_label(
                    selected_channel == ChannelSelection::MonoAverage,
                    "M: Mono mix",
                );
                #[cfg(any(test, feature = "automation"))]
                pending_controls.push((
                    ControlKey::Channel(ChannelSelection::MonoAverage),
                    response.clone(),
                ));
                if response.clicked() {
                    let _ = self.apply_action(AppAction::SetChannel {
                        instance_id,
                        channel: ChannelSelection::MonoAverage,
                    });
                }
            }
            let status = self.services.source.source_status();
            let status_color = match status.state {
                SourceState::Starting | SourceState::Running => egui::Color32::LIGHT_GRAY,
                SourceState::Paused => egui::Color32::YELLOW,
                SourceState::DeviceLost | SourceState::Stopped => egui::Color32::LIGHT_RED,
            };
            ui.colored_label(status_color, source_status_text(&status));
            let button_label = match status.state {
                SourceState::Paused => "Space: Resume",
                SourceState::Starting => "Starting…",
                SourceState::Running => "Space: Pause",
                SourceState::DeviceLost => "Device lost",
                SourceState::Stopped => "Stopped",
            };
            let can_toggle = matches!(status.state, SourceState::Running | SourceState::Paused);
            let pause_response = ui.add_enabled(can_toggle, egui::Button::new(button_label));
            #[cfg(any(test, feature = "automation"))]
            pending_controls.push((ControlKey::SourcePause, pause_response.clone()));
            if pause_response.clicked() {
                let _ = self.apply_action(AppAction::TogglePause);
            }
            if let Some(error) = &self.control_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
            }
        });

        #[cfg(any(test, feature = "automation"))]
        for (key, response) in pending_controls {
            self.control_targets.insert(
                key,
                ControlTarget {
                    rect: response.rect,
                    enabled: response.enabled(),
                    hovered: response.hovered(),
                    frame_token: self.probe_frame_token,
                    pass_token: self.probe_pass_token,
                },
            );
        }
    }

    pub(crate) fn ui_body(&mut self, ui: &mut egui::Ui, display_time: std::time::Duration) {
        #[cfg(any(test, feature = "automation"))]
        self.begin_probe_pass(ui);

        self.handle_shortcuts(ui);
        let background = egui::Color32::from_rgb(13, 18, 24);
        egui::Panel::top("controls")
            .frame(egui::Frame::default().fill(background))
            .show_inside(ui, |ui| self.controls(ui));
        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(background))
            .show_inside(ui, |ui| {
                let area = ui.max_rect();
                let rect = Rect {
                    min: Point {
                        x: area.min.x,
                        y: area.min.y,
                    },
                    max: Point {
                        x: area.max.x,
                        y: area.max.y,
                    },
                };
                let pointer = ui
                    .input(|input| input.pointer.hover_pos())
                    .map(|position| Point {
                        x: position.x,
                        y: position.y,
                    });
                let scroll_delta_y = ui.input(|input| input.smooth_scroll_delta.y);
                let zoom_delta = ui.input(egui::InputState::zoom_delta);
                let mut renderer = EguiRenderTarget::new(ui.painter_at(area));
                if let Some(index) = self.active_index() {
                    let response = self.modules[index].module.draw(ModuleFrame {
                        rect,
                        monotonic_time: display_time,
                        pointer,
                        scroll_delta_y,
                        zoom_delta,
                        renderer: &mut renderer,
                    });
                    if let Some(delay) = response.repaint_after {
                        ui.request_repaint_after(delay);
                    }
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.label("No module. Use Add module to create one.");
                    });
                }
            });
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.modules.clear();
    }
}

fn channel_options(
    descriptor: &AudioDescriptor,
) -> impl Iterator<Item = (u16, ChannelSelection)> + '_ {
    (0..descriptor.channel_count)
        .map(|index| (index, channel_for_index(descriptor.channel_layout, index)))
}

fn channel_for_index(layout: ChannelLayout, index: u16) -> ChannelSelection {
    match (layout, index) {
        (ChannelLayout::Stereo, 0) => ChannelSelection::Left,
        (ChannelLayout::Stereo, 1) => ChannelSelection::Right,
        _ => ChannelSelection::Discrete(index),
    }
}

fn source_status_text(status: &SourceStatus) -> String {
    use std::fmt::Write as _;

    let state = match status.state {
        SourceState::Starting => "STARTING",
        SourceState::Running => "RUNNING",
        SourceState::Paused => "PAUSED",
        SourceState::DeviceLost => "DEVICE LOST",
        SourceState::Stopped => "STOPPED",
    };
    let mut text = format!("Source: {state}");
    if status.dropped_blocks > 0 {
        let _ = write!(text, " | dropped {}", status.dropped_blocks);
    }
    if status.invalid_samples > 0 {
        let _ = write!(text, " | invalid {}", status.invalid_samples);
    }
    if let Some(detail) = &status.detail {
        let _ = write!(text, " | {detail}");
    }
    text
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let display_time = self.started_at.elapsed();
        self.ui_body(ui, display_time);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Generation, SourceId};

    #[test]
    fn every_discrete_input_channel_has_a_selectable_option() {
        let descriptor = AudioDescriptor::new(
            SourceId(1),
            Generation(1),
            48_000,
            4,
            ChannelLayout::UnknownDiscrete,
        )
        .unwrap();
        let options: Vec<_> = channel_options(&descriptor).collect();
        assert_eq!(
            options,
            vec![
                (0, ChannelSelection::Discrete(0)),
                (1, ChannelSelection::Discrete(1)),
                (2, ChannelSelection::Discrete(2)),
                (3, ChannelSelection::Discrete(3)),
            ]
        );
    }

    #[test]
    fn shared_source_status_exposes_failures_and_telemetry() {
        let status = SourceStatus {
            state: SourceState::DeviceLost,
            dropped_blocks: 3,
            invalid_samples: 7,
            detail: Some("endpoint unavailable".to_owned()),
        };
        assert_eq!(
            source_status_text(&status),
            "Source: DEVICE LOST | dropped 3 | invalid 7 | endpoint unavailable"
        );
    }
}
