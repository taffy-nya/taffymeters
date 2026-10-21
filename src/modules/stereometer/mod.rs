use crate::{
    dataflow::{DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        REFERENCE_HEIGHT, draw_notice, flag_setting, number_setting, overlay_setting,
        reference_gain,
    },
    render::{Circle, Point, Stroke},
    settings::{SettingValue, ViewSetting},
    snapshot::DataReader,
    theme::ColorRole,
};

use crate::modules::pcm_view::{SCALE_KEY, SCALE_RANGE, scale_setting, subscribe_pcm, wheel_scale};

const SHOW_GUIDES_KEY: &str = "show_guides";

pub const STEREOMETER_MODULE_ID: &str = "taffymeters.stereometer";

const COLOR_ROLES: &[ColorRole] = &[ColorRole::Primary, ColorRole::Guide];

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

    fn color_roles(&self) -> &'static [ColorRole] {
        COLOR_ROLES
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

    fn color_roles(&self) -> &[ColorRole] {
        COLOR_ROLES
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            scale_setting(self.scale),
            overlay_setting(SHOW_GUIDES_KEY, "Axes", self.show_guides),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            SCALE_KEY => self.scale = number_setting(key, value, SCALE_RANGE)?,
            SHOW_GUIDES_KEY => self.show_guides = flag_setting(key, value)?,
            _ => return Err(ModuleError::UnknownSetting(key.to_owned())),
        }
        Ok(())
    }

    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        self.scale = wheel_scale(self.scale, &frame);
        let Some(snapshot) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
        };
        let channels = self.reader.descriptor().channel_count as usize;
        if channels < 2 {
            draw_notice(&mut frame, "Stereo input required");
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
        // Positive mid points up, positive side right; at unit scale a reference-level mono peak
        // reaches the guide because both channel combinations are averaged.
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
