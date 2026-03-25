use eframe::egui;
use egui_plot::{Line, Plot};

pub fn draw(ui: &mut egui::Ui, fft_data: &[f32]) {
    let sample_rate = 44100.0;
    let fft_size = (fft_data.len() * 2) as f32; 
    let hz_per_bin = sample_rate / fft_size;

    // 我们可以提高视觉点数，比如 300 或 500，让曲线像矢量图一样圆滑
    let visual_bands = 300; 
    
    let min_freq: f32 = 20.0;
    let max_freq: f32 = 20000.0;

    let mut points = Vec::with_capacity(visual_bands);

    // 平滑插值
    let get_amplitude = |exact_bin: f32| -> f32 {
        let idx0 = exact_bin.floor() as usize;
        let idx1 = idx0 + 1;
        
        // 边界安全检查
        if idx0 >= fft_data.len() { return 0.0; }
        if idx1 >= fft_data.len() { return fft_data[idx0]; }
        
        let t = exact_bin - (idx0 as f32);
        // 线性插值公式：A*(1-t) + B*t
        fft_data[idx0] * (1.0 - t) + fft_data[idx1] * t
    };

    for i in 0..visual_bands {
        let fraction_start = i as f32 / visual_bands as f32;
        let fraction_end = (i + 1) as f32 / visual_bands as f32;
        
        let start_freq = min_freq * (max_freq / min_freq).powf(fraction_start);
        let end_freq = min_freq * (max_freq / min_freq).powf(fraction_end);

        let exact_start_bin = start_freq / hz_per_bin;
        let exact_end_bin = end_freq / hz_per_bin;

        let width = exact_end_bin - exact_start_bin;

        let mut avg_amp = if width < 1.0 {
            // 低频区：一个物理 FFT 点要拆给好几个视觉点用。
            // 直接计算当前频带的中心频率位置，进行精确的线性插值
            let center_bin = (exact_start_bin + exact_end_bin) / 2.0;
            get_amplitude(center_bin)
        } else {
            // 高频区：好几个物理 FFT 点要挤进一个视觉点里。
            // 对它们求平均值，消除杂乱的毛刺
            let start_idx = exact_start_bin.ceil() as usize;
            let end_idx = exact_end_bin.floor() as usize;
            
            if start_idx < end_idx {
                let mut sum = 0.0;
                for bin_idx in start_idx..end_idx {
                    if bin_idx < fft_data.len() {
                        sum += fft_data[bin_idx];
                    }
                }
                sum / ((end_idx - start_idx) as f32)
            } else {
                get_amplitude((exact_start_bin + exact_end_bin) / 2.0)
            }
        };

        // 听觉补偿 (Tilt 倾斜)
        // 自然界的音乐，高频能量远小于低频能量 (1/f 衰减)。
        // 为了让图表右边的高音与低音平衡，根据频率稍微放大高音
        let center_freq = (start_freq + end_freq) / 2.0;
        let tilt_compensation = (center_freq / 1000.0).powf(0.5).clamp(0.5, 3.0);
        avg_amp *= tilt_compensation;

        // 转为对数显示 (调整 200.0 这个数字可以控制整体的上下敏感度)
        let y = (avg_amp * 200.0 + 1.0).log10().max(0.0);

        points.push([i as f64, y as f64]);
    }

    let line = Line::new("spectrum_line", points)
        .color(egui::Color32::LIGHT_BLUE)
        .fill(0.0)
        .width(2.0);

        Plot::new("spectrum_plot")
        .include_y(0.0)
        .include_y(5.0)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_boxed_zoom(false)
        .show_axes([false, false])
        .show_x(false)
        .show_y(false)
        .show(ui, |plot_ui| {
            plot_ui.line(line);
        });
}