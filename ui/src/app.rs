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
    
    new_samples: Vec<f32>,
    fps: usize,
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
            new_samples: Vec::with_capacity(2048),
            fps: 60,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.new_samples.clear();

        // 读取音频数据
        use ringbuf::traits::Consumer;
        while let Some(sample) = self.audio_consumer.try_pop() {
            self.new_samples.push(sample);
        }

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

        // 仅在有新音频数据产生时才请求重绘
        if should_repaint {
            ctx.request_repaint_after(std::time::Duration::from_millis(1000 / self.fps as u64));
        } else {
            // 如果处于没有声音播放的状态，以稍微缓慢的频率轮询恢复
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}