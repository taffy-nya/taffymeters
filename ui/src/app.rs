use eframe::egui;
use minimeters_core::audio::AudioEngine;
use minimeters_core::buffer::AudioConsumer;
use minimeters_core::dsp::DspProcessor;
use crate::views;

#[derive(PartialEq)]
pub enum ViewMode {
    Waveform,
    Spectrum,
}

pub struct App {
    audio_consumer: AudioConsumer,
    _audio_engine: AudioEngine,
    dsp: DspProcessor,

    view_mode: ViewMode,
    audio_buffer: Vec<f32>,
    window_size: usize,
}

impl App {
    pub fn new(consumer: AudioConsumer, engine: AudioEngine) -> Self {
        Self {
            audio_consumer: consumer,
            _audio_engine: engine,
            dsp: DspProcessor::new(2048),
            view_mode: ViewMode::Waveform,
            audio_buffer: vec![0.0; 2048],
            window_size: 2048,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 以滑动窗口的方式更新数据
        let mut new_samples = Vec::new();

        // 读取音频数据
        use ringbuf::traits::Consumer;
        while let Some(sample) = self.audio_consumer.try_pop() {
            new_samples.push(sample);
        }

        let new_len = new_samples.len();
        if new_len > 0 {
            if new_len >= self.window_size {
                self.audio_buffer.copy_from_slice(&new_samples[new_len - self.window_size..]);
            } else {
                self.audio_buffer.rotate_left(new_len);
                self.audio_buffer[self.window_size - new_len..].copy_from_slice(&new_samples);
            }
        }

        // 绘制顶部工具栏
        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.view_mode, ViewMode::Waveform, "Waveform");
                ui.selectable_value(&mut self.view_mode, ViewMode::Spectrum, "Spectrum");
            });
        });

        // 绘制主内容区域
        egui::CentralPanel::default().show(ctx, |ui| {
            match self.view_mode {
                ViewMode::Waveform => {
                    views::waveform::draw(ui, &self.audio_buffer);
                }
                ViewMode::Spectrum => {
                    let fft_result = self.dsp.process(&self.audio_buffer);
                    views::spectrum::draw(ui, &fft_result);
                }
            }
        });

        ctx.request_repaint();
    }
}