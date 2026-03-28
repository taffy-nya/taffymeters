use eframe::egui::{self, Color32};
use taffymeters_core::signal::AudioData;
use crate::views::{traits::View, ViewType};

pub enum PanelAction {
    None,
    Split(SplitDir),
    Remove,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SplitDir { Horizontal, Vertical }

pub struct Panel {
    pub view: Box<dyn View>,
    pub view_type: ViewType,
    overlay_open: bool,
}

impl Panel {
    pub fn new(view_type: ViewType) -> Self {
        Self { view: view_type.create(), view_type, overlay_open: false }
    }

    pub fn set_view(&mut self, view_type: ViewType) {
        self.view_type = view_type;
        self.view = view_type.create();
    }

    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
        data: &AudioData,
        idx: usize,         // 用于生成唯一 egui ID
        multi_panel: bool,  // 控制是否显示关闭按钮（单面板时不显示）
    ) -> PanelAction {
        let desired = ui.available_size_before_wrap();
        let rect = egui::Rect::from_min_size(ui.cursor().min, desired);
        let body = ui.interact(rect, ui.id().with(("body", idx)), egui::Sense::click_and_drag());
        
        if body.secondary_clicked() {
            self.overlay_open = true;
        }

        let builder = egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::LEFT));
        let mut child = ui.new_child(builder);
        self.view.draw(&mut child, data);

        if body.dragged_by(egui::PointerButton::Primary) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        if self.overlay_open {
            return self.draw_overlay(ui, rect, idx, multi_panel);
        }

        self.draw_split_edges(ui, rect, idx)
    }

    fn draw_overlay(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        idx: usize,
        multi_panel: bool,
    ) -> PanelAction {
        struct Outcome {
            close: bool,
            switch_to: Option<ViewType>,
            remove: bool,
        }

        let current_type = self.view_type;
        let mut out = Outcome { close: false, switch_to: None, remove: false };

        let bg_id = egui::Id::new(("panel_bg", idx));
        egui::Area::new(bg_id)
            .fixed_pos(rect.min)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.set_clip_rect(rect);
                
                ui.painter().rect_filled(
                    rect, 0.0,
                    egui::Color32::from_rgba_unmultiplied(0, 0, 0, 190),
                );
                
                let bg = ui.allocate_rect(rect, egui::Sense::click_and_drag());
                if bg.clicked() || bg.secondary_clicked() {
                    out.close = true;
                }
                if bg.dragged_by(egui::PointerButton::Primary) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                let padding = 40.0;
                let content_rect = rect.shrink(padding);
                
                if content_rect.is_positive() {
                    let builder = egui::UiBuilder::new().max_rect(content_rect);
                    ui.scope_builder(builder, |ui| {
                        
                        ui.columns(2, |cols| {
                            let max_h = content_rect.height() - 32.0;

                            cols[0].vertical(|ui| {
                                ui.label(egui::RichText::new("MODULE").strong().color(egui::Color32::from_gray(160)));
                                ui.add_space(16.0);

                                egui::ScrollArea::vertical().id_salt("left_scroll").max_height(max_h).show(ui, |ui| {
                                    let btn_w = ui.available_width() - 8.0; 

                                    for &vt in ViewType::ALL {
                                        let selected = vt == current_type;
                                        let text = if selected {
                                            egui::RichText::new(vt.label()).color(egui::Color32::from_rgb(100, 180, 255)).strong()
                                        } else {
                                            egui::RichText::new(vt.label()).color(egui::Color32::LIGHT_GRAY)
                                        };

                                        let btn = egui::Button::new(text)
                                            .min_size(egui::vec2(btn_w, 32.0))
                                            .fill(if selected {
                                                egui::Color32::from_rgba_unmultiplied(100, 180, 255, 25)
                                            } else {
                                                egui::Color32::TRANSPARENT
                                            });

                                        if ui.add(btn).clicked() {
                                            out.switch_to = Some(vt);
                                        }
                                    }
                                });
                            });

                            cols[1].vertical(|ui| {
                                ui.label(egui::RichText::new("SETTINGS").strong().color(egui::Color32::from_gray(160)));
                                ui.add_space(16.0);

                                egui::ScrollArea::vertical().id_salt("right_scroll").max_height(max_h).show(ui, |ui| {
                                    let btn_w = ui.available_width() - 8.0;
                                    
                                    if multi_panel {
                                        let close_btn = egui::Button::new(
                                            egui::RichText::new("Close Panel").color(egui::Color32::from_rgb(220, 80, 80))
                                        )
                                        .min_size(egui::vec2(btn_w, 32.0))
                                        .fill(egui::Color32::TRANSPARENT);

                                        if ui.add(close_btn).clicked() {
                                            out.remove = true;
                                            out.close = true;
                                        }
                                        
                                        ui.add_space(12.0);
                                        ui.separator();
                                        ui.add_space(12.0);
                                    }

                                    self.view.settings_ui(ui);
                                });
                            });
                        });
                    });
                }
            });

        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            out.close = true;
        }

        if let Some(vt) = out.switch_to { self.set_view(vt); }
        if out.close { self.overlay_open = false; }
        if out.remove { return PanelAction::Remove; }

        PanelAction::None
    }

    fn draw_split_edges(&self, ui: &mut egui::Ui, rect: egui::Rect, idx: usize) -> PanelAction {
        const EDGE: f32 = 20.0;
        let hover = ui.input(|i| i.pointer.hover_pos());

        let right = egui::Rect::from_min_max(
            egui::pos2(rect.max.x - EDGE, rect.min.y),
            rect.max,
        );
        let bottom = egui::Rect::from_min_max(
            egui::pos2(rect.min.x, rect.max.y - EDGE),
            egui::pos2(rect.max.x - EDGE, rect.max.y),
        );

        let edge_color = Color32::from_rgba_unmultiplied(100, 180, 255, 50);
        let plus_color = Color32::from_rgba_unmultiplied(200, 225, 255, 210);
        let plus_font = egui::FontId::proportional(22.0);

        let right_hovered = hover.map_or(false, |p| right.contains(p));
        if right_hovered {
            ui.painter().rect_filled(right, 0.0, edge_color);
            ui.painter().text(right.center(), egui::Align2::CENTER_CENTER, "+", plus_font.clone(), plus_color);
        }
        let r_resp = ui.interact(right, ui.id().with(("edge_r", idx)), egui::Sense::click());
        if r_resp.clicked() { return PanelAction::Split(SplitDir::Horizontal); }

        let bottom_hovered = hover.map_or(false, |p| bottom.contains(p));
        if bottom_hovered {
            ui.painter().rect_filled(bottom, 0.0, edge_color);
            ui.painter().text(bottom.center(), egui::Align2::CENTER_CENTER, "+", plus_font, plus_color);
        }
        let b_resp = ui.interact(bottom, ui.id().with(("edge_b", idx)), egui::Sense::click());
        if b_resp.clicked() { return PanelAction::Split(SplitDir::Vertical); }

        PanelAction::None
    }
}

pub struct PanelLayout {
    pub panels: Vec<Panel>,
    pub split: Option<SplitDir>,
    pub selected: usize,
}

impl PanelLayout {
    pub fn single(view_type: ViewType) -> Self {
        Self { panels: vec![Panel::new(view_type)], split: None, selected: 0 }
    }

    pub fn split(&mut self, dir: SplitDir) {
        let new_type = self.panels[self.selected].view_type;
        let insert_at = self.selected + 1;
        self.panels.insert(insert_at, Panel::new(new_type));
        self.split = Some(dir);
        self.selected = insert_at;
    }

    pub fn remove_selected(&mut self) {
        if self.panels.len() <= 1 { return; }
        self.panels.remove(self.selected);
        self.selected = self.selected.saturating_sub(1);
        if self.panels.len() == 1 { self.split = None; }
    }

    pub fn draw(&mut self, ui: &mut egui::Ui, data: &AudioData) {
        let full = ui.max_rect();
        let n = self.panels.len();
        let multi = n > 1;

        if multi {
            self.draw_dividers(ui, full, n);
        }

        let mut pending: Option<(usize, PanelAction)> = None;

        for i in 0..n {
            let panel_rect = self.panel_rect(full, i, n);
            let builder = egui::UiBuilder::new()
                .max_rect(panel_rect)
                .layout(egui::Layout::top_down(egui::Align::LEFT));
            let mut child = ui.new_child(builder);
            let action = self.panels[i].draw(&mut child, data, i, multi);
            if !matches!(action, PanelAction::None) && pending.is_none() {
                pending = Some((i, action));
            }
        }

        if let Some((idx, action)) = pending {
            self.selected = idx;
            match action {
                PanelAction::Split(dir) => self.split(dir),
                PanelAction::Remove => self.remove_selected(),
                PanelAction::None => {}
            }
        }
    }

    fn panel_rect(&self, full: egui::Rect, i: usize, n: usize) -> egui::Rect {
        match self.split {
            None => full,
            Some(SplitDir::Horizontal) => {
                let w = full.width() / n as f32;
                egui::Rect::from_min_max(
                    egui::pos2(full.min.x + w * i as f32, full.min.y),
                    egui::pos2(full.min.x + w * (i + 1) as f32, full.max.y),
                )
            }
            Some(SplitDir::Vertical) => {
                let h = full.height() / n as f32;
                egui::Rect::from_min_max(
                    egui::pos2(full.min.x, full.min.y + h * i as f32),
                    egui::pos2(full.max.x, full.min.y + h * (i + 1) as f32),
                )
            }
        }
    }

    fn draw_dividers(&self, ui: &mut egui::Ui, full: egui::Rect, n: usize) {
        let stroke = egui::Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 30));
        for i in 1..n {
            let (a, b) = match self.split {
                Some(SplitDir::Horizontal) => {
                    let w = full.width() / n as f32;
                    let x = full.min.x + w * i as f32;
                    (egui::pos2(x, full.min.y), egui::pos2(x, full.max.y))
                }
                Some(SplitDir::Vertical) => {
                    let h = full.height() / n as f32;
                    let y = full.min.y + h * i as f32;
                    (egui::pos2(full.min.x, y), egui::pos2(full.max.x, y))
                }
                None => continue,
            };
            ui.painter().line_segment([a, b], stroke);
        }
    }
}
