use std::time::Instant;

use crate::{
    LiveSpectrum,
    audio::{AudioDescriptor, ChannelLayout, SourceState},
    builtins::{self, SPECTRUM_MODULE_ID},
    dataflow::ChannelSelection,
    module::{Module, ModuleFrame, ModuleServices, Registry, RegistryError},
    render::{EguiRenderTarget, Point, Rect},
};
use eframe::egui;
pub struct App {
    live: LiveSpectrum,
    module: Box<dyn Module>,
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
        let module =
            registry.create_module(SPECTRUM_MODULE_ID, &ModuleServices { spectrum: reader })?;
        Ok(Self {
            live,
            module,
            selected_channel,
            started_at: Instant::now(),
            control_error: None,
        })
    }

    fn select_channel(&mut self, channel: ChannelSelection) {
        match self.live.set_channel(channel) {
            Ok(()) => {
                self.selected_channel = channel;
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
        if ui.input(|input| input.key_pressed(egui::Key::Space)) {
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
            let descriptor = self.live.reader().descriptor().clone();
            for (index, channel) in channel_options(&descriptor) {
                let label = match channel {
                    ChannelSelection::Left => "1: Left".to_owned(),
                    ChannelSelection::Right => "2: Right".to_owned(),
                    ChannelSelection::Discrete(_) => format!("{}: Ch {index}", index + 1),
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
            let paused = self.live.reader().source_status().state == SourceState::Paused;
            if ui
                .button(if paused {
                    "Space: Resume"
                } else {
                    "Space: Pause"
                })
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
                let mut renderer = EguiRenderTarget::new(ui.painter_at(area));
                let response = self.module.draw(ModuleFrame {
                    rect,
                    monotonic_time: self.started_at.elapsed(),
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
}
