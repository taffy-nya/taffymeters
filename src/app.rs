use std::time::Instant;

use crate::{
    LiveSpectrum,
    audio::{AudioDescriptor, ChannelLayout, SourceState, SourceStatus},
    builtins::{self, SPECTRUM_MODULE_ID},
    dataflow::ChannelSelection,
    module::{Module, ModuleFrame, ModuleServices, Registry, RegistryError},
    render::{EguiRenderTarget, Point, Rect},
};
use eframe::egui;

struct ModuleEntry {
    id: &'static str,
    label: &'static str,
    module: Box<dyn Module>,
}

pub struct App {
    live: LiveSpectrum,
    registry: Registry,
    modules: Vec<ModuleEntry>,
    active_module: usize,
    selected_channel: ChannelSelection,
    started_at: Instant,
    control_error: Option<String>,
}

impl App {
    pub fn new(live: LiveSpectrum) -> Result<Self, RegistryError> {
        let reader = live.reader();
        let selected_channel = reader.requested_channel();
        let mut registry = Registry::default();
        builtins::register(&mut registry)?;
        let services = ModuleServices {
            spectrum: reader,
            time_domain: live.time_domain_reader(),
        };
        let mut modules = Vec::new();
        for (id, label) in registry.module_types() {
            modules.push(ModuleEntry {
                id,
                label,
                module: registry.create_module(id, &services)?,
            });
        }
        let active_module = modules
            .iter()
            .position(|entry| entry.id == SPECTRUM_MODULE_ID)
            .unwrap_or_default();
        Ok(Self {
            live,
            registry,
            modules,
            active_module,
            selected_channel,
            started_at: Instant::now(),
            control_error: None,
        })
    }

    fn select_channel(&mut self, channel: ChannelSelection) {
        match self.live.set_channel(channel) {
            Ok(()) => {
                self.selected_channel = channel;
                for entry in &mut self.modules {
                    entry.module.set_channel(channel);
                }
                self.control_error = None;
            }
            Err(error) => self.control_error = Some(error.to_string()),
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
                self.live.reader().source_status().state,
                SourceState::Running | SourceState::Paused
            )
        {
            self.toggle_pause();
        }
        if ui.input(|input| input.key_pressed(egui::Key::M)) {
            self.select_channel(ChannelSelection::MonoAverage);
        }
        if ui.input(|input| input.key_pressed(egui::Key::Num1)) {
            self.select_channel(self.numbered_channel(0));
        }
        if ui.input(|input| input.key_pressed(egui::Key::Num2))
            && self.live.reader().descriptor().channel_count > 1
        {
            self.select_channel(self.numbered_channel(1));
        }
    }

    fn numbered_channel(&self, index: u16) -> ChannelSelection {
        channel_for_index(self.live.reader().descriptor().channel_layout, index)
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("Module:");
            let mut selected_module = self.active_module;
            egui::ComboBox::from_id_salt("module-selector")
                .selected_text(self.modules[self.active_module].label)
                .show_ui(ui, |ui| {
                    for (index, entry) in self.modules.iter().enumerate() {
                        ui.selectable_value(&mut selected_module, index, entry.label);
                    }
                });
            self.active_module = selected_module;

            let style_options = self.modules[self.active_module].module.style_options();
            if !style_options.is_empty() {
                ui.separator();
                ui.label("Style:");
                let active_style = self.modules[self.active_module].module.active_style();
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
                    && let Err(error) = self.modules[self.active_module]
                        .module
                        .set_style(style_id, &self.registry)
                {
                    self.control_error = Some(error.to_string());
                }
                if ui.button("Clear visual state").clicked() {
                    self.modules[self.active_module].module.clear_visual_state();
                }
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Channel:");
            let descriptor = self.live.reader().descriptor().clone();
            for (index, channel) in channel_options(&descriptor) {
                let label = match channel {
                    ChannelSelection::Left => "1: Left".to_owned(),
                    ChannelSelection::Right => "2: Right".to_owned(),
                    ChannelSelection::Discrete(_) => format!("{}: Ch {}", index + 1, index + 1),
                    ChannelSelection::MonoAverage => unreachable!(),
                };
                if ui
                    .selectable_label(self.selected_channel == channel, label)
                    .clicked()
                {
                    self.select_channel(channel);
                }
            }
            if ui
                .selectable_label(
                    self.selected_channel == ChannelSelection::MonoAverage,
                    "M: Mono mix",
                )
                .clicked()
            {
                self.select_channel(ChannelSelection::MonoAverage);
            }
            let status = self.live.reader().source_status();
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
                let response = self.modules[self.active_module].module.draw(ModuleFrame {
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
