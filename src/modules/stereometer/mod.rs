use crate::{
    dataflow::{DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        REFERENCE_HEIGHT, flag_setting, number_setting, overlay_setting, reference_gain,
    },
    render::{Circle, Point, Stroke, TextStyle},
    settings::{SettingValue, ViewSetting},
    snapshot::DataReader,
    theme::ColorRole,
};

use crate::modules::pcm_view::{SCALE_RANGE, scale_setting, subscribe_pcm, wheel_scale};

pub const STEREOMETER_MODULE_ID: &str = "taffymeters.stereometer";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_module(StereometerFactory)
}

struct StereometerFactory;

impl ModuleFactory for StereometerFactory {
    fn type_id(&self) -> &'static str {
        STEREOMETER_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Stereometer"
    }

    fn create(
        &self,
        services: &ModuleServices,
        _registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (subscription, output) = subscribe_pcm(services, Keep::Latest, data)?;
        Ok(Box::new(StereometerModule {
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            scale: 1.0,
            show_guides: false,
            circles: Vec::with_capacity(512),
        }))
    }
}

struct StereometerModule {
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    scale: f32,
    show_guides: bool,
    circles: Vec<Circle>,
}

impl Module for StereometerModule {
    fn set_visible(&mut self, visible: bool) {
        let _ = self.controller.set_visible(visible);
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary, ColorRole::Guide, ColorRole::Muted]
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            scale_setting(self.scale),
            overlay_setting("show_guides", "Axes", self.show_guides),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "scale" => self.scale = number_setting(key, value, SCALE_RANGE)?,
            "show_guides" => self.show_guides = flag_setting(key, value)?,
            _ => return Err(ModuleError::UnknownSetting(key.to_owned())),
        }
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        self.scale = wheel_scale(self.scale, &frame);
        let Some(snapshot) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
        };
        let channels = self.reader.descriptor().channel_count as usize;
        if channels < 2 {
            frame.renderer.text(
                frame.rect,
                frame.rect.center(),
                "Stereo input required",
                TextStyle {
                    size: frame.palette.text_size + 2.0,
                    color: frame.palette.color(ColorRole::Muted),
                },
            );
            return ModuleResponse::default();
        }
        let center = frame.rect.center();
        let radius = frame.rect.width().min(frame.rect.height()) * 0.5 * REFERENCE_HEIGHT;
        if self.show_guides {
            let guide = Stroke {
                width: 1.0,
                color: frame.palette.color(ColorRole::Guide),
            };
            frame.renderer.polyline(
                frame.rect,
                &[
                    Point {
                        x: center.x,
                        y: center.y - radius,
                    },
                    Point {
                        x: center.x,
                        y: center.y + radius,
                    },
                ],
                guide,
            );
            frame.renderer.polyline(
                frame.rect,
                &[
                    Point {
                        x: center.x - radius,
                        y: center.y,
                    },
                    Point {
                        x: center.x + radius,
                        y: center.y,
                    },
                ],
                guide,
            );
        }
        let dot = frame.palette.color(ColorRole::Primary);
        let dot = dot.with_alpha((f32::from(dot.alpha) * 180.0 / 255.0) as u8);
        self.circles.clear();
        // Mid up, side across, each the mean of the two channels: mono-like content
        // peaking at the reference reaches the guide radius. Scale is relative.
        let half = frame.rect.width().min(frame.rect.height()) * 0.5;
        let gain = 0.5 * half * self.scale * reference_gain(frame.reference_dbfs);
        if let Some(chunk) = snapshot.pcm() {
            let step = chunk.frame_count.div_ceil(512).max(1);
            for samples in chunk.interleaved.chunks_exact(channels).step_by(step) {
                let left = samples[0];
                let right = samples[1];
                self.circles.push(Circle {
                    center: Point {
                        x: center.x + (left - right) * gain,
                        y: center.y - (left + right) * gain,
                    },
                    radius: 1.4,
                    color: dot,
                });
            }
        }
        frame.renderer.circles(frame.rect, &self.circles);
        ModuleResponse::default()
    }
    fn clear_visual_state(&mut self) {
        self.circles.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::modules::test_support::{
        BLOCK_FRAMES, PcmFixture, RecordingTarget, TEST_RECT, draw, repeat_frame,
    };

    fn stereometer(fixture: &PcmFixture) -> Box<dyn Module> {
        StereometerFactory
            .create(&fixture.services, &Registry::default(), None)
            .unwrap()
    }

    #[test]
    fn mono_input_shows_a_hint_and_draws_no_points() {
        let mut fixture = PcmFixture::new(1);
        let mut module = stereometer(&fixture);
        fixture.feed(&repeat_frame(&[0.5], BLOCK_FRAMES));

        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);

        assert_eq!(target.texts.len(), 1, "one hint explaining the requirement");
        assert!(!target.texts[0].text.is_empty());
        assert!(
            TEST_RECT.contains(target.texts[0].position),
            "the hint must be inside the Module's area"
        );
        assert!(
            target.circles.is_empty(),
            "mono has no stereo field to plot"
        );
        assert!(
            target.polylines.is_empty(),
            "no stereo guides without a field"
        );
    }

    #[test]
    fn stereo_input_plots_points_and_no_hint() {
        let mut fixture = PcmFixture::new(2);
        let mut module = stereometer(&fixture);
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));

        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);

        assert!(target.texts.is_empty());
        assert!(!target.circles.is_empty(), "stereo input plots points");
    }

    #[test]
    fn identical_channels_plot_on_the_vertical_axis_and_opposite_ones_on_the_horizontal() {
        let mut fixture = PcmFixture::new(2);
        let mut module = stereometer(&fixture);
        let center = TEST_RECT.center();
        let mut target = RecordingTarget::default();

        // L = R: pure mid signal, straight up from the center for positive samples.
        fixture.feed(&repeat_frame(&[0.5, 0.5], BLOCK_FRAMES));
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert!(!target.circles.is_empty());
        for circle in &target.circles {
            assert_eq!(
                circle.center.x, center.x,
                "L = R must stay on the vertical axis"
            );
            assert!(
                circle.center.y < center.y,
                "positive mid plots above center"
            );
        }

        // L = -R: pure side signal, to the right of the center when L is positive.
        fixture.feed(&repeat_frame(&[0.5, -0.5], BLOCK_FRAMES));
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        assert!(!target.circles.is_empty());
        for circle in &target.circles {
            assert_eq!(
                circle.center.y, center.y,
                "L = -R must stay on the horizontal axis"
            );
            assert!(
                circle.center.x > center.x,
                "positive left plots right of center"
            );
        }
    }
}
