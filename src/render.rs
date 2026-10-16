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

/// An RGBA image a Module keeps and updates in place, stored column by column (views
/// that scroll in time write one column per result). A render target uploads only
/// the columns marked changed since it last drew the image; one it has not seen
/// (first draw, another window) gets the whole image.
pub struct Image {
    id: u64,
    width: usize,
    height: usize,
    /// Column-major: column `x` is `pixels[x * height..(x + 1) * height]`, top first.
    pixels: Vec<Color>,
    /// Bumped by `resize`: a target holding another revision uploads everything.
    revision: u64,
    /// Columns changed since the last upload, each once (`marked` flags them).
    dirty: Vec<usize>,
    marked: Vec<bool>,
}

impl Image {
    pub fn new() -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            width: 0,
            height: 0,
            pixels: Vec::new(),
            revision: 0,
            dirty: Vec::new(),
            marked: Vec::new(),
        }
    }

    /// Process-unique identity, for render targets that cache uploaded images.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    /// Set the size and clear every pixel to transparent; the next draw uploads
    /// the whole image.
    pub fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.pixels.clear();
        self.pixels.resize(width * height, Color::rgba(0, 0, 0, 0));
        self.revision += 1;
        self.dirty.clear();
        self.marked.clear();
        self.marked.resize(width, false);
    }

    /// Column `x` (top first) to rewrite; it is uploaded on the next draw.
    pub fn column_mut(&mut self, x: usize) -> &mut [Color] {
        if !self.marked[x] {
            self.marked[x] = true;
            self.dirty.push(x);
        }
        &mut self.pixels[x * self.height..(x + 1) * self.height]
    }

    pub fn column(&self, x: usize) -> &[Color] {
        &self.pixels[x * self.height..(x + 1) * self.height]
    }

    /// Columns changed since the last upload.
    pub fn changed_columns(&self) -> &[usize] {
        &self.dirty
    }

    /// A target uploaded the image: nothing is pending any more.
    pub fn mark_uploaded(&mut self) {
        for &x in &self.dirty {
            self.marked[x] = false;
        }
        self.dirty.clear();
    }
}

impl Default for Image {
    fn default() -> Self {
        Self::new()
    }
}

pub trait RenderTarget {
    fn polyline(&mut self, clip: Rect, points: &[Point], stroke: Stroke);
    fn fill_rects(&mut self, clip: Rect, rects: &[ColoredRect]);
    fn circles(&mut self, clip: Rect, circles: &[Circle]);
    fn text(&mut self, clip: Rect, position: Point, text: &str, style: TextStyle);
    /// `image` stretched over `rect` with smooth (bilinear) filtering, read as a
    /// horizontal ring: the left edge shows column `first_column`, the image wraps
    /// around to `first_column - 1` at the right edge.
    fn scrolling_image(&mut self, clip: Rect, rect: Rect, image: &mut Image, first_column: usize);
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

    fn scrolling_image(&mut self, clip: Rect, rect: Rect, image: &mut Image, first_column: usize) {
        use eframe::egui::{
            Color32, ColorImage, Id, Mesh, Shape, TextureOptions, epaint::Rect as UvRect, pos2,
        };
        let (width, height) = (image.width(), image.height());
        if width == 0 || height == 0 {
            return;
        }
        let ctx = self.painter.ctx().clone();
        let pass = ctx.cumulative_pass_nr();
        let cache_id = Id::new("taffymeters.images");
        // Shared per context; the map itself is not copied per draw.
        let cache = ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<std::sync::Arc<std::sync::Mutex<ImageCache>>>(cache_id)
                .clone()
        });
        let mut cache = cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Textures of images no longer drawn (closed Modules) are released.
        cache
            .entries
            .retain(|_, entry| pass.saturating_sub(entry.last_pass) < IMAGE_IDLE_PASSES);
        // Repeat wraps the ring horizontally; linear filtering smooths both axes.
        let options = TextureOptions::LINEAR_REPEAT;
        let entry = match cache.entries.get_mut(&image.id()) {
            Some(entry) if entry.revision == image.revision() => {
                for &x in image.changed_columns() {
                    let column = ColorImage::new(
                        [1, height],
                        image
                            .column(x)
                            .iter()
                            .map(|&color| to_egui_color(color))
                            .collect(),
                    );
                    entry.texture.set_partial([x, 0], column, options);
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
                cache.entries.get_mut(&image.id()).expect("just inserted")
            }
        };
        entry.last_pass = pass;
        let texture = entry.texture.id();
        image.mark_uploaded();
        // Sample texel centers at the edges: the left edge shows `first_column`, the
        // right edge the column before it, the top and bottom rows unblended.
        let (w, h) = (width as f32, height as f32);
        let u0 = (first_column % width) as f32 / w + 0.5 / w;
        let uv = UvRect::from_min_max(pos2(u0, 0.5 / h), pos2(u0 + 1.0 - 1.0 / w, 1.0 - 0.5 / h));
        let mut mesh = Mesh::with_texture(texture);
        mesh.add_rect_with_uv(to_egui_rect(rect), uv, Color32::WHITE);
        self.painter
            .with_clip_rect(to_egui_rect(clip))
            .add(Shape::mesh(mesh));
    }
}

/// Passes an uploaded image may go undrawn before its texture is released.
const IMAGE_IDLE_PASSES: u64 = 600;

/// Textures of the `Image`s drawn in one egui context, by image id.
#[derive(Default)]
struct ImageCache {
    entries: std::collections::HashMap<u64, CachedImage>,
}

struct CachedImage {
    texture: eframe::egui::TextureHandle,
    revision: u64,
    last_pass: u64,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_color_serializes_to_the_short_hex_form_and_round_trips() {
        let color = Color::rgb(0x12, 0xab, 0xff);

        assert_eq!(color.to_hex(), "#12abff");
        assert_eq!(Color::from_hex("#12abff"), Some(color));
    }

    #[test]
    fn translucent_color_serializes_with_alpha_and_round_trips() {
        let color = Color::rgba(0x01, 0x02, 0x03, 0x04);

        assert_eq!(color.to_hex(), "#01020304");
        assert_eq!(Color::from_hex("#01020304"), Some(color));
    }

    #[test]
    fn fully_transparent_color_keeps_its_alpha() {
        let color = Color::rgba(0xff, 0x00, 0x80, 0);

        assert_eq!(color.to_hex(), "#ff008000");
        assert_eq!(Color::from_hex("#ff008000"), Some(color));
    }

    #[test]
    fn explicit_full_alpha_reads_as_opaque_and_is_written_in_the_short_form() {
        let color = Color::from_hex("#00ff00ff").unwrap();

        assert_eq!(color, Color::rgb(0, 255, 0));
        assert_eq!(color.to_hex(), "#00ff00");
    }

    #[test]
    fn hex_digits_are_read_case_insensitively_and_written_in_lower_case() {
        let color = Color::from_hex("#ABCDEF").unwrap();

        assert_eq!(color, Color::rgb(0xab, 0xcd, 0xef));
        assert_eq!(color.to_hex(), "#abcdef");
    }

    #[test]
    fn malformed_hex_colors_are_rejected() {
        for text in [
            "",
            "#",
            "ff0000",
            "ff000080",
            "#fff",
            "#ff00",
            "#ff000",
            "#ff00000",
            "#ff0000000",
            "##ff0000",
            "#gg0000",
            "#ff00zz",
            "#ff0000 ",
            " #ff0000",
            "#ff00000g",
        ] {
            assert_eq!(Color::from_hex(text), None, "{text:?}");
        }
    }

    #[test]
    fn non_ascii_hex_colors_are_rejected_without_panicking() {
        // Three two-byte characters make six bytes: the length check alone passes.
        assert_eq!(Color::from_hex("#\u{e9}\u{e9}\u{e9}"), None);
        assert_eq!(Color::from_hex("#ff00\u{e9}"), None);
        assert_eq!(Color::from_hex("#\u{e9}\u{e9}\u{e9}\u{e9}"), None);
    }

    #[test]
    fn color_is_saved_as_a_hex_string_and_loaded_back() {
        let opaque = Color::rgb(0x10, 0x20, 0x30);
        let translucent = Color::rgba(0x10, 0x20, 0x30, 0x40);

        assert_eq!(serde_json::to_string(&opaque).unwrap(), "\"#102030\"");
        assert_eq!(
            serde_json::to_string(&translucent).unwrap(),
            "\"#10203040\""
        );
        assert_eq!(
            serde_json::from_str::<Color>("\"#102030\"").unwrap(),
            opaque
        );
        assert_eq!(
            serde_json::from_str::<Color>("\"#10203040\"").unwrap(),
            translucent
        );
    }

    #[test]
    fn loading_an_invalid_color_string_is_an_error() {
        assert!(serde_json::from_str::<Color>("\"red\"").is_err());
        assert!(serde_json::from_str::<Color>("\"102030\"").is_err());
        assert!(serde_json::from_str::<Color>("\"#12345\"").is_err());
        assert!(serde_json::from_str::<Color>("1056816").is_err());
    }
}
