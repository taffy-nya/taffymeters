use eframe::egui;
use taffymeters_core::channel::ChannelMode;

pub struct ScaleControl {
    pub value: f32,
    min: f32,
    max: f32,
    sensitivity: f32,
}

impl ScaleControl {
    pub fn new(value: f32, min: f32, max: f32) -> Self {
        Self { value, min, max, sensitivity: 0.001 }
    }

    pub fn handle_scroll(&mut self, ui: &mut egui::Ui) -> bool {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() < f32::EPSILON { return false; }
        let factor = (1.0 + scroll * self.sensitivity).clamp(0.8, 1.25);
        let new = (self.value * factor).clamp(self.min, self.max);
        if (new - self.value).abs() < f32::EPSILON { return false; }
        self.value = new;
        true
    }
}

pub fn channel_select_ui(ui: &mut egui::Ui, channel: &mut ChannelMode) -> bool {
    let old = *channel;
    ui.horizontal(|ui| {
        ui.selectable_value(channel, ChannelMode::Left, "Left");
        ui.selectable_value(channel, ChannelMode::Mono, "Mono");
        ui.selectable_value(channel, ChannelMode::Right, "Right");
    });
    *channel != old
}
