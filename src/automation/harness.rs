use std::{collections::HashMap, time::Duration};

use eframe::egui::{self, Event, PointerButton, Pos2};

use crate::app::{App, ControlKey, ControlTarget};

pub(super) struct Harness {
    pub app: App,
    pub context: egui::Context,
    pub width: f32,
    pub height: f32,
    pub ppp: f32,
    pub display_time: Duration,
    pub frame: u64,
    targets: HashMap<ControlKey, ControlTarget>,
}

impl Harness {
    pub fn new(app: App) -> Self {
        let context = egui::Context::default();
        context.set_visuals(egui::Visuals::dark());
        Self {
            app,
            context,
            width: 900.0,
            height: 420.0,
            ppp: 1.0,
            display_time: Duration::from_secs(1),
            frame: 0,
            targets: HashMap::new(),
        }
    }

    pub fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
        self.frame += 1;
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(self.width, self.height),
            )),
            time: Some(self.display_time.as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            focused: true,
            events,
            ..Default::default()
        };
        if let Some(root) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
            root.native_pixels_per_point = Some(self.ppp);
        }
        let output = self
            .context
            .run_ui(input, |ui| self.app.ui_body(ui, self.display_time));
        self.targets = self.app.control_targets().clone();
        output
    }

    pub fn control_state(&self, key: &ControlKey) -> Option<bool> {
        self.targets.get(key).map(|target| target.enabled)
    }

    pub fn locate(&self, key: &ControlKey) -> Result<ControlTarget, String> {
        let target = self
            .targets
            .get(key)
            .ok_or_else(|| format!("Missing control: {key:?}"))?;
        if !target.enabled {
            return Err(format!("Disabled control: {key:?}"));
        }
        Ok(*target)
    }

    pub fn click(&mut self, key: ControlKey) -> Result<(), String> {
        let target = self.locate(&key)?;
        self.click_target(key, target)
    }

    pub fn click_target(&mut self, key: ControlKey, target: ControlTarget) -> Result<(), String> {
        let current = self.locate(&key)?;
        if current.frame_token != target.frame_token || current.pass_token != target.pass_token {
            return Err(format!("Stale control: {key:?}"));
        }
        // egui hit-tests against the previous pointer position. Move without
        // pressing, then select a point actually hovered by this Response.
        let rect = target.rect;
        let size = rect.size();
        let points = [
            rect.center(),
            rect.min + size * 0.25,
            egui::pos2(rect.max.x - size.x * 0.25, rect.min.y + size.y * 0.25),
            egui::pos2(rect.min.x + size.x * 0.25, rect.max.y - size.y * 0.25),
            rect.max - size * 0.25,
        ];
        let mut hit = None;
        for point in points {
            self.frame(vec![Event::PointerMoved(point)]);
            let hover = self.locate(&key)?;
            if hover.rect.contains(point) && hover.hovered {
                hit = Some(point);
                break;
            }
        }
        let pos = hit.ok_or_else(|| {
            format!("No hit-testable point in control: {key:?}, target={target:?}")
        })?;
        self.frame(vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let pressed = self.locate(&key)?;
        if !pressed.rect.contains(pos) {
            self.frame(vec![Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }]);
            return Err(format!(
                "Control moved between press and release: {key:?}, before={:?}, pressed={:?}",
                target.rect, pressed.rect
            ));
        }
        self.frame(vec![
            Event::PointerMoved(pos),
            Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        Ok(())
    }

    pub fn dropdown(&mut self, opener: ControlKey, item: ControlKey) -> Result<(), String> {
        self.click(opener)?;
        self.frame(vec![]);
        self.click(item)?;
        self.frame(vec![]);
        Ok(())
    }

    pub fn key(&mut self, key: egui::Key) {
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        self.frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
    }
}
