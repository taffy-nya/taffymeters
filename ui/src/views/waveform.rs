use eframe::egui;
use crate::view_state::{ViewInteractionState, ZoomAnchor};

pub fn draw(ui: &mut egui::Ui, audio_data: &[f32], state: &mut ViewInteractionState) {
    let desired_size = ui.available_size_before_wrap();
    let (response, painter) = ui.allocate_painter(desired_size, egui::Sense::hover());
    let rect = response.rect;

    if response.hovered() {
        let scroll = ui.input(|i| {
            let dy = i.smooth_scroll_delta.y;
            if dy.abs() > f32::EPSILON {
                dy
            } else {
                i.raw_scroll_delta.y
            }
        });
        state.apply_scroll(scroll);
    }

    if audio_data.len() < 2 || rect.width() <= 1.0 || rect.height() <= 1.0 {
        return;
    }

    let step = ((audio_data.len() as f32) / rect.width().max(1.0)).ceil() as usize;
    let step = step.max(1);
    let half_h = rect.height() * 0.5;
    let center_y = rect.center().y;
    let count = ((audio_data.len() - 1) / step) + 1;
    let denom = (count.saturating_sub(1)).max(1) as f32;

    let mut points = Vec::with_capacity(count);
    for (idx, &sample) in audio_data.iter().step_by(step).enumerate() {
        let t = idx as f32 / denom;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let y = match state.anchor {
            ZoomAnchor::Center => center_y - sample.clamp(-1.0, 1.0) * half_h * state.y_zoom,
            ZoomAnchor::Bottom => rect.bottom() - sample.clamp(0.0, 1.0) * rect.height() * state.y_zoom,
        };
        points.push(egui::pos2(x, y));
    }

    painter.add(egui::Shape::line(
        points,
        egui::Stroke::new(1.5, egui::Color32::LIGHT_BLUE),
    ));
}
