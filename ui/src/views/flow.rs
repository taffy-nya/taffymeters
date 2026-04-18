use eframe::egui;

#[derive(PartialEq, Clone, Copy)]
pub enum Direction { LtoR, RtoL, UtoD, DtoU }

impl Direction {
    pub fn history_pixels(self, rect: egui::Rect) -> f32 {
        match self {
            Direction::LtoR | Direction::RtoL => rect.width(),
            Direction::UtoD | Direction::DtoU => rect.height(),
        }
    }

    pub fn cross_pixels(self, rect: egui::Rect) -> f32 {
        match self {
            Direction::LtoR | Direction::RtoL => rect.height(),
            Direction::UtoD | Direction::DtoU => rect.width(),
        }
    }

    pub fn texture_size(self, history: usize, cross: usize) -> [usize; 2] {
        match self {
            Direction::LtoR | Direction::RtoL => [history, cross],
            Direction::UtoD | Direction::DtoU => [cross, history],
        }
    }

    pub fn advance(self, head: usize, len: usize) -> usize {
        match self {
            Direction::LtoR | Direction::UtoD => (head + len - 1) % len,
            Direction::RtoL | Direction::DtoU => (head + 1) % len,
        }
    }

    pub fn start(self, head: usize, len: usize) -> usize {
        match self {
            Direction::LtoR | Direction::UtoD => head,
            Direction::RtoL | Direction::DtoU => (head + 1) % len,
        }
    }

    pub fn history_pos(self, index: usize, len: usize) -> usize {
        match self {
            Direction::LtoR | Direction::UtoD => index,
            Direction::RtoL | Direction::DtoU => (len - index) % len,
        }
    }

    pub fn patch_pos(self, head: usize) -> [usize; 2] {
        match self {
            Direction::LtoR | Direction::RtoL => [head, 0],
            Direction::UtoD | Direction::DtoU => [0, head],
        }
    }

    pub fn uv(self, head: usize, history: usize, cross: usize) -> egui::Rect {
        let start = self.start(head, history) as f32 / history as f32;
        match self {
            Direction::LtoR | Direction::RtoL => {
                let eps_x = 0.5 / history as f32;
                let eps_y = 0.5 / cross as f32;
                egui::Rect::from_min_max(
                    egui::pos2(start + eps_x, eps_y),
                    egui::pos2(start + 1.0 - eps_x, 1.0 - eps_y),
                )
            }
            Direction::UtoD | Direction::DtoU => {
                let eps_x = 0.5 / cross as f32;
                let eps_y = 0.5 / history as f32;
                egui::Rect::from_min_max(
                    egui::pos2(eps_x, start + eps_y),
                    egui::pos2(1.0 - eps_x, start + 1.0 - eps_y),
                )
            }
        }
    }
}

pub struct FlowTexture {
    texture: Option<egui::TextureHandle>,
    head: usize,
}

impl FlowTexture {
    pub fn new() -> Self {
        Self { texture: None, head: 0 }
    }

    pub fn reset(&mut self) {
        self.texture = None;
        self.head = 0;
    }

    pub fn matches_size(&self, size: [usize; 2]) -> bool {
        self.texture.as_ref().is_some_and(|tex| tex.size() == size)
    }

    pub fn ensure(
        &mut self,
        ui: &egui::Ui,
        name: &str,
        image: egui::ColorImage,
        options: egui::TextureOptions,
    ) {
        match &mut self.texture {
            Some(tex) if tex.size() == image.size => {}
            Some(tex) => {
                tex.set(image, options);
                self.head = 0;
            }
            None => {
                self.texture = Some(ui.load_texture(name, image, options));
                self.head = 0;
            }
        }
    }

    pub fn push_patch(
        &mut self,
        direction: Direction,
        history: usize,
        patch: egui::ColorImage,
        options: egui::TextureOptions,
    ) {
        let Some(tex) = &mut self.texture else { return; };
        self.head = direction.advance(self.head, history);
        tex.set_partial(direction.patch_pos(self.head), patch, options);
    }

    pub fn paint(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        direction: Direction,
        history: usize,
        cross: usize,
    ) {
        if let Some(tex) = &self.texture {
            painter.image(tex.id(), rect, direction.uv(self.head, history, cross), egui::Color32::WHITE);
        }
    }
}
