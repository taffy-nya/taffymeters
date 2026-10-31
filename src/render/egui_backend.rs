//! The egui backend: draws `RenderTarget` calls with an egui painter.

use std::collections::HashMap;

use super::{
    Circle, Color, ColoredRect, Flow, Image, Point, Rect, RenderTarget, Stroke, TextStyle,
};

pub struct EguiRenderTarget<'a> {
    painter: egui::Painter,
    marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> EguiRenderTarget<'a> {
    pub fn new(painter: egui::Painter) -> Self {
        Self {
            painter,
            marker: std::marker::PhantomData,
        }
    }
}

impl RenderTarget for EguiRenderTarget<'_> {
    fn polyline(&mut self, clip: Rect, points: &[Point], stroke: Stroke) {
        if points.len() < 2 {
            return;
        }
        let points = points
            .iter()
            .map(|point| egui::pos2(point.x, point.y))
            .collect();
        self.painter
            .with_clip_rect(to_egui_rect(clip))
            .add(egui::Shape::line(
                points,
                egui::Stroke::new(stroke.width, to_egui_color(stroke.color)),
            ));
    }

    fn fill_rects(&mut self, clip: Rect, rects: &[ColoredRect]) {
        if rects.is_empty() {
            return;
        }
        // Axis-aligned fills need no feathering; one mesh keeps dense views from becoming CPU bound.
        let mut mesh = egui::Mesh::default();
        mesh.reserve_vertices(rects.len() * 4);
        mesh.reserve_triangles(rects.len() * 2);
        for colored in rects {
            mesh.add_colored_rect(to_egui_rect(colored.rect), to_egui_color(colored.color));
        }
        self.painter
            .with_clip_rect(to_egui_rect(clip))
            .add(egui::Shape::mesh(mesh));
    }

    fn circles(&mut self, clip: Rect, circles: &[Circle]) {
        let painter = self.painter.with_clip_rect(to_egui_rect(clip));
        for circle in circles {
            painter.circle_filled(
                egui::pos2(circle.center.x, circle.center.y),
                circle.radius,
                to_egui_color(circle.color),
            );
        }
    }

    fn text(&mut self, clip: Rect, position: Point, text: &str, style: TextStyle) {
        self.painter.with_clip_rect(to_egui_rect(clip)).text(
            egui::pos2(position.x, position.y),
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::monospace(style.size),
            to_egui_color(style.color),
        );
    }

    fn scrolling_image(
        &mut self,
        clip: Rect,
        rect: Rect,
        image: &mut Image,
        first_column: usize,
        flow: Flow,
    ) {
        use std::sync::{Arc, Mutex, PoisonError};

        use egui::{
            Color32, ColorImage, Id, Mesh, Pos2, Shape, TextureOptions, epaint::Vertex, pos2,
        };
        let (width, height) = (image.width(), image.height());
        if width == 0 || height == 0 {
            return;
        }
        let ctx = self.painter.ctx().clone();
        let pass = ctx.cumulative_pass_nr();
        let cache_id = Id::new("taffymeters.images");
        let cache = ctx.data_mut(|data| {
            Arc::clone(data.get_temp_mut_or_default::<Arc<Mutex<ImageCache>>>(cache_id))
        });
        let mut cache = cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .entries
            .retain(|_, entry| pass.saturating_sub(entry.last_pass) < IMAGE_IDLE_PASSES);
        let options = TextureOptions::LINEAR_REPEAT;
        let entry = match cache.entries.get_mut(&image.id()) {
            Some(entry) if entry.revision == image.revision() => {
                // One upload per run of adjacent columns: a fast-scrolling image changes
                // hundreds per pass, and each upload has a fixed cost in egui and wgpu.
                let mut changed = image.changed_columns().to_vec();
                changed.sort_unstable();
                let mut start = 0;
                while start < changed.len() {
                    let mut end = start + 1;
                    while end < changed.len() && changed[end] == changed[end - 1] + 1 {
                        end += 1;
                    }
                    let (first, run) = (changed[start], end - start);
                    let mut pixels = Vec::with_capacity(run * height);
                    for y in 0..height {
                        pixels.extend(
                            (first..first + run).map(|x| to_egui_color(image.column(x)[y])),
                        );
                    }
                    entry.texture.set_partial(
                        [first, 0],
                        ColorImage::new([run, height], pixels),
                        options,
                    );
                    start = end;
                }
                entry
            }
            _ => {
                let mut pixels = Vec::with_capacity(width * height);
                for y in 0..height {
                    pixels.extend((0..width).map(|x| to_egui_color(image.column(x)[y])));
                }
                let texture = ctx.load_texture(
                    format!("taffymeters.image.{}", image.id()),
                    ColorImage::new([width, height], pixels),
                    options,
                );
                cache.entries.insert(
                    image.id(),
                    CachedImage {
                        texture,
                        revision: image.revision(),
                        last_pass: pass,
                    },
                );
                cache
                    .entries
                    .get_mut(&image.id())
                    .expect("the image entry was just inserted")
            }
        };
        entry.last_pass = pass;
        let texture = entry.texture.id();
        image.mark_uploaded();
        // Sample texel centers so the outer rows do not blend with their neighbors. The ring's
        // seam splits the mesh instead of relying on a wrapping sample: egui-wgpu's predictable
        // filtering (the offscreen visual checks) clamps texel coordinates whatever the wrap mode.
        let (w, h) = (width as f32, height as f32);
        let u0 = (first_column % width) as f32 / w + 0.5 / w;
        let (u1, v0, v1) = (u0 + 1.0 - 1.0 / w, 0.5 / h, 1.0 - 0.5 / h);
        let corners = [
            rect.min,
            Point {
                x: rect.max.x,
                y: rect.min.y,
            },
            Point {
                x: rect.min.x,
                y: rect.max.y,
            },
            rect.max,
        ];
        let (mut origin, mut newest, mut far) = (Pos2::ZERO, Pos2::ZERO, Pos2::ZERO);
        for (corner, flow_corner) in corners.into_iter().zip(flow.corners()) {
            let corner = pos2(corner.x, corner.y);
            match flow_corner {
                (0.0, 0.0) => origin = corner,
                (1.0, 0.0) => newest = corner,
                (0.0, 1.0) => far = corner,
                _ => {}
            }
        }
        let (along, across) = (newest - origin, far - origin);
        let mut mesh = Mesh::with_texture(texture);
        let mut quad = |from: f32, to: f32, u_from: f32, u_to: f32| {
            let base = mesh.vertices.len() as u32;
            for (time, u) in [(from, u_from), (to, u_to)] {
                for (cross, v) in [(0.0, v0), (1.0, v1)] {
                    mesh.vertices.push(Vertex {
                        pos: origin + along * time + across * cross,
                        uv: pos2(u, v),
                        color: Color32::WHITE,
                    });
                }
            }
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 2, base + 1, base + 3);
        };
        if u1 <= 1.0 {
            quad(0.0, 1.0, u0, u1);
        } else {
            let seam = (1.0 - u0) / (u1 - u0);
            quad(0.0, seam, u0, 1.0);
            quad(seam, 1.0, 0.0, u1 - 1.0);
        }
        self.painter
            .with_clip_rect(to_egui_rect(clip))
            .add(Shape::mesh(mesh));
    }
}

/// Passes an uploaded image may go undrawn before its texture is released.
const IMAGE_IDLE_PASSES: u64 = 600;

#[derive(Default)]
struct ImageCache {
    entries: HashMap<u64, CachedImage>,
}

struct CachedImage {
    texture: egui::TextureHandle,
    revision: u64,
    last_pass: u64,
}

fn to_egui_rect(rect: Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.min.x, rect.min.y),
        egui::pos2(rect.max.x, rect.max.y),
    )
}

fn to_egui_color(color: Color) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, color.alpha)
}
