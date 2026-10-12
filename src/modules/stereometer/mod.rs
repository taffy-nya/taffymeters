use crate::{
    dataflow::{DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        number_setting,
    },
    render::{Circle, Point, Stroke, TextStyle},
    settings::{SettingValue, ViewSetting},
    snapshot::DataReader,
    theme::ColorRole,
};

use crate::modules::pcm_view::{scale_setting, subscribe_pcm};

const STEREOMETER_SCALE: (f32, f32) = (0.5, 10.0);

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
        vec![scale_setting(self.scale, STEREOMETER_SCALE)]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        if key != "scale" {
            return Err(ModuleError::UnknownSetting(key.to_owned()));
        }
        self.scale = number_setting(key, value, STEREOMETER_SCALE)?;
        Ok(())
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && frame.scroll_delta_y.abs() > 0.01
        {
            self.scale = (self.scale * (frame.scroll_delta_y * 0.002).exp())
                .clamp(STEREOMETER_SCALE.0, STEREOMETER_SCALE.1);
        }
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
        let radius = frame.rect.width().min(frame.rect.height()) * 0.45;
        let guide = Stroke {
            width: 1.0,
            color: frame.palette.color(ColorRole::Guide),
        };
        let dot = frame.palette.color(ColorRole::Primary);
        let dot = dot.with_alpha((f32::from(dot.alpha) * 180.0 / 255.0) as u8);
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
        self.circles.clear();
        if let Some(chunk) = snapshot.pcm() {
            let step = chunk.frame_count.div_ceil(512).max(1);
            for samples in chunk.interleaved.chunks_exact(channels).step_by(step) {
                let left = samples[0];
                let right = samples[1];
                self.circles.push(Circle {
                    center: Point {
                        x: center.x + (left - right) * 0.707 * radius * self.scale,
                        y: center.y - (left + right) * 0.707 * radius * self.scale,
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
