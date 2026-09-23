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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ModuleInstanceId(u64);

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
        };
        app.add_module(SPECTRUM_MODULE_ID)?;
        Ok(app)
    }

    fn add_module(&mut self, type_id: &'static str) -> Result<ModuleInstanceId, RegistryError> {
        let module = self.registry.create_module(type_id, &self.services)?;
        let label = self
            .registry
            .module_types()
            .into_iter()
            .find(|(id, _)| *id == type_id)
            .expect("registered module type")
            .1;
        let id = ModuleInstanceId(self.next_instance_id);
        self.next_instance_id = self
            .next_instance_id
            .checked_add(1)
            .expect("module IDs exhausted");
        self.modules.push(ModuleEntry {
            instance_id: id,
            type_id,
            label,
            module,
        });
        self.active_module = Some(id);
        self.control_error = None;
        Ok(id)
    }

    fn active_index(&self) -> Option<usize> {
        self.modules
            .iter()
            .position(|entry| Some(entry.instance_id) == self.active_module)
    }

    fn close_active(&mut self) {
        if let Some(index) = self.active_index() {
            self.modules.remove(index);
            self.active_module = self
                .modules
                .get(index.min(self.modules.len().saturating_sub(1)))
                .map(|entry| entry.instance_id);
        }
    }

    fn select_channel(&mut self, channel: ChannelSelection) {
        if let Some(index) = self.active_index() {
            match self.modules[index].module.set_channel(channel) {
                Ok(()) => self.control_error = None,
                Err(error) => self.control_error = Some(error.to_string()),
            }
        }
    }

    fn toggle_pause(&mut self) {
        if let Err(error) = self.live.toggle_pause() {
            self.control_error = Some(error.to_string());
        } else {
            self.control_error = None;
        }
    }

    fn handle_shortcuts(&mut self, ui: &egui::Ui) {
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            ui.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ui.input(|input| input.key_pressed(egui::Key::Space))
            && matches!(
                self.services.source.source_status().state,
                SourceState::Running | SourceState::Paused
            )
        {
            self.toggle_pause();
        }
        if self
            .active_index()
            .is_some_and(|index| self.modules[index].module.channel().is_some())
        {
            if ui.input(|input| input.key_pressed(egui::Key::M)) {
                self.select_channel(ChannelSelection::MonoAverage);
            }
            if ui.input(|input| input.key_pressed(egui::Key::Num1)) {
                self.select_channel(self.numbered_channel(0));
            }
            if ui.input(|input| input.key_pressed(egui::Key::Num2))
                && self.services.source.descriptor().channel_count > 1
            {
                self.select_channel(self.numbered_channel(1));
            }
        }
    }

    fn numbered_channel(&self, index: u16) -> ChannelSelection {
        channel_for_index(self.services.source.descriptor().channel_layout, index)
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Module:");
            let selected_text = self
                .active_index()
                .map(|index| {
                    let entry = &self.modules[index];
                    format!("{} #{}", entry.label, entry.instance_id.0)
                })
                .unwrap_or_else(|| "None".to_owned());
            egui::ComboBox::from_id_salt("module-selector")
                .selected_text(selected_text)
                .show_ui(ui, |ui| {
                    for entry in &self.modules {
                        let label = format!("{} #{}", entry.label, entry.instance_id.0);
                        ui.selectable_value(
                            &mut self.active_module,
                            Some(entry.instance_id),
                            label,
                        )
                        .on_hover_text(entry.type_id);
                    }
                });
            egui::ComboBox::from_id_salt("module-add")
                .selected_text("Add module")
                .show_ui(ui, |ui| {
                    for (type_id, label) in self.registry.module_types() {
                        if ui.button(label).clicked()
                            && let Err(error) = self.add_module(type_id)
                        {
                            self.control_error = Some(error.to_string());
                        }
                    }
                });
            if ui
                .add_enabled(
                    self.active_module.is_some(),
                    egui::Button::new("Close module"),
                )
                .clicked()
            {
                self.close_active();
            }

            if let Some(index) = self.active_index() {
                let style_options = self.modules[index].module.style_options();
                if !style_options.is_empty() {
                    ui.separator();
                    ui.label("Style:");
                    let active_style = self.modules[index].module.active_style();
                    let active_label = style_options
                        .iter()
                        .find(|option| Some(option.id) == active_style)
                        .map_or("Select", |option| option.label);
                    let mut selected_style = None;
                    egui::ComboBox::from_id_salt("style-selector")
                        .selected_text(active_label)
                        .show_ui(ui, |ui| {
                            for option in style_options {
                                if ui
                                    .selectable_label(active_style == Some(option.id), option.label)
                                    .clicked()
                                {
                                    selected_style = Some(option.id);
                                }
                            }
                        });
                    if let Some(style_id) = selected_style
                        && let Err(error) = self.modules[index]
                            .module
                            .set_style(style_id, &self.registry)
                    {
                        self.control_error = Some(error.to_string());
                    }
                    if ui.button("Clear visual state").clicked() {
                        self.modules[index].module.clear_visual_state();
                    }
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            if let Some(index) = self.active_index()
                && let Some(selected_channel) = self.modules[index].module.channel()
            {
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
                    if ui
                        .selectable_label(selected_channel == channel, label)
                        .clicked()
                    {
                        self.select_channel(channel);
                    }
                }
                if ui
                    .selectable_label(
                        selected_channel == ChannelSelection::MonoAverage,
                        "M: Mono mix",
                    )
                    .clicked()
                {
                    self.select_channel(ChannelSelection::MonoAverage);
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
            if ui
                .add_enabled(can_toggle, egui::Button::new(button_label))
                .clicked()
            {
                self.toggle_pause();
            }
            if let Some(error) = &self.control_error {
                ui.colored_label(egui::Color32::LIGHT_RED, error);
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
                        monotonic_time: self.started_at.elapsed(),
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
