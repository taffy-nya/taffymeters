use eframe::egui;
use taffymeters_core::audio::AudioCapture;
use taffymeters_core::buffer::AudioConsumer;
use taffymeters_core::dsp::FftProcessor;
use taffymeters_core::signal::AudioData;
use crate::panel::PanelLayout;
use crate::views::ViewType;

pub struct App {
    consumer: AudioConsumer,
    _capture: AudioCapture,
    fft: FftProcessor,
    window_size: usize,
    audio_data: AudioData,
    scratch: Vec<Vec<f32>>,
    layout: PanelLayout,
    fps: usize,
}

impl App {
    pub fn new(consumer: AudioConsumer, capture: AudioCapture) -> Self {
        let sample_rate = capture.sample_rate as f32;
        let num_channels = capture.num_channels;
        let window_size = 2048;
        Self {
            consumer,
            _capture: capture,
            fft: FftProcessor::new(window_size),
            window_size,
            audio_data: AudioData::new(sample_rate, num_channels, window_size),
            scratch: vec![Vec::with_capacity(window_size * 4); num_channels],
            layout: PanelLayout::new(ViewType::Oscilloscope),
            fps: 60,
        }
    }

    fn tick_audio(&mut self) -> bool {
        for b in &mut self.scratch { b.clear(); }
        if !self.consumer.pop_into(&mut self.scratch) { return false; }

        let ws = self.window_size;
        for (ch, new_ch) in self.scratch.iter().enumerate() {
            if new_ch.is_empty() { continue; }
            let win = &mut self.audio_data.channels[ch];
            let n = new_ch.len();
            if n >= ws { win.copy_from_slice(&new_ch[n - ws..]); }
            else { win.rotate_left(n); win[ws - n..].copy_from_slice(new_ch); }
        }

        let nc = self.audio_data.num_channels as f32;
        for i in 0..ws {
            self.audio_data.mono[i] =
                self.audio_data.channels.iter().map(|ch| ch[i]).sum::<f32>() / nc;
        }
        self.audio_data.fft = self.fft.compute(&self.audio_data.mono);
        self.audio_data.new_sample_count =
            self.scratch.iter().map(|b| b.len()).max().unwrap_or(0);
        true
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

        let got_audio = self.tick_audio();

        let bg = egui::Frame::default()
            .fill(egui::Color32::from_rgba_unmultiplied(0, 0, 0, 50));

        egui::CentralPanel::default().frame(bg).show_inside(ui, |ui| {
            self.layout.draw(ui, &self.audio_data);
        });

        egui::Area::new(egui::Id::new("window_resize_edges"))
            .order(egui::Order::Tooltip)
            .fixed_pos(egui::pos2(0.0, 0.0))
            .show(ui.ctx(), |ui| {
                self.handle_window_interactions(ui);
            });

        let delay = if got_audio {
            std::time::Duration::from_millis(1000 / self.fps as u64)
        } else {
            std::time::Duration::from_millis(50)
        };
        ui.request_repaint_after(delay);
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