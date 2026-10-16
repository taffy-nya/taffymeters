pub mod style;
mod styles;

use self::style::{OscilloscopeStyle, OscilloscopeStyleContext};
pub use self::styles::{LINE_OSCILLOSCOPE_STYLE_ID, PERSISTENCE_OSCILLOSCOPE_STYLE_ID};

use crate::{
    audio::SourceReader,
    dataflow::{ChannelSelection, DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        StyleOption, flag_setting, number_setting, overlay_setting, reference_gain,
    },
    render::{Point, Stroke},
    settings::{SettingValue, ViewSetting},
    snapshot::DataReader,
    theme::ColorRole,
};

use crate::modules::pcm_view::{channel_setting, channel_value, scale_setting, subscribe_pcm};

pub const OSCILLOSCOPE_MODULE_ID: &str = "taffymeters.oscilloscope";

const OSCILLOSCOPE_SCALE: (f32, f32) = (0.2, 10.0);

const OSCILLOSCOPE_STYLES: &[StyleOption] = &[
    StyleOption {
        id: LINE_OSCILLOSCOPE_STYLE_ID,
        label: "Line",
    },
    StyleOption {
        id: PERSISTENCE_OSCILLOSCOPE_STYLE_ID,
        label: "Persistence",
    },
];

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    styles::register(registry)?;
    registry.register_module(OscilloscopeFactory)
}

struct OscilloscopeFactory;

impl ModuleFactory for OscilloscopeFactory {
    fn type_id(&self) -> &'static str {
        OSCILLOSCOPE_MODULE_ID
    }

    fn display_name(&self) -> &'static str {
        "Oscilloscope"
    }

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
        data: Option<&DataSpec>,
    ) -> Result<Box<dyn Module>, RegistryError> {
        let (subscription, output) = subscribe_pcm(services, Keep::Latest, data)?;
        Ok(Box::new(OscilloscopeModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            channel: ChannelSelection::MonoAverage,
            y_scale: 1.0,
            show_guide: false,
            style: registry.create_oscilloscope_style(LINE_OSCILLOSCOPE_STYLE_ID)?,
            style_id: LINE_OSCILLOSCOPE_STYLE_ID,
        }))
    }
}

struct OscilloscopeModule {
    source: SourceReader,
    controller: DataController,
    reader: DataReader,
    _subscription: DataSubscription,
    output: OutputId,
    channel: ChannelSelection,
    y_scale: f32,
    show_guide: bool,
    style: Box<dyn OscilloscopeStyle>,
    style_id: &'static str,
}

impl Module for OscilloscopeModule {
    fn channel(&self) -> Option<ChannelSelection> {
        Some(self.channel)
    }

    fn set_channel(&mut self, channel: ChannelSelection) -> Result<(), ModuleError> {
        channel.resolve(self.source.descriptor())?;
        if self.channel == channel {
            return Ok(());
        }
        self.channel = channel;
        self.style.clear();
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) {
        let _ = self.controller.set_visible(visible);
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        &[ColorRole::Primary, ColorRole::Guide]
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            channel_setting(self.channel),
            scale_setting(self.y_scale, OSCILLOSCOPE_SCALE),
            overlay_setting("show_guide", "Center line", self.show_guide),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "channel" => self.set_channel(channel_value(key, value)?),
            "scale" => {
                self.y_scale = number_setting(key, value, OSCILLOSCOPE_SCALE)?;
                Ok(())
            }
            "show_guide" => {
                self.show_guide = flag_setting(key, value)?;
                Ok(())
            }
            _ => Err(ModuleError::UnknownSetting(key.to_owned())),
        }
    }

    fn draw(&mut self, frame: ModuleFrame<'_>) -> ModuleResponse {
        if frame
            .pointer
            .is_some_and(|point| frame.rect.contains(point))
            && frame.scroll_delta_y.abs() > 0.01
        {
            self.y_scale = (self.y_scale * (frame.scroll_delta_y * 0.002).exp())
                .clamp(OSCILLOSCOPE_SCALE.0, OSCILLOSCOPE_SCALE.1);
        }
        let Some(snapshot) = self.reader.latest(self.output) else {
            return ModuleResponse::default();
        };
        if self.show_guide {
            let center_y = frame.rect.center().y;
            frame.renderer.polyline(
                frame.rect,
                &[
                    Point {
                        x: frame.rect.min.x,
                        y: center_y,
                    },
                    Point {
                        x: frame.rect.max.x,
                        y: center_y,
                    },
                ],
                Stroke {
                    width: 1.0,
                    color: frame.palette.color(ColorRole::Guide),
                },
            );
        }
        self.style.draw(
            OscilloscopeStyleContext {
                descriptor: self.reader.descriptor(),
                frame: &snapshot,
                channel: self.channel,
                rect: frame.rect,
                // The Scale setting is relative to the workspace reference level.
                y_scale: self.y_scale * reference_gain(frame.reference_dbfs),
                monotonic_time: frame.monotonic_time,
                palette: frame.palette,
            },
            frame.renderer,
        )
    }

    fn style_options(&self) -> &'static [StyleOption] {
        OSCILLOSCOPE_STYLES
    }

    fn active_style(&self) -> Option<&'static str> {
        Some(self.style_id)
    }

    fn set_style(&mut self, style_id: &str, registry: &Registry) -> Result<(), RegistryError> {
        let option = OSCILLOSCOPE_STYLES
            .iter()
            .find(|option| option.id == style_id)
            .ok_or_else(|| RegistryError::UnknownStyle(style_id.to_owned()))?;
        self.style = registry.create_oscilloscope_style(option.id)?;
        self.style_id = option.id;
        Ok(())
    }

    fn clear_visual_state(&mut self) {
        self.style.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::modules::test_support::{
        BLOCK_FRAMES, PcmFixture, RecordingTarget, TEST_RECT, draw, repeat_frame,
    };

    fn persistence_scope(fixture: &PcmFixture) -> Box<dyn Module> {
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = OscilloscopeFactory
            .create(&fixture.services, &registry, None)
            .unwrap();
        module
            .set_style(PERSISTENCE_OSCILLOSCOPE_STYLE_ID, &registry)
            .unwrap();
        module
    }

    /// One persistence trace (the center line is hidden by default).
    const ONE_TRACE: usize = 1;

    /// Height of `level` in the test frames, whose full-scale reference draws 1.0
    /// at `REFERENCE_HEIGHT` of the half height.
    fn level_y(level: f32) -> f32 {
        TEST_RECT.center().y - level * crate::module::REFERENCE_HEIGHT * TEST_RECT.height() * 0.5
    }

    #[test]
    fn reselecting_oscilloscope_channel_keeps_persistence_traces() {
        let mut fixture = PcmFixture::new(2);
        let mut module = persistence_scope(&fixture);
        fixture.feed(&repeat_frame(&[0.2, -0.1], BLOCK_FRAMES));

        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert_eq!(target.non_empty_polylines(), ONE_TRACE);

        module.set_channel(ChannelSelection::MonoAverage).unwrap();
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        assert_eq!(
            target.non_empty_polylines(),
            ONE_TRACE,
            "reselecting cleared a live trace"
        );
    }

    #[test]
    fn switching_to_a_different_channel_clears_persistence_traces() {
        let mut fixture = PcmFixture::new(2);
        let mut module = persistence_scope(&fixture);
        fixture.feed(&repeat_frame(&[0.2, -0.1], BLOCK_FRAMES));

        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert_eq!(target.non_empty_polylines(), ONE_TRACE);

        module.set_channel(ChannelSelection::Left).unwrap();
        assert_eq!(module.channel(), Some(ChannelSelection::Left));
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        // The old channel's trace (mono mix 0.05) must be gone. Whether the same
        // result is re-traced from the new channel is not specified; if it is, it
        // must follow Left.
        let mono_y = level_y(0.05);
        let left_y = level_y(0.2);
        for trace in &target.polylines {
            assert!(
                trace
                    .points
                    .iter()
                    .all(|point| (point.y - left_y).abs() < 0.001),
                "a trace of the previous channel survived the switch"
            );
            assert!(
                trace
                    .points
                    .iter()
                    .all(|point| (point.y - mono_y).abs() > 0.001)
            );
        }

        // The next result is traced from the newly selected channel.
        fixture.feed(&repeat_frame(&[0.2, -0.1], BLOCK_FRAMES));
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(100));
        assert_eq!(target.non_empty_polylines(), ONE_TRACE);
        let trace = target.polylines.last().unwrap();
        // Left is 0.2 (mono mix would be 0.05).
        let expected_y = level_y(0.2);
        assert!(
            trace
                .points
                .iter()
                .all(|point| (point.y - expected_y).abs() < 0.001),
            "trace did not follow the Left channel"
        );
    }

    #[test]
    fn rejected_channel_change_keeps_channel_and_persistence_traces() {
        let mut fixture = PcmFixture::new(2);
        let mut module = persistence_scope(&fixture);
        fixture.feed(&repeat_frame(&[0.2, -0.1], BLOCK_FRAMES));

        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert_eq!(target.non_empty_polylines(), ONE_TRACE);

        assert!(matches!(
            module.set_channel(ChannelSelection::Discrete(2)),
            Err(ModuleError::InvalidChannel(_))
        ));
        assert_eq!(module.channel(), Some(ChannelSelection::MonoAverage));
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        assert_eq!(target.non_empty_polylines(), ONE_TRACE);
    }
}
