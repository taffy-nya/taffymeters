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
        StyleOption, number_setting,
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
        ]
    }

    fn set_view_setting(&mut self, key: &str, value: &SettingValue) -> Result<(), ModuleError> {
        match key {
            "channel" => self.set_channel(channel_value(key, value)?),
            "scale" => {
                self.y_scale = number_setting(key, value, OSCILLOSCOPE_SCALE)?;
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
        self.style.draw(
            OscilloscopeStyleContext {
                descriptor: self.reader.descriptor(),
                frame: &snapshot,
                channel: self.channel,
                rect: frame.rect,
                y_scale: self.y_scale,
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
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use crate::{
        audio::{
            AudioDescriptor, ChannelLayout, Generation, SourceId, SourceReader, SourceTelemetry,
            create_block_transport,
        },
        dataflow::Dataflow,
        render::{Circle, ColoredRect, Rect, RenderTarget, Stroke, TextStyle},
    };

    #[derive(Default)]
    struct RecordingTarget {
        polylines: usize,
    }

    impl RenderTarget for RecordingTarget {
        fn polyline(&mut self, _clip: Rect, points: &[Point], _stroke: Stroke) {
            self.polylines += usize::from(!points.is_empty());
        }

        fn fill_rects(&mut self, _clip: Rect, _rects: &[ColoredRect]) {}

        fn circles(&mut self, _clip: Rect, _circles: &[Circle]) {}

        fn text(&mut self, _clip: Rect, _position: Point, _text: &str, _style: TextStyle) {}
    }

    #[test]
    fn reselecting_oscilloscope_channel_keeps_persistence_traces() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, endpoint) =
            create_block_transport(2, 8, 1_600, Arc::clone(&telemetry)).unwrap();
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), endpoint, Arc::clone(&telemetry));
        capture.set_worker(dataflow.worker_thread());
        let services = ModuleServices {
            source: SourceReader::new(descriptor, telemetry),
            dataflow: handle,
        };
        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = OscilloscopeFactory
            .create(&services, &registry, None)
            .unwrap();
        let palette = crate::theme::Palette::default();
        module
            .set_style(PERSISTENCE_OSCILLOSCOPE_STYLE_ID, &registry)
            .unwrap();
        let samples: Vec<f32> = (0..1_600).flat_map(|_| [0.2, -0.1]).collect();
        capture.write(&samples);

        let rect = Rect {
            min: Point { x: 0.0, y: 0.0 },
            max: Point { x: 400.0, y: 200.0 },
        };
        let mut target = RecordingTarget::default();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            target.polylines = 0;
            module.draw(ModuleFrame {
                rect,
                monotonic_time: Duration::ZERO,
                pointer: None,
                scroll_delta_y: 0.0,
                zoom_delta: 1.0,
                palette: &palette,
                renderer: &mut target,
            });
            if target.polylines == 2 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Persistence never received audio"
            );
            std::thread::yield_now();
        }

        module.set_channel(ChannelSelection::MonoAverage).unwrap();
        target.polylines = 0;
        module.draw(ModuleFrame {
            rect,
            monotonic_time: Duration::from_millis(50),
            pointer: None,
            scroll_delta_y: 0.0,
            zoom_delta: 1.0,
            palette: &palette,
            renderer: &mut target,
        });
        assert_eq!(target.polylines, 2, "reselecting cleared a live trace");
    }
}
