use std::collections::VecDeque;
use eframe::egui::{self, Rect, Stroke, pos2, vec2};
use taffymeters_core::{
    config::DEFAULT_SAMPLE_RATE,
    dsp::kweighting::Biquad,
    frame::AudioFrame,
};
use crate::theme::Theme;
use super::traits::View;

const LUFS_MIN: f32 = -60.0;
const LUFS_MAX: f32 = 3.0;
const PEAK_HOLD_S: f32 = 3.0;
const PEAK_ACCEL: f32 = 30.0;

struct ChannelMeter {
    f1: Biquad,
    f2: Biquad,
    sq_buf: VecDeque<f32>,
    sq_sum: f64,
    window_size: usize,

    lufs: f32,
    peak_lufs: f32,
    hold_timer: f32,
    fall_speed: f32,
    last_instant: std::time::Instant,
}

impl ChannelMeter {
    fn new(sample_rate: f32) -> Self {
        let fs = sample_rate as f64;
        let window_size = (sample_rate * 0.4) as usize;
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

            if self.sq_buf.len() > self.window_size
                && let Some(old) = self.sq_buf.pop_front()
            {
                self.sq_sum -= old as f64;
            }
        }

        let mean_sq = if self.sq_buf.is_empty() { 0.0 } else {
            (self.sq_sum / self.sq_buf.len() as f64).max(0.0)
        };
        self.lufs = if mean_sq < 1e-10 {
            LUFS_MIN
        } else {
            (-0.691 + 10.0 * mean_sq.log10() as f32).clamp(LUFS_MIN, LUFS_MAX + 3.0)
        };

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

    fn is_animating(&self) -> bool {
        self.peak_lufs > LUFS_MIN
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

const SCALE_DB: &[f32] = &[0.0, -6.0, -12.0, -18.0, -23.0, -30.0, -40.0, -60.0];

pub struct LevelMeterView {
    left: ChannelMeter,
    right: ChannelMeter,
    cached_rate: f32,
    theme: &'static Theme,
}

impl LevelMeterView {
    pub fn new() -> Self {
        Self {
            left: ChannelMeter::new(DEFAULT_SAMPLE_RATE),
            right: ChannelMeter::new(DEFAULT_SAMPLE_RATE),
            cached_rate: DEFAULT_SAMPLE_RATE,
            theme: crate::theme::dark(),
        }
    }
}

impl View for LevelMeterView {
    fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect) {
        if (frame.sample_rate - self.cached_rate).abs() > 0.5 {
            self.left.reset_filters(frame.sample_rate);
            self.right.reset_filters(frame.sample_rate);
            self.cached_rate = frame.sample_rate;
        }

        let n = frame.new_sample_count;
        let l_src = frame.channels.first().map(|v| v.as_slice()).unwrap_or(&frame.mono);
        let r_src = frame.channels.get(1).map(|v| v.as_slice()).unwrap_or(&frame.mono);
        if n > 0 {
            self.left.process(l_src, n);
            self.right.process(r_src, n);
        }

        if rect.width() < 40.0 || rect.height() < 60.0 { return; }

        let (response, painter) = ui.allocate_painter(rect.size(), egui::Sense::hover());
        let r = response.rect;

        let pad = 6.0;
        let scale_w = 36.0;
        let gap = 4.0;

        let bars_x = r.min.x + pad;
        let bars_w = r.width() - pad * 2.0 - scale_w;
        let bar_w = (bars_w - gap) / 2.0;
        let bar_top = r.min.y + pad;
        let bar_bot = r.max.y - pad - 14.0;
        let bar_h = (bar_bot - bar_top).max(1.0);

        let l_rect = Rect::from_min_size(pos2(bars_x, bar_top), vec2(bar_w, bar_h));
        let r_rect = Rect::from_min_size(pos2(bars_x + bar_w + gap, bar_top), vec2(bar_w, bar_h));
        let scale_x = bars_x + bars_w + pad;

        painter.rect_filled(l_rect, 2.0, self.theme.meter_bg);
        painter.rect_filled(r_rect, 2.0, self.theme.meter_bg);

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
                        painter.rect_filled(clipped, 0.0, self.theme.meter_bar(mid_norm));
                    }
                }
            }

            let peak_norm = lufs_to_norm(meter.peak_lufs);
            if peak_norm > 0.01 {
                let py = bar_rect.max.y - bar_rect.height() * peak_norm;
                let color = if peak_norm > 0.85 {
                    self.theme.meter_peak_high
                } else {
                    self.theme.meter_peak
                };
                painter.line_segment(
                    [pos2(bar_rect.min.x, py), pos2(bar_rect.max.x, py)],
                    Stroke::new(1.5, color),
                );
            }

            painter.rect_stroke(bar_rect, 2.0, Stroke::new(1.0, self.theme.meter_border), egui::StrokeKind::Inside);
        }

        let tick_font = egui::FontId::monospace(9.0);
        let tick_stroke = Stroke::new(1.0, self.theme.meter_scale_tick);

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
                self.theme.meter_scale_text,
            );
        }

        let label_y = bar_bot + 4.0;
        let label_font = egui::FontId::proportional(10.0);

        let fmt_lufs = |v: f32| -> String {
            if v <= LUFS_MIN + 0.5 { "-∞".to_string() }
            else { format!("{:.1}", v) }
        };

        painter.text(pos2(l_rect.center().x, label_y), egui::Align2::CENTER_TOP,
            format!("L {}", fmt_lufs(self.left.lufs)), label_font.clone(), self.theme.meter_label);
        painter.text(pos2(r_rect.center().x, label_y), egui::Align2::CENTER_TOP,
            format!("R {}", fmt_lufs(self.right.lufs)), label_font, self.theme.meter_label);
    }

    fn settings_ui(&mut self, _ui: &mut egui::Ui) {}

    fn needs_repaint(&self) -> bool {
        self.left.is_animating() || self.right.is_animating()
    }
}
