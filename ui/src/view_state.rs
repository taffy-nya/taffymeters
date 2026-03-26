pub enum ZoomAnchor {
    Center,
    Bottom,
}

pub struct ViewInteractionState {
    pub y_zoom: f32,
    pub anchor: ZoomAnchor,
    pub min_zoom: f32,
    pub max_zoom: f32,
}

impl ViewInteractionState {
    pub fn new(anchor: ZoomAnchor, min_zoom: f32, max_zoom: f32) -> Self {
        Self {
            y_zoom: 1.0,
            anchor,
            min_zoom,
            max_zoom,
        }
    }

    pub fn apply_scroll(&mut self, scroll_y: f32) {
        if scroll_y.abs() <= f32::EPSILON {
            return;
        }

        let factor = (1.0 + scroll_y * 0.001).clamp(0.8, 1.25);
        self.y_zoom = (self.y_zoom * factor).clamp(self.min_zoom, self.max_zoom);
    }
}

pub struct ViewStates {
    pub waveform: ViewInteractionState,
    pub spectrum: ViewInteractionState,
}

impl ViewStates {
    pub fn new() -> Self {
        Self {
            waveform: ViewInteractionState::new(ZoomAnchor::Center, 0.2, 8.0),
            spectrum: ViewInteractionState::new(ZoomAnchor::Bottom, 0.2, 12.0),
        }
    }
}