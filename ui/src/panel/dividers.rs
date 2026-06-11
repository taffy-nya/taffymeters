use eframe::egui::{self, Stroke};
use crate::theme::Theme;
use super::action::Dir;

const DIV_HALF: f32 = 3.0;

pub fn split_rect(rect: egui::Rect, dir: Dir, ratio: f32) -> (egui::Rect, egui::Rect, egui::Rect) {
    match dir {
        Dir::H => {
            let x = rect.min.x + rect.width() * ratio;
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(x, rect.max.y)),
                egui::Rect::from_min_max(
                    egui::pos2(x - DIV_HALF, rect.min.y),
                    egui::pos2(x + DIV_HALF, rect.max.y),
                ),
                egui::Rect::from_min_max(egui::pos2(x, rect.min.y), rect.max),
            )
        }
        Dir::V => {
            let y = rect.min.y + rect.height() * ratio;
            (
                egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, y)),
                egui::Rect::from_min_max(
                    egui::pos2(rect.min.x, y - DIV_HALF),
                    egui::pos2(rect.max.x, y + DIV_HALF),
                ),
                egui::Rect::from_min_max(egui::pos2(rect.min.x, y), rect.max),
            )
        }
    }
}

pub fn draw_divider(ui: &mut egui::Ui, div: egui::Rect, dir: Dir, ratio: &mut f32, parent: egui::Rect, split_id: u64, theme: &Theme) -> egui::Response {
    let id = ui.id().with(("div", split_id));
    let resp = ui.interact(div, id, egui::Sense::click_and_drag());

    if resp.hovered() || resp.dragged() {
        ui.set_cursor_icon(match dir {
            Dir::H => egui::CursorIcon::ResizeHorizontal,
            Dir::V => egui::CursorIcon::ResizeVertical,
        });
    }
    if resp.dragged() {
        let (span, d) = match dir {
            Dir::H => (parent.width(), resp.drag_delta().x),
            Dir::V => (parent.height(), resp.drag_delta().y),
        };
        if span > 0.0 { *ratio = (*ratio + d / span).clamp(0.05, 0.95); }
    }

    let color = if resp.hovered() || resp.dragged() {
        theme.divider_hover
    } else {
        theme.divider_normal
    };
    let c = div.center();
    match dir {
        Dir::H => ui.painter().line_segment(
            [egui::pos2(c.x, div.min.y), egui::pos2(c.x, div.max.y)],
            Stroke::new(1.0, color),
        ),
        Dir::V => ui.painter().line_segment(
            [egui::pos2(div.min.x, c.y), egui::pos2(div.max.x, c.y)],
            Stroke::new(1.0, color),
        ),
    };

    resp
}
