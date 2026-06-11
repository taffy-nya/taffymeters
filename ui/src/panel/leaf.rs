use eframe::egui::{self, Color32};
use taffymeters_core::frame::AudioFrame;
use crate::{
    views::{View, ViewType},
    theme::Theme,
};
use super::action::PanelAction;

static PANEL_ID_COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
fn next_panel_id() -> usize {
    PANEL_ID_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

pub struct Panel {
    pub id: usize,
    pub view: Box<dyn View>,
    pub view_type: ViewType,
    overlay_open: bool,
}

impl Panel {
    pub fn new(vt: ViewType) -> Self {
        Self { id: next_panel_id(), view: vt.create(), view_type: vt, overlay_open: false }
    }

    fn set_view(&mut self, vt: ViewType) {
        self.view_type = vt;
        self.view = vt.create();
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, frame: &AudioFrame, rect: egui::Rect, multi: bool, theme: &Theme) -> PanelAction {
        {
            let mut child = ui.new_child(
                egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::LEFT)),
            );
            self.view.handle_input(&mut child, rect);
            self.view.draw(&mut child, frame, rect);
        }

        let body = ui.interact(rect, ui.id().with(("body", self.id)), egui::Sense::click_and_drag());
        if body.secondary_clicked() { self.overlay_open = true; }
        if !self.overlay_open && body.dragged_by(egui::PointerButton::Primary) {
            ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        if self.overlay_open { return self.draw_overlay(ui, rect, multi, theme); }
        self.draw_split_edges(ui, rect, theme)
    }

    fn draw_overlay(&mut self, ui: &mut egui::Ui, rect: egui::Rect, multi: bool, theme: &Theme) -> PanelAction {
        struct Out { close: bool, switch_to: Option<ViewType>, remove: bool }
        let cur = self.view_type;
        let mut out = Out { close: false, switch_to: None, remove: false };

        egui::Area::new(egui::Id::new(("ov", self.id)))
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .show(ui, |ui| {
                ui.set_clip_rect(rect);
                ui.painter().rect_filled(rect, 0.0, theme.overlay_bg);

                let bg = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                if bg.clicked() || bg.secondary_clicked() { out.close = true; }
                if bg.dragged_by(egui::PointerButton::Primary) {
                    ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                let cr = rect.shrink(40.0);
                if !cr.is_positive() { return; }

                ui.scope_builder(egui::UiBuilder::new().max_rect(cr), |ui| {
                    ui.columns(2, |cols| {
                        let mh = cr.height() - 32.0;
                        cols[0].vertical(|ui| {
                            ui.label(egui::RichText::new("MODULE").strong().color(theme.overlay_title));
                            ui.add_space(16.0);
                            egui::ScrollArea::vertical().id_salt("ovL").max_height(mh).show(ui, |ui| {
                                let w = ui.available_width() - 8.0;
                                for &vt in ViewType::ALL {
                                    let sel = vt == cur;
                                    let txt = if sel {
                                        egui::RichText::new(vt.label()).color(theme.overlay_accent).strong()
                                    } else {
                                        egui::RichText::new(vt.label()).color(theme.overlay_text)
                                    };
                                    let btn = egui::Button::new(txt)
                                        .min_size(egui::vec2(w, 32.0))
                                        .fill(if sel { theme.overlay_selected_bg } else { Color32::TRANSPARENT });
                                    if ui.add(btn).clicked() { out.switch_to = Some(vt); out.close = true; }
                                }
                            });
                        });
                        cols[1].vertical(|ui| {
                            ui.label(egui::RichText::new("SETTINGS").strong().color(theme.overlay_title));
                            ui.add_space(16.0);
                            egui::ScrollArea::vertical().id_salt("ovR").max_height(mh).show(ui, |ui| {
                                let w = ui.available_width() - 8.0;
                                if multi {
                                    let b = egui::Button::new(
                                        egui::RichText::new("Close Panel").color(theme.overlay_close)
                                    ).min_size(egui::vec2(w, 32.0)).fill(Color32::TRANSPARENT);
                                    if ui.add(b).clicked() { out.remove = true; out.close = true; }
                                    ui.add_space(12.0); ui.separator(); ui.add_space(12.0);
                                }
                                self.view.settings_ui(ui);
                            });
                        });
                    });
                });
            });

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) { out.close = true; }
        if let Some(vt) = out.switch_to { self.set_view(vt); }
        if out.close { self.overlay_open = false; }
        if out.remove { return PanelAction::Remove(self.id); }
        PanelAction::None
    }

    fn draw_split_edges(&self, ui: &mut egui::Ui, rect: egui::Rect, theme: &Theme) -> PanelAction {
        const E: f32 = 20.0;

        let (hover, dragging) = ui.input(|i| (i.pointer.hover_pos(), i.pointer.is_decidedly_dragging()));
        if dragging { return PanelAction::None; }

        let right  = egui::Rect::from_min_max(egui::pos2(rect.max.x - E, rect.min.y), rect.max);
        let bottom = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - E),
            egui::pos2(rect.max.x - E, rect.max.y),
        );

        let pf = egui::FontId::proportional(22.0);

        if hover.is_some_and(|p| right.contains(p)) {
            ui.painter().rect_filled(right, 0.0, theme.split_hover_bg);
            ui.painter().text(right.center(), egui::Align2::CENTER_CENTER, "+", pf.clone(), theme.split_plus_sign);
        }
        if ui.interact(right, ui.id().with(("er", self.id)), egui::Sense::click()).clicked() {
            return PanelAction::SplitRight(self.id);
        }

        if hover.is_some_and(|p| bottom.contains(p)) {
            ui.painter().rect_filled(bottom, 0.0, theme.split_hover_bg);
            ui.painter().text(bottom.center(), egui::Align2::CENTER_CENTER, "+", pf, theme.split_plus_sign);
        }
        if ui.interact(bottom, ui.id().with(("eb", self.id)), egui::Sense::click()).clicked() {
            return PanelAction::SplitDown(self.id);
        }

        PanelAction::None
    }
}
