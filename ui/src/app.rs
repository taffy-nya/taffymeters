use eframe::egui;
use taffymeters_core::audio::AudioCapture;
use taffymeters_core::audio::AudioConsumer;
use taffymeters_core::config::DEFAULT_WINDOW_SIZE;
use taffymeters_core::processor::AudioProcessor;
use crate::panel::PanelLayout;
use crate::theme;
use crate::views::ViewType;

pub struct App {
    processor: AudioProcessor,
    _capture: AudioCapture,
    layout: PanelLayout,
    theme: &'static theme::Theme,
}

impl App {
    pub fn new(consumer: AudioConsumer, capture: AudioCapture) -> Self {
        let sample_rate = capture.sample_rate as f32;
        let num_channels = capture.num_channels;
        Self {
            processor: AudioProcessor::new(consumer, sample_rate, num_channels, DEFAULT_WINDOW_SIZE),
            _capture: capture,
            layout: PanelLayout::new(ViewType::OscilloscopeView),
            theme: theme::dark(),
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _: &mut eframe::Frame) {
        if ui.input(|i| {
            i.key_pressed(egui::Key::Escape)
        || (i.modifiers.command && i.key_pressed(egui::Key::W))
        }) {
            ui.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        let got_audio = self.processor.tick();

        let bg = egui::Frame::default()
            .fill(self.theme.background);

        egui::CentralPanel::default().frame(bg).show_inside(ui, |ui| {
            let view_needs = self.layout.draw(ui, self.processor.frame(), self.theme);
            if view_needs {
                ui.request_repaint_after(std::time::Duration::from_millis(16));
            }
        });

        egui::Area::new(egui::Id::new("window_resize_edges"))
            .order(egui::Order::Tooltip)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ui, |ui| {
                self.handle_window_interactions(ui);
            });

        if got_audio {
            ui.request_repaint_after(std::time::Duration::from_millis(16));
        } else {
            ui.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

impl App {
    fn handle_window_interactions(&self, ui: &mut egui::Ui) {
        let rect = ui.content_rect();
        let border = 6.0;

        let edges = [
            (egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, rect.min.y + border)), egui::CursorIcon::ResizeVertical, egui::ResizeDirection::North),
            (egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - border), rect.max), egui::CursorIcon::ResizeVertical, egui::ResizeDirection::South),
            (egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + border, rect.max.y)), egui::CursorIcon::ResizeHorizontal, egui::ResizeDirection::West),
            (egui::Rect::from_min_max(egui::pos2(rect.max.x - border, rect.min.y), rect.max), egui::CursorIcon::ResizeHorizontal, egui::ResizeDirection::East),
            (egui::Rect::from_min_max(rect.min, egui::pos2(rect.min.x + border, rect.min.y + border)), egui::CursorIcon::ResizeNwSe, egui::ResizeDirection::NorthWest),
            (egui::Rect::from_min_max(egui::pos2(rect.max.x - border, rect.min.y), egui::pos2(rect.max.x, rect.min.y + border)), egui::CursorIcon::ResizeNeSw, egui::ResizeDirection::NorthEast),
            (egui::Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - border), egui::pos2(rect.min.x + border, rect.max.y)), egui::CursorIcon::ResizeNeSw, egui::ResizeDirection::SouthWest),
            (egui::Rect::from_min_max(egui::pos2(rect.max.x - border, rect.max.y - border), rect.max), egui::CursorIcon::ResizeNwSe, egui::ResizeDirection::SouthEast),
        ];

        for (i, (er, cursor, dir)) in edges.into_iter().enumerate() {
            let r = ui.interact(er, ui.id().with(("resize", i)), egui::Sense::drag());
            if r.hovered() { ui.set_cursor_icon(cursor); }
            if r.drag_started() {
                ui.send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
            }
        }
    }
}
