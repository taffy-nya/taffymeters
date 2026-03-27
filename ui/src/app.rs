use eframe::egui;
use taffymeters_core::audio::AudioStream;
use taffymeters_core::buffer::AudioConsumer;
use taffymeters_core::dsp::DspProcessor;
use crate::views;

#[derive(PartialEq)]
pub enum ViewMode {
    Oscilloscope,
    Spectrum,
    Spectrogram,
}

pub struct App {
    audio_consumer: AudioConsumer,
    _audio_stream: AudioStream,
    dsp: DspProcessor,

    view_mode: ViewMode,
    audio_buffer: Vec<f32>,
    window_size: usize,
    new_samples: Vec<f32>,
    
    fps: usize,

    waveform_view: views::oscilloscope::OscilloscopeView,
    spectrum_view: views::spectrum::SpectrumView,
    spectrogram_view: views::spectrogram::SpectrogramView, 
}

impl App {
    pub fn new(consumer: AudioConsumer, stream: AudioStream) -> Self {
        Self {
            audio_consumer: consumer,
            _audio_stream: stream,
            dsp: DspProcessor::new(2048),

            view_mode: ViewMode::Oscilloscope,
            audio_buffer: vec![0.0; 2048],
            window_size: 2048,
            new_samples: Vec::with_capacity(2048),

            fps: 60,

            waveform_view: views::oscilloscope::OscilloscopeView::new(),
            spectrum_view: views::spectrum::SpectrumView::new(),
            spectrogram_view: views::spectrogram::SpectrogramView::new(),
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Esc 或 Ctrl + W 关闭窗口
        if ctx.input(|i| i.key_pressed(egui::Key::Escape) || (i.modifiers.command && i.key_pressed(egui::Key::W))) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        self.new_samples.clear();

        // 读取音频数据
        use ringbuf::traits::Consumer;
        self.new_samples.extend(self.audio_consumer.pop_iter());

        let new_len = self.new_samples.len();
        let mut should_repaint = false;
        if new_len > 0 {
            should_repaint = true;
            if new_len >= self.window_size {
                self.audio_buffer.copy_from_slice(&self.new_samples[new_len - self.window_size..]);
            } else {
                self.audio_buffer.rotate_left(new_len);
                self.audio_buffer[self.window_size - new_len..].copy_from_slice(&self.new_samples);
            }
        }

        // 设置背景为半透明
        let mut frame = egui::Frame::default();
        frame.fill = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 50); // 半透明黑色背景

        // 绘制主内容区域
        egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            // 拖动边缘调整大小
            let rect = ui.max_rect();
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

            let mut is_resizing = false;
            for (index, (edge_rect, cursor, resize_dir)) in edges.into_iter().enumerate() {
                let response = ui.interact(edge_rect, ui.id().with(("resize", index)), egui::Sense::drag());
                if response.hovered() {
                    ctx.set_cursor_icon(cursor);
                }
                if response.drag_started() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(resize_dir));
                    is_resizing = true;
                }
            }

            // 任意位置拖拽窗口
            let drag_rect = rect.shrink(border);
            let drag_response = ui.interact(drag_rect, ui.id().with("window_drag"), egui::Sense::drag());
            if !is_resizing && drag_response.dragged() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // 图表绘制区域
            match self.view_mode {
                ViewMode::Oscilloscope => {
                    self.waveform_view.draw(ui, &self.audio_buffer);
                }
                ViewMode::Spectrum => {
                    let fft_result = self.dsp.compute_fft(&self.audio_buffer);
                    self.spectrum_view.draw(ui, &fft_result);
                }
                ViewMode::Spectrogram => {
                    let fft_result = self.dsp.compute_fft(&self.audio_buffer);
                    self.spectrogram_view.draw(ui, &fft_result);
                }
            }

            // 悬浮工具栏 (HUD模式)
            // 当鼠标靠近窗口顶部时显示工具栏
            let pointer_pos = ctx.pointer_hover_pos();
            let show_toolbar = pointer_pos.map_or(false, |pos| pos.y <= 40.0);

            if show_toolbar {
                egui::Area::new("overlay_toolbar".into())
                    .fixed_pos(egui::pos2(border + 10.0, border + 10.0))
                    .show(ctx, |ui| {
                        egui::Frame::window(&ctx.style())
                            .fill(egui::Color32::from_rgba_unmultiplied(30, 30, 30, 200))
                            .inner_margin(6.0)
                            .corner_radius(4.0)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.selectable_value(&mut self.view_mode, ViewMode::Oscilloscope, "Oscilloscope");
                                    ui.selectable_value(&mut self.view_mode, ViewMode::Spectrum, "Spectrum");
                                    ui.selectable_value(&mut self.view_mode, ViewMode::Spectrogram, "Spectrogram");
                                });
                            });
                    });
            }
        });

        // 仅在有新音频数据产生时才请求重绘
        if should_repaint {
            ctx.request_repaint_after(std::time::Duration::from_millis(1000 / self.fps as u64));
        } else {
            // 如果处于没有声音播放的状态，以稍微缓慢的频率轮询恢复
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}