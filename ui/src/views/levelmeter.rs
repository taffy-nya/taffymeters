use std::collections::VecDeque;
use eframe::egui::{self, Color32, Rect, Stroke, pos2, vec2};
use taffymeters_core::signal::AudioData;
use super::traits::View;

/// Claude Sonnet 神力之 ITU-R BS.1770 K-weighting 双二阶滤波器
struct Biquad {
    b0: f64, b1: f64, b2: f64,
    a1: f64, a2: f64,
    x1: f64, x2: f64,
    y1: f64, y2: f64,
}

impl Biquad {
    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
              - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1; self.x1 = x;
        self.y2 = self.y1; self.y1 = y;
        y
    }

    /// 高搁置预滤波
    fn stage1(fs: f64) -> Self {
        let f0 = 1681.974450955533_f64;
        let g = 3.999843853973347_f64;
        let q = 0.7071752369554196_f64;
        let k = (std::f64::consts::PI * f0 / fs).tan();
        let vh = 10_f64.powf(g / 20.0);
        let vb = 10_f64.powf(g / 40.0);
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }

    /// RLB 高通加权
    fn stage2(fs: f64) -> Self {
        let f0 = 38.13547087602444_f64;
        let q = 0.5003270373238773_f64;
        let k = (std::f64::consts::PI * f0 / fs).tan();
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: 1.0 / a0,
            b1: -2.0 / a0,
            b2: 1.0 / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0,
        }
    }
}

const LUFS_MIN: f32 = -60.0;
const LUFS_MAX: f32 = 3.0;
const PEAK_HOLD_S: f32 = 3.0;
const PEAK_ACCEL: f32 = 30.0;

struct ChannelMeter {
    f1: Biquad,
    f2: Biquad,
    /// 400ms 滑动窗口内各采样的 K-weighted 平方值
    sq_buf: VecDeque<f32>,
    sq_sum: f64,            // 滑动窗口维护总和
    window_size: usize,     // 400ms 对应的采样数

    lufs: f32,              // 当前 Momentary LUFS
    peak_lufs: f32,         // 峰值保持
    hold_timer: f32,
    fall_speed: f32,
    last_instant: std::time::Instant,
}

impl ChannelMeter {
    fn new(sample_rate: f32) -> Self {
        let fs = sample_rate as f64;
        let window_size = (sample_rate * 0.4) as usize; // 400ms
        Self {
            f1: Biquad::stage1(fs),
            f2: Biquad::stage2(fs),
            sq_buf: VecDeque::with_capacity(window_size),
            sq_sum: 0.0,
            window_size,
            lufs: LUFS_MIN,
            peak_lufs: LUFS_MIN,
            hold_timer: 0.0,
            fall_speed: 0.0,
            last_instant: std::time::Instant::now(),
        }
    }

    fn process(&mut self, src: &[f32], new_count: usize) {
        let now = std::time::Instant::now();
        let dt  = now.duration_since(self.last_instant).as_secs_f32().min(0.5);
        self.last_instant = now;

        let start = src.len().saturating_sub(new_count);
        for &s in &src[start..] {
            let s = s as f64;
            let filtered = self.f2.process(self.f1.process(s));
            let sq = (filtered * filtered) as f32;

            self.sq_buf.push_back(sq);
            self.sq_sum += sq as f64;

            if self.sq_buf.len() > self.window_size {
                if let Some(old) = self.sq_buf.pop_front() {
                    self.sq_sum -= old as f64;
                }
            }
        }

        // Momentary LUFS
        let mean_sq = if self.sq_buf.is_empty() { 0.0 } else {
            (self.sq_sum / self.sq_buf.len() as f64).max(0.0)
        };
        self.lufs = if mean_sq < 1e-10 {
            LUFS_MIN
        } else {
            (-0.691 + 10.0 * mean_sq.log10() as f32).clamp(LUFS_MIN, LUFS_MAX + 3.0)
        };

        // 峰值保持
        if self.lufs >= self.peak_lufs {
            self.peak_lufs = self.lufs;
            self.hold_timer = PEAK_HOLD_S;
            self.fall_speed = 0.0;
        } else if self.hold_timer > 0.0 {
            self.hold_timer = (self.hold_timer - dt).max(0.0);
            self.fall_speed = 0.0;
        } else {
            self.fall_speed += PEAK_ACCEL * dt;
            self.peak_lufs = (self.peak_lufs - self.fall_speed * dt).max(self.lufs);
        }
    }

    fn reset_filters(&mut self, sample_rate: f32) {
        let fs = sample_rate as f64;
        self.f1 = Biquad::stage1(fs);
        self.f2 = Biquad::stage2(fs);
        self.window_size = (sample_rate * 0.4) as usize;
        self.sq_buf.clear();
        self.sq_sum = 0.0;
    }
}

fn lufs_to_norm(lufs: f32) -> f32 {
    ((lufs - LUFS_MIN) / (LUFS_MAX - LUFS_MIN)).clamp(0.0, 1.0)
}

fn bar_color(norm: f32) -> Color32 {
    if norm < 0.70 {
        Color32::LIGHT_BLUE
    } else if norm < 0.85 { // 黄 -> 红
        let t = (norm - 0.70) / 0.15;
        Color32::from_rgb(
            (60.0 + t * (255.0 - 60.0)) as u8,
            (200.0 + t * (200.0 - 200.0)) as u8,
            (80.0  * (1.0 - t)) as u8,
        )
    } else {                // 红
        Color32::from_rgb(220, 60, 40)
    }
}

const SCALE_DB: &[f32] = &[0.0, -6.0, -12.0, -18.0, -23.0, -30.0, -40.0, -60.0];

pub struct LevelMeterView {
    left: ChannelMeter,
    right: ChannelMeter,
    cached_rate: f32,
}

impl LevelMeterView {
    pub fn new() -> Self {
        let default_rate = 44100.0;
        Self {
            left: ChannelMeter::new(default_rate),
            right: ChannelMeter::new(default_rate),
            cached_rate: default_rate,
        }
    }
}

impl View for LevelMeterView {
    fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        // 若采样率变化则重置滤波器
        if (data.sample_rate - self.cached_rate).abs() > 0.5 {
            self.left.reset_filters(data.sample_rate);
            self.right.reset_filters(data.sample_rate);
            self.cached_rate = data.sample_rate;
        }

        let n = data.new_sample_count;
        let l_src = data.channels.first().map(|v| v.as_slice()).unwrap_or(&data.mono);
        let r_src = data.channels.get(1).map(|v| v.as_slice()).unwrap_or(&data.mono);
        if n > 0 {
            self.left.process(l_src, n);
            self.right.process(r_src, n);
        }

        let desired = ui.available_size_before_wrap();
        let (response, painter) = ui.allocate_painter(desired, egui::Sense::hover());
        let rect = response.rect;
        if rect.width() < 40.0 || rect.height() < 60.0 { return; }

        let pad = 6.0;
        let scale_w = 36.0;   // 刻度区宽度
        let gap = 4.0;    // 两条分贝表之间间距

        let bars_x = rect.min.x + pad;
        let bars_w = rect.width() - pad * 2.0 - scale_w;
        let bar_w = (bars_w - gap) / 2.0;
        let bar_top = rect.min.y + pad;
        let bar_bot = rect.max.y - pad - 14.0;
        let bar_h = (bar_bot - bar_top).max(1.0);

        let l_rect = Rect::from_min_size(pos2(bars_x, bar_top), vec2(bar_w, bar_h));
        let r_rect = Rect::from_min_size(pos2(bars_x + bar_w + gap, bar_top), vec2(bar_w, bar_h));
        let scale_x = bars_x + bars_w + pad;

        let bg = Color32::from_rgba_unmultiplied(20, 20, 20, 200);
        painter.rect_filled(l_rect, 2.0, bg);
        painter.rect_filled(r_rect, 2.0, bg);

        // db 表
        for (meter, bar_rect) in [(&self.left, l_rect), (&self.right, r_rect)] {
            let norm = lufs_to_norm(meter.lufs);
            if norm > 0.0 {
                let fill_h = bar_rect.height() * norm;
                let fill_top = bar_rect.max.y - fill_h;
                let fill = Rect::from_min_max(
                    pos2(bar_rect.min.x, fill_top),
                    pos2(bar_rect.max.x, bar_rect.max.y),
                );
                let thresholds = [(0.0, 0.70), (0.70, 0.85), (0.85, 1.0)];
                for (lo, hi) in thresholds {
                    let seg_bot = bar_rect.max.y - bar_rect.height() * lo;
                    let seg_top = bar_rect.max.y - bar_rect.height() * hi;
                    let clipped = Rect::from_min_max(
                        pos2(fill.min.x, seg_top.max(fill.min.y)),
                        pos2(fill.max.x, seg_bot.min(fill.max.y)),
                    );
                    if clipped.is_positive() {
                        let mid_norm = (lo + hi) / 2.0;
                        painter.rect_filled(clipped, 0.0, bar_color(mid_norm));
                    }
                }
            }

            // 峰值保持
            let peak_norm = lufs_to_norm(meter.peak_lufs);
            if peak_norm > 0.01 {
                let py = bar_rect.max.y - bar_rect.height() * peak_norm;
                let color = if peak_norm > 0.85 {
                    Color32::from_rgb(255, 80, 60)
                } else {
                    Color32::WHITE
                };
                painter.line_segment(
                    [pos2(bar_rect.min.x, py), pos2(bar_rect.max.x, py)],
                    Stroke::new(1.5, color),
                );
            }

            painter.rect_stroke(bar_rect, 2.0, Stroke::new(1.0, Color32::from_gray(50)), egui::StrokeKind::Inside);
        }

        // dB 刻度
        let scale_color = Color32::from_gray(140);
        let tick_font = egui::FontId::monospace(9.0);
        let tick_stroke = Stroke::new(1.0, Color32::from_gray(55));

        for &db in SCALE_DB {
            let norm = lufs_to_norm(db);
            let y = bar_bot - bar_h * norm;

            painter.line_segment(
                [pos2(l_rect.min.x, y), pos2(r_rect.max.x, y)],
                tick_stroke,
            );
            let label = if db == 0.0 { " 0".to_string() } else { format!("{}", db as i32) };
            painter.text(
                pos2(scale_x, y),
                egui::Align2::LEFT_CENTER,
                label,
                tick_font.clone(),
                scale_color,
            );
        }

        // 底部标签
        let label_y = bar_bot + 4.0;
        let label_font = egui::FontId::proportional(10.0);
        let label_color = Color32::from_gray(160);

        let fmt_lufs = |v: f32| -> String {
            if v <= LUFS_MIN + 0.5 { "-∞".to_string() }
            else { format!("{:.1}", v) }
        };

        painter.text(pos2(l_rect.center().x, label_y), egui::Align2::CENTER_TOP,
            format!("L {}", fmt_lufs(self.left.lufs)), label_font.clone(), label_color);
        painter.text(pos2(r_rect.center().x, label_y), egui::Align2::CENTER_TOP,
            format!("R {}", fmt_lufs(self.right.lufs)), label_font, label_color);
    }

    fn settings_ui(&mut self, _ui: &mut egui::Ui) {
        // 暂无可调参数
    }

    fn repaint_interval(&self) -> Option<std::time::Duration> {
        // 峰值保持线需要平滑衰减
        if self.left.peak_lufs == LUFS_MIN && self.right.peak_lufs == LUFS_MIN {
            None
        } else {
            Some(std::time::Duration::from_millis(16))
        }
    }
}