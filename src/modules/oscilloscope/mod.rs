pub mod style;
mod styles;

use self::style::{OscilloscopeStyle, OscilloscopeStyleContext};
pub use self::styles::LINE_OSCILLOSCOPE_STYLE_ID;

use crate::{
    audio::SourceReader,
    dataflow::{ChannelSelection, DataController, DataSpec, DataSubscription, Keep, OutputId},
    extensions::{Registry, RegistryError},
    module::{
        Module, ModuleError, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices,
        StyleOption, flag_setting, number_setting, overlay_setting, reference_gain,
        styled_color_roles,
    },
    render::{Point, Stroke},
    settings::{SettingValue, ViewSetting},
    snapshot::DataReader,
    theme::ColorRole,
};

use crate::modules::pcm_view::{
    SCALE_RANGE, channel_setting, channel_value, scale_setting, subscribe_pcm, wheel_scale,
};

pub const OSCILLOSCOPE_MODULE_ID: &str = "taffymeters.oscilloscope";

const OSCILLOSCOPE_STYLES: &[StyleOption] = &[StyleOption {
    id: LINE_OSCILLOSCOPE_STYLE_ID,
    label: "Line",
}];

/// The center line; the trace is the style's.
const OWN_COLOR_ROLES: &[ColorRole] = &[ColorRole::Guide];
/// Every role an Oscilloscope can draw with across its styles.
const TYPE_COLOR_ROLES: &[ColorRole] = &[ColorRole::Primary, ColorRole::Guide];

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
        let style = registry.create_oscilloscope_style(LINE_OSCILLOSCOPE_STYLE_ID)?;
        Ok(Box::new(OscilloscopeModule {
            source: services.source.clone(),
            controller: subscription.controller(),
            reader: subscription.reader(),
            _subscription: subscription,
            output,
            channel: ChannelSelection::MonoAverage,
            y_scale: 1.0,
            show_guide: false,
            color_roles: styled_color_roles(style.color_roles(), OWN_COLOR_ROLES),
            style,
            style_id: LINE_OSCILLOSCOPE_STYLE_ID,
        }))
    }

    fn color_roles(&self) -> &'static [ColorRole] {
        TYPE_COLOR_ROLES
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
    /// The style's roles and the center line's.
    color_roles: Vec<ColorRole>,
}

impl Module for OscilloscopeModule {
    fn channel(&self) -> Option<ChannelSelection> {
        Some(self.channel)
    }

    fn set_channel(&mut self, channel: ChannelSelection) -> Result<(), ModuleError> {
        channel.resolve(self.source.descriptor())?;
        self.channel = channel;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) {
        let _ = self.controller.set_visible(visible);
    }

    fn data(&self) -> Option<&DataController> {
        Some(&self.controller)
    }

    fn color_roles(&self) -> &[ColorRole] {
        &self.color_roles
    }

    fn view_settings(&self) -> Vec<ViewSetting> {
        vec![
            channel_setting(self.channel),
            scale_setting(self.y_scale),
            overlay_setting("show_guide", "Center line", self.show_guide),
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "channel" => self.set_channel(channel_value(key, value)?),
            "scale" => {
                self.y_scale = number_setting(key, value, SCALE_RANGE)?;
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
        self.y_scale = wheel_scale(self.y_scale, &frame);
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
        self.color_roles = styled_color_roles(self.style.color_roles(), OWN_COLOR_ROLES);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::modules::test_support::{
        BLOCK_FRAMES, PcmFixture, RecordingTarget, TEST_RECT, draw, repeat_frame,
    };

    fn scope(fixture: &PcmFixture) -> Box<dyn Module> {
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        OscilloscopeFactory
            .create(&fixture.services, &registry, None)
            .unwrap()
    }

    /// Height of `level` in the test frames, whose full-scale reference draws 1.0
    /// at `REFERENCE_HEIGHT` of the half height.
    fn level_y(level: f32) -> f32 {
        TEST_RECT.center().y - level * crate::module::REFERENCE_HEIGHT * TEST_RECT.height() * 0.5
    }

    /// The single trace drawn sits at `level` (the center line is hidden by default).
    fn assert_trace_at(target: &RecordingTarget, level: f32) {
        assert_eq!(target.non_empty_polylines(), 1);
        let expected_y = level_y(level);
        let trace = target.polylines.last().unwrap();
        assert!(
            trace
                .points
                .iter()
                .all(|point| (point.y - expected_y).abs() < 0.001),
            "trace is not at level {level}"
        );
    }

    #[test]
    fn switching_channel_traces_the_new_channel() {
        let mut fixture = PcmFixture::new(2);
        let mut module = scope(&fixture);
        fixture.feed(&repeat_frame(&[0.2, -0.1], BLOCK_FRAMES));

        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        // Mono mix of 0.2 and -0.1.
        assert_trace_at(&target, 0.05);

        module.set_channel(ChannelSelection::Left).unwrap();
        assert_eq!(module.channel(), Some(ChannelSelection::Left));
        target.clear();
        draw(module.as_mut(), &mut target, Duration::from_millis(50));
        assert_trace_at(&target, 0.2);
    }

    #[test]
    fn rejected_channel_change_keeps_the_channel() {
        let mut fixture = PcmFixture::new(2);
        let mut module = scope(&fixture);
        fixture.feed(&repeat_frame(&[0.2, -0.1], BLOCK_FRAMES));

        assert!(matches!(
            module.set_channel(ChannelSelection::Discrete(2)),
            Err(ModuleError::InvalidChannel(_))
        ));
        assert_eq!(module.channel(), Some(ChannelSelection::MonoAverage));
        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert_trace_at(&target, 0.05);
    }

    /// The trace of one 1600-frame mono chunk, silent except for `spikes`;
    /// `TEST_RECT` is 400 wide, so four frames share a column.
    fn trace_of(spikes: &[(usize, f32)]) -> Vec<Point> {
        let mut fixture = PcmFixture::new(1);
        let mut module = scope(&fixture);
        let mut pcm = vec![0.0; BLOCK_FRAMES];
        for &(frame, level) in spikes {
            pcm[frame] = level;
        }
        fixture.feed(&pcm);
        let mut target = RecordingTarget::default();
        draw(module.as_mut(), &mut target, Duration::ZERO);
        assert_eq!(target.non_empty_polylines(), 1);
        target.polylines.pop().unwrap().points
    }

    fn position_of(points: &[Point], level: f32) -> usize {
        points
            .iter()
            .position(|point| (point.y - level_y(level)).abs() < 0.001)
            .unwrap_or_else(|| panic!("no point at level {level}"))
    }

    #[test]
    fn a_spike_narrower_than_a_column_reaches_its_height() {
        let points = trace_of(&[(1, 0.5), (803, -0.3)]);
        position_of(&points, 0.5);
        position_of(&points, -0.3);
    }

    #[test]
    fn column_extremes_keep_their_time_order_across_the_width() {
        // A maximum before the minimum in one column, the reverse in another.
        let points = trace_of(&[(41, 0.6), (42, -0.6), (81, -0.4), (82, 0.4)]);
        assert!(position_of(&points, 0.6) < position_of(&points, -0.6));
        assert!(position_of(&points, -0.4) < position_of(&points, 0.4));
        assert!(points.windows(2).all(|pair| pair[0].x < pair[1].x));
        assert_eq!(points.first().unwrap().x, TEST_RECT.min.x);
        assert!((points.last().unwrap().x - TEST_RECT.max.x).abs() < 0.001);
        // At most two points per column besides the two ends.
        assert!(points.len() <= 2 * TEST_RECT.width() as usize + 2);
    }
}
