use eframe::egui::Color32;
use std::sync::OnceLock;

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + t * (b as f32 - a as f32)) as u8
}

pub struct Theme {
    pub background: Color32,
    pub line: Color32,
    pub waveform_gradient: &'static [(f32, (u8, u8, u8))],
    pub heatmap_max_db: f32,
    pub meter_bg: Color32,
    pub meter_border: Color32,
    pub meter_scale_text: Color32,
    pub meter_scale_tick: Color32,
    pub meter_label: Color32,
    pub meter_peak: Color32,
    pub meter_peak_high: Color32,
    pub meter_bar_low: Color32,
    pub meter_bar_mid_start: Color32,
    pub meter_bar_mid_end: Color32,
    pub meter_bar_high: Color32,
    pub goniometer_guide: Color32,
    pub goniometer_points: Color32,
    pub overlay_bg: Color32,
    pub overlay_title: Color32,
    pub overlay_text: Color32,
    pub overlay_accent: Color32,
    pub overlay_selected_bg: Color32,
    pub overlay_close: Color32,
    pub split_hover_bg: Color32,
    pub split_plus_sign: Color32,
    pub divider_hover: Color32,
    pub divider_normal: Color32,
}

static DARK: OnceLock<Theme> = OnceLock::new();

pub fn dark() -> &'static Theme {
    DARK.get_or_init(|| Theme {
        background:          Color32::from_rgb(8, 8, 10),
        line:                Color32::LIGHT_BLUE,
        waveform_gradient:   &[
            (0.00, (173, 216, 230)),
            (0.25, (  0, 150, 255)),
            (0.55, (140,  80, 255)),
            (0.78, (255,  50, 180)),
            (1.00, (255,  30,  80)),
        ],
        heatmap_max_db:      2.5,
        meter_bg:            Color32::from_rgba_unmultiplied(20, 20, 20, 200),
        meter_border:        Color32::from_gray(50),
        meter_scale_text:    Color32::from_gray(140),
        meter_scale_tick:    Color32::from_gray(55),
        meter_label:         Color32::from_gray(160),
        meter_peak:          Color32::WHITE,
        meter_peak_high:     Color32::from_rgb(255, 80, 60),
        meter_bar_low:       Color32::LIGHT_BLUE,
        meter_bar_mid_start: Color32::from_rgb(60, 200, 80),
        meter_bar_mid_end:   Color32::from_rgb(255, 200, 80),
        meter_bar_high:      Color32::from_rgb(220, 60, 40),
        goniometer_guide:    Color32::from_gray(60),
        goniometer_points:   Color32::from_rgb(100, 220, 255),
        overlay_bg:          Color32::from_rgba_unmultiplied(0, 0, 0, 190),
        overlay_title:       Color32::from_gray(160),
        overlay_text:        Color32::LIGHT_GRAY,
        overlay_accent:      Color32::from_rgb(100, 180, 255),
        overlay_selected_bg: Color32::from_rgba_unmultiplied(100, 180, 255, 25),
        overlay_close:       Color32::from_rgb(220, 80, 80),
        split_hover_bg:      Color32::from_rgba_unmultiplied(100, 180, 255, 50),
        split_plus_sign:     Color32::from_rgba_unmultiplied(200, 225, 255, 210),
        divider_hover:       Color32::from_rgba_unmultiplied(100, 180, 255, 160),
        divider_normal:      Color32::from_rgba_unmultiplied(255, 255, 255, 30),
    })
}

impl Theme {
    pub fn waveform_amplitude(&self, amp: f32) -> Color32 {
        let amp = amp.clamp(0.0, 1.0);
        for w in self.waveform_gradient.windows(2) {
            let (t0, c0) = w[0];
            let (t1, c1) = w[1];
            if amp <= t1 {
                let t = if (t1 - t0).abs() < 1e-6 { 0.0 } else { (amp - t0) / (t1 - t0) };
                return Color32::from_rgb(lerp_u8(c0.0, c1.0, t), lerp_u8(c0.1, c1.1, t), lerp_u8(c0.2, c1.2, t));
            }
        }
        Color32::from_rgb(255, 40, 40)
    }

    pub fn heatmap_db(&self, val: f32) -> Color32 {
        let t = (val / self.heatmap_max_db).clamp(0.0, 1.0);
        if t < 0.05 { return Color32::TRANSPARENT; }
        let r = (t * 3.0 - 1.0).clamp(0.0, 1.0) * 255.0;
        let g = (t * 3.0 - 2.0).clamp(0.0, 1.0) * 255.0;
        let b = (t * 3.0).clamp(0.0, 1.0) * 255.0;
        let a = t * 255.0;
        Color32::from_rgba_unmultiplied(r as u8, g as u8, b as u8, a as u8)
    }

    pub fn meter_bar(&self, norm: f32) -> Color32 {
        if norm < 0.70 {
            self.meter_bar_low
        } else if norm < 0.85 {
            let t = (norm - 0.70) / 0.15;
            Color32::from_rgb(
                lerp_u8(self.meter_bar_mid_start.r(), self.meter_bar_mid_end.r(), t),
                lerp_u8(self.meter_bar_mid_start.g(), self.meter_bar_mid_end.g(), t),
                lerp_u8(self.meter_bar_mid_start.b(), self.meter_bar_mid_end.b(), t),
            )
        } else {
            self.meter_bar_high
        }
    }
}
