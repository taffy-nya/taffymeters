#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub min: Point,
    pub max: Point,
}

/// Width and height in logical points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn width(self) -> f32 {
        (self.max.x - self.min.x).max(0.0)
    }

    pub fn height(self) -> f32 {
        (self.max.y - self.min.y).max(0.0)
    }

    pub fn inset(self, x: f32, y: f32) -> Self {
        Self {
            min: Point {
                x: self.min.x + x,
                y: self.min.y + y,
            },
            max: Point {
                x: self.max.x - x,
                y: self.max.y - y,
            },
        }
    }

    pub fn contains(self, point: Point) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
    }

    pub fn center(self) -> Point {
        Point {
            x: (self.min.x + self.max.x) * 0.5,
            y: (self.min.y + self.max.y) * 0.5,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Color {
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 255,
        }
    }

    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    pub const fn with_alpha(self, alpha: u8) -> Self {
        Self { alpha, ..self }
    }
}

impl Color {
    /// `self` moved toward `other` by `t` in [0, 1], per channel.
    pub fn lerp(self, other: Self, t: f32) -> Self {
        let mix = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t) as u8;
        Self {
            red: mix(self.red, other.red),
            green: mix(self.green, other.green),
            blue: mix(self.blue, other.blue),
            alpha: mix(self.alpha, other.alpha),
        }
    }

    /// `#rrggbb`, or `#rrggbbaa` when not opaque.
    pub fn to_hex(self) -> String {
        if self.alpha == 255 {
            format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                self.red, self.green, self.blue, self.alpha
            )
        }
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('#')?;
        if !digits.is_ascii() || !matches!(digits.len(), 6 | 8) {
            return None;
        }
        let channel = |index: usize| u8::from_str_radix(&digits[index..index + 2], 16).ok();
        Some(Self {
            red: channel(0)?,
            green: channel(2)?,
            blue: channel(4)?,
            alpha: if digits.len() == 8 { channel(6)? } else { 255 },
        })
    }
}

impl serde::Serialize for Color {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> serde::Deserialize<'de> for Color {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid color {text:?}")))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub width: f32,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub size: f32,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColoredRect {
    pub rect: Rect,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Circle {
    pub center: Point,
    pub radius: f32,
    pub color: Color,
}

pub trait RenderTarget {
    fn polyline(&mut self, clip: Rect, points: &[Point], stroke: Stroke);
    fn fill_rects(&mut self, clip: Rect, rects: &[ColoredRect]);
    fn circles(&mut self, clip: Rect, circles: &[Circle]);
    fn text(&mut self, clip: Rect, position: Point, text: &str, style: TextStyle);
}

pub struct EguiRenderTarget<'a> {
    painter: eframe::egui::Painter,
    marker: std::marker::PhantomData<&'a ()>,
}

impl<'a> EguiRenderTarget<'a> {
    pub fn new(painter: eframe::egui::Painter) -> Self {
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
            .map(|point| eframe::egui::pos2(point.x, point.y))
            .collect();
        self.painter
            .with_clip_rect(to_egui_rect(clip))
            .add(eframe::egui::Shape::line(
                points,
                eframe::egui::Stroke::new(stroke.width, to_egui_color(stroke.color)),
            ));
    }

    fn fill_rects(&mut self, clip: Rect, rects: &[ColoredRect]) {
        if rects.is_empty() {
            return;
        }
        // One untessellated mesh for the whole batch: axis-aligned fills need no
        // feathering, and per-rect shapes made dense bar/heat-map views CPU bound.
        let mut mesh = eframe::egui::Mesh::default();
        mesh.reserve_vertices(rects.len() * 4);
        mesh.reserve_triangles(rects.len() * 2);
        for colored in rects {
            mesh.add_colored_rect(to_egui_rect(colored.rect), to_egui_color(colored.color));
        }
        self.painter
            .with_clip_rect(to_egui_rect(clip))
            .add(eframe::egui::Shape::mesh(mesh));
    }

    fn circles(&mut self, clip: Rect, circles: &[Circle]) {
        let painter = self.painter.with_clip_rect(to_egui_rect(clip));
        for circle in circles {
            painter.circle_filled(
                eframe::egui::pos2(circle.center.x, circle.center.y),
                circle.radius,
                to_egui_color(circle.color),
            );
        }
    }

    fn text(&mut self, clip: Rect, position: Point, text: &str, style: TextStyle) {
        self.painter.with_clip_rect(to_egui_rect(clip)).text(
            eframe::egui::pos2(position.x, position.y),
            eframe::egui::Align2::LEFT_TOP,
            text,
            eframe::egui::FontId::monospace(style.size),
            to_egui_color(style.color),
        );
    }
}

fn to_egui_rect(rect: Rect) -> eframe::egui::Rect {
    eframe::egui::Rect::from_min_max(
        eframe::egui::pos2(rect.min.x, rect.min.y),
        eframe::egui::pos2(rect.max.x, rect.max.y),
    )
}

fn to_egui_color(color: Color) -> eframe::egui::Color32 {
    eframe::egui::Color32::from_rgba_unmultiplied(color.red, color.green, color.blue, color.alpha)
}
