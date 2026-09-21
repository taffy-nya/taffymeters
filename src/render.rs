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
        let painter = self.painter.with_clip_rect(to_egui_rect(clip));
        for colored in rects {
            painter.rect_filled(
                to_egui_rect(colored.rect),
                0.0,
                to_egui_color(colored.color),
            );
        }
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
