use eframe::egui;
use std::collections::VecDeque;

pub struct SpectrogramView {
    history: VecDeque<Vec<f32>>,
    max_history_len: usize,     // 屏幕上保留的历史帧数（流动的长度）
    visual_bands: usize,        // 纵向的频率细分度
    
    // 用于给 GPU 渲染的纹理句柄
    texture: Option<egui::TextureHandle>,
}

impl SpectrogramView {
    pub fn new() -> Self {
        Self {
            history: VecDeque::new(),
            max_history_len: 500,
            visual_bands: 300,
            texture: None,
        }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, fft_data: &[f32]) {
        let sample_rate = 44100.0;
        let fft_size = (fft_data.len() * 2) as f32;
        let hz_per_bin = sample_rate / fft_size;
        let min_freq: f32 = 20.0;
        let max_freq: f32 = 20000.0;

        let get_amplitude = |exact_bin: f32| -> f32 {
            let idx0 = exact_bin.floor() as usize;
            let idx1 = idx0 + 1;
            if idx0 >= fft_data.len() { return 0.0; }
            if idx1 >= fft_data.len() { return fft_data[idx0]; }
            let t = exact_bin - (idx0 as f32);
            fft_data[idx0] * (1.0 - t) + fft_data[idx1] * t
        };

        let mut current_column = Vec::with_capacity(self.visual_bands);

        for i in 0..self.visual_bands {
            let fraction_start = i as f32 / self.visual_bands as f32;
            let fraction_end = (i + 1) as f32 / self.visual_bands as f32;
            let start_freq = min_freq * (max_freq / min_freq).powf(fraction_start);
            let end_freq = min_freq * (max_freq / min_freq).powf(fraction_end);

            let exact_start_bin = start_freq / hz_per_bin;
            let exact_end_bin = end_freq / hz_per_bin;
            let width = exact_end_bin - exact_start_bin;

            let avg_amp = if width < 1.0 {
                get_amplitude((exact_start_bin + exact_end_bin) / 2.0)
            } else {
                let start_idx = exact_start_bin.ceil() as usize;
                let end_idx = exact_end_bin.floor() as usize;
                if start_idx < end_idx {
                    let mut sum = 0.0;
                    for bin_idx in start_idx..end_idx {
                        if bin_idx < fft_data.len() { sum += fft_data[bin_idx]; }
                    }
                    sum / ((end_idx - start_idx) as f32)
                } else {
                    get_amplitude((exact_start_bin + exact_end_bin) / 2.0)
                }
            };

            let y = (avg_amp * 200.0 + 1.0).log10().max(0.0);
            current_column.push(y);
        }

        self.history.push_front(current_column);
        if self.history.len() > self.max_history_len {
            self.history.pop_back();
        }

        // 生成图像
        let width = self.max_history_len;
        let height = self.visual_bands;
        
        let mut pixels = vec![egui::Color32::TRANSPARENT; width * height];

        for (x, column) in self.history.iter().enumerate() {
            for (y_freq, &val) in column.iter().enumerate() {
                // 低频在下
                let y_img = height - 1 - y_freq;              
                pixels[y_img * width + x] = amplitude_to_color(val);
            }
        }

        let image = egui::ColorImage {
            size: [width, height], 
            source_size: egui::vec2(width as f32, height as f32), 
            pixels 
        };

        if let Some(tex) = &mut self.texture {
            tex.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.texture = Some(ui.ctx().load_texture(
                "spectrogram_texture", 
                image, 
                egui::TextureOptions::LINEAR
            ));
        }

        let desired_size = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired_size, egui::Sense::hover());
        let rect = response.rect;

        if let Some(tex) = &self.texture {
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(tex.id(), rect, uv, egui::Color32::WHITE);
        }
    }
}

// 把 0.0 ~ 2.5 的幅度映射到颜色
fn amplitude_to_color(amp: f32) -> egui::Color32 {
    let t = (amp / 2.5).clamp(0.0, 1.0);

    // 经典的热力图颜色配方 (黑 -> 蓝 -> 紫 -> 红 -> 黄 -> 白)
    let r = (t * 3.0 - 1.0).clamp(0.0, 1.0) * 255.0;
    let g = (t * 3.0 - 2.0).clamp(0.0, 1.0) * 255.0;
    let b = (t * 3.0).clamp(0.0, 1.0) * 255.0;
    
    // 如果声音极小，返回透明
    if t < 0.05 {
        return egui::Color32::TRANSPARENT;
    }

    egui::Color32::from_rgb(r as u8, g as u8, b as u8)
}