//! egui's picker hard-codes its numeric row and keeps its sliders private, so the
//! picker is drawn here: RGBA and hex fields over the same plane and bars.

use eframe::egui::{
    self, Color32, DragValue, Id, Key, Mesh, PopupCloseBehavior, Rect, Response, Sense, Shape,
    Stroke, StrokeKind, TextEdit, Ui, Vec2,
    color_picker::show_color_at,
    ecolor::{Hsva, HsvaGamma},
    lerp, pos2, remap_clamp, vec2,
};

use super::{PickerPart, SettingsItem, widgets::record};
use crate::{
    render::Color,
    workspace::window::{Controls, color32},
};

/// Logical points; the plane is this wide and square.
const PICKER_WIDTH: f32 = 280.0;
/// Leaves the hex field room for `#rrggbbaa`.
const CHANNEL_WIDTH: f32 = 44.0;
/// Gradient segments per axis; a multiple of 6 so the hue bar meets every primary
/// and secondary hue.
const STEPS: u32 = 36;

/// The hue and saturation of a gray cannot be recovered from RGB, so they are kept
/// while the picker shows the color it last produced.
#[derive(Clone, Copy)]
struct Picking {
    hsva: HsvaGamma,
    color: Color,
}

/// Returns the color picked in this pass, if it differs from `current`.
pub(super) fn color_picker(
    ui: &mut Ui,
    current: Color,
    key: impl Fn(PickerPart) -> SettingsItem,
    controls: &mut Controls,
) -> Option<Color> {
    let id = Id::new(key(PickerPart::Button));
    let button = color_button(ui, id, current);
    record(controls, key(PickerPart::Button), &button);
    egui::Popup::menu(&button)
        .id(id.with("popup"))
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| picker(ui, id, current, &key, controls))
        .and_then(|shown| shown.inner)
        .filter(|picked| *picked != current)
}

fn color_button(ui: &mut Ui, id: Id, color: Color) -> Response {
    let (rect, _) = ui.allocate_exact_size(ui.spacing().interact_size, Sense::hover());
    let response = ui.interact(rect, id, Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        show_color_at(ui.painter(), color32(color), rect.shrink(1.0));
        ui.painter()
            .rect_stroke(rect, 2.0, (1.0, visuals.bg_fill), StrokeKind::Inside);
    }
    response
}

fn picker(
    ui: &mut Ui,
    id: Id,
    current: Color,
    key: &impl Fn(PickerPart) -> SettingsItem,
    controls: &mut Controls,
) -> Option<Color> {
    ui.set_width(PICKER_WIDTH);
    ui.spacing_mut().item_spacing = vec2(4.0, 6.0);
    let state = id.with("hsva");
    let mut hsva = ui
        .data(|data| data.get_temp::<Picking>(state))
        .filter(|picking| picking.color == current)
        .map_or_else(|| HsvaGamma::from(hsva_of(current)), |picking| picking.hsva);
    let mut picked = None;

    ui.horizontal(|ui| {
        let mut channels = [current.red, current.green, current.blue, current.alpha];
        let mut edited = false;
        for (index, (channel, prefix)) in channels
            .iter_mut()
            .zip(["R ", "G ", "B ", "A "])
            .enumerate()
        {
            let response = ui.add_sized(
                vec2(CHANNEL_WIDTH, ui.spacing().interact_size.y),
                DragValue::new(channel).speed(0.5).prefix(prefix),
            );
            record(controls, key(PickerPart::Channel(index as u8)), &response);
            edited |= response.changed();
        }
        if edited {
            let [red, green, blue, alpha] = channels;
            picked = Some(Color::rgba(red, green, blue, alpha));
        }
        if let Some(color) = hex_field(ui, id.with("hex"), current, key, controls) {
            picked = Some(color);
        }
    });
    if let Some(color) = picked {
        if (color.red, color.green, color.blue) == (current.red, current.green, current.blue) {
            hsva.a = alpha_of(color);
        } else {
            hsva = HsvaGamma::from(hsva_of(color));
        }
    }

    let (bar, _) = ui.allocate_exact_size(
        vec2(PICKER_WIDTH, ui.spacing().interact_size.y),
        Sense::hover(),
    );
    show_color_at(ui.painter(), color32(picked.unwrap_or(current)), bar);

    let opaque = HsvaGamma { a: 1.0, ..hsva };
    let HsvaGamma { h, s, v, a } = &mut hsva;
    let mut slid = false;
    let response = plane(ui, s, v, |s, v| HsvaGamma { s, v, ..opaque }.into());
    record(controls, key(PickerPart::Plane), &response);
    slid |= response.changed();
    let response = bar_slider(ui, h, |h| {
        HsvaGamma {
            h,
            s: 1.0,
            v: 1.0,
            a: 1.0,
        }
        .into()
    });
    record(controls, key(PickerPart::Hue), &response);
    slid |= response.changed();
    let response = bar_slider(ui, a, |a| HsvaGamma { a, ..opaque }.into());
    record(controls, key(PickerPart::Alpha), &response);
    slid |= response.changed();
    if slid {
        picked = Some(color_of(hsva));
    }

    let shown = picked.unwrap_or(current);
    ui.data_mut(|data| data.insert_temp(state, Picking { hsva, color: shown }));
    picked
}

/// Commits on Enter or when focus leaves; Escape drops the edit.
fn hex_field(
    ui: &mut Ui,
    id: Id,
    current: Color,
    key: &impl Fn(PickerPart) -> SettingsItem,
    controls: &mut Controls,
) -> Option<Color> {
    let buffer = id.with("text");
    let mut text = ui
        .data(|data| data.get_temp::<String>(buffer))
        .unwrap_or_else(|| current.to_hex());
    let response = ui.add(
        TextEdit::singleline(&mut text)
            .id(id)
            .font(egui::TextStyle::Monospace)
            .desired_width(ui.available_width()),
    );
    record(controls, key(PickerPart::Hex), &response);
    if response.has_focus() {
        ui.data_mut(|data| data.insert_temp(buffer, text));
        return None;
    }
    ui.data_mut(|data| data.remove::<String>(buffer));
    (response.lost_focus() && !ui.input(|input| input.key_pressed(Key::Escape)))
        .then(|| parse_hex(&text))
        .flatten()
}

fn parse_hex(text: &str) -> Option<Color> {
    let text = text.trim();
    Color::from_hex(text).or_else(|| Color::from_hex(&format!("#{text}")))
}

/// egui's `from_srgba_unmultiplied` goes through premultiplied `Color32`, which rounds
/// RGB by alpha and turns every color black (hue red) at zero alpha.
fn hsva_of(color: Color) -> Hsva {
    Hsva {
        a: alpha_of(color),
        ..Hsva::from_srgb([color.red, color.green, color.blue])
    }
}

fn alpha_of(color: Color) -> f32 {
    f32::from(color.alpha) / 255.0
}

fn color_of(hsva: HsvaGamma) -> Color {
    let [red, green, blue, alpha] = Hsva::from(hsva).to_srgba_unmultiplied();
    Color::rgba(red, green, blue, alpha)
}

fn contrast(color: Color32) -> Color32 {
    if color.intensity() < 0.5 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

/// Saturation grows to the right, value upward.
fn plane(
    ui: &mut Ui,
    s: &mut f32,
    v: &mut f32,
    color_at: impl Fn(f32, f32) -> Color32,
) -> Response {
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::splat(PICKER_WIDTH), Sense::click_and_drag());
    if let Some(pointer) = response.interact_pointer_pos() {
        *s = remap_clamp(pointer.x, rect.left()..=rect.right(), 0.0..=1.0);
        *v = remap_clamp(pointer.y, rect.bottom()..=rect.top(), 0.0..=1.0);
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let mut mesh = Mesh::default();
        for row in 0..=STEPS {
            for column in 0..=STEPS {
                let x = column as f32 / STEPS as f32;
                let y = row as f32 / STEPS as f32;
                mesh.colored_vertex(
                    pos2(
                        lerp(rect.left()..=rect.right(), x),
                        lerp(rect.bottom()..=rect.top(), y),
                    ),
                    color_at(x, y),
                );
                if row < STEPS && column < STEPS {
                    let corner = row * (STEPS + 1) + column;
                    let above = corner + STEPS + 1;
                    mesh.add_triangle(corner, corner + 1, above);
                    mesh.add_triangle(corner + 1, above, above + 1);
                }
            }
        }
        let painter = ui.painter();
        painter.add(Shape::mesh(mesh));
        let visuals = ui.style().interact(&response);
        painter.rect_stroke(rect, 0.0, visuals.bg_stroke, StrokeKind::Inside);
        let picked = color_at(*s, *v);
        painter.circle(
            pos2(
                lerp(rect.left()..=rect.right(), *s),
                lerp(rect.bottom()..=rect.top(), *v),
            ),
            rect.width() / 12.0,
            picked,
            Stroke::new(visuals.fg_stroke.width, contrast(picked)),
        );
    }
    response
}

fn bar_slider(ui: &mut Ui, value: &mut f32, color_at: impl Fn(f32) -> Color32) -> Response {
    let (rect, mut response) = ui.allocate_exact_size(
        vec2(PICKER_WIDTH, ui.spacing().interact_size.y),
        Sense::click_and_drag(),
    );
    if let Some(pointer) = response.interact_pointer_pos() {
        *value = remap_clamp(pointer.x, rect.left()..=rect.right(), 0.0..=1.0);
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        checkers(painter, rect);
        let mut mesh = Mesh::default();
        for step in 0..=STEPS {
            let t = step as f32 / STEPS as f32;
            let x = lerp(rect.left()..=rect.right(), t);
            mesh.colored_vertex(pos2(x, rect.top()), color_at(t));
            mesh.colored_vertex(pos2(x, rect.bottom()), color_at(t));
            if step < STEPS {
                let left = 2 * step;
                mesh.add_triangle(left, left + 1, left + 2);
                mesh.add_triangle(left + 1, left + 2, left + 3);
            }
        }
        painter.add(Shape::mesh(mesh));
        let visuals = ui.style().interact(&response);
        painter.rect_stroke(rect, 0.0, visuals.bg_stroke, StrokeKind::Inside);
        let x = lerp(rect.left()..=rect.right(), *value);
        let half = rect.height() / 4.0;
        let picked = color_at(*value);
        painter.add(Shape::convex_polygon(
            vec![
                pos2(x, rect.center().y),
                pos2(x + half, rect.bottom()),
                pos2(x - half, rect.bottom()),
            ],
            picked,
            Stroke::new(visuals.fg_stroke.width, contrast(picked)),
        ));
    }
    response
}

/// Same grays as egui's `show_color_at`, so the bars and the color bar match.
fn checkers(painter: &egui::Painter, rect: Rect) {
    let cell = rect.height() / 2.0;
    let mut mesh = Mesh::default();
    mesh.add_colored_rect(rect, Color32::from_gray(32));
    let columns = (rect.width() / cell).round() as u32;
    for column in 0..columns {
        let x = lerp(rect.left()..=rect.right(), column as f32 / columns as f32);
        let y = if column % 2 == 0 {
            rect.top()
        } else {
            rect.center().y
        };
        mesh.add_colored_rect(
            Rect::from_min_size(pos2(x, y), Vec2::splat(cell)),
            Color32::from_gray(128),
        );
    }
    painter.add(Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_input_accepts_the_saved_forms_with_or_without_the_hash() {
        let opaque = Some(Color::rgb(0x89, 0xb4, 0xfa));
        assert_eq!(parse_hex("#89b4fa"), opaque);
        assert_eq!(parse_hex("89B4FA"), opaque);
        assert_eq!(parse_hex("  #89b4fa\n"), opaque);
        assert_eq!(
            parse_hex("89b4fa80"),
            Some(Color::rgba(0x89, 0xb4, 0xfa, 0x80))
        );
        for text in [
            "",
            "#",
            "##89b4fa",
            "#89b4f",
            "89b4fa8",
            "#fff",
            "rgb(1, 2, 3)",
            "+1+2+3",
        ] {
            assert_eq!(parse_hex(text), None, "{text:?}");
        }
    }

    #[test]
    fn hsv_round_trip_keeps_every_channel_at_any_alpha() {
        for alpha in [0, 1, 128, 255] {
            for red in (0..=255_u8).step_by(15) {
                for green in (0..=255_u8).step_by(15) {
                    for blue in (0..=255_u8).step_by(15) {
                        let color = Color::rgba(red, green, blue, alpha);
                        assert_eq!(color_of(HsvaGamma::from(hsva_of(color))), color);
                    }
                }
            }
        }
    }
}
