use std::{fmt::Write, sync::Arc, time::Duration};

use crate::{
    audio::SourceState,
    module::{
        Module, ModuleFactory, ModuleFrame, ModuleResponse, ModuleServices, Registry,
        RegistryError, SpectrumCoordinates, SpectrumStyle, SpectrumStyleContext,
        SpectrumStyleFactory,
    },
    render::{Color, Point, RenderTarget, Stroke, TextStyle},
    snapshot::{SpectrumReader, SpectrumSnapshotSet},
};

pub const SPECTRUM_MODULE_ID: &str = "taffymeters.spectrum";
pub const DEFAULT_SPECTRUM_STYLE_ID: &str = "taffymeters.spectrum.line";

pub(super) fn register(registry: &mut Registry) -> Result<(), RegistryError> {
    registry.register_spectrum_style(LineSpectrumStyleFactory)?;
    registry.register_module(SpectrumModuleFactory)
}

struct SpectrumModuleFactory;

impl ModuleFactory for SpectrumModuleFactory {
    fn type_id(&self) -> &'static str {
        SPECTRUM_MODULE_ID
    }

    fn create(
        &self,
        services: &ModuleServices,
        registry: &Registry,
    ) -> Result<Box<dyn Module>, RegistryError> {
        Ok(Box::new(SpectrumModule {
            reader: services.spectrum.clone(),
            style: registry.create_spectrum_style(DEFAULT_SPECTRUM_STYLE_ID)?,
            coordinates: SpectrumCoordinates {
                min_frequency_hz: 20.0,
                max_frequency_hz: 20_000.0,
                min_dbfs: -100.0,
                max_dbfs: 0.0,
            },
            latest: None,
            label_version: 0,
            label: String::with_capacity(192),
            status_line: String::with_capacity(256),
        }))
    }
}

struct LineSpectrumStyleFactory;

impl SpectrumStyleFactory for LineSpectrumStyleFactory {
    fn style_id(&self) -> &'static str {
        DEFAULT_SPECTRUM_STYLE_ID
    }

    fn create(&self) -> Box<dyn SpectrumStyle> {
        Box::new(LineSpectrumStyle {
            points: Vec::with_capacity(1025),
        })
    }
}

struct LineSpectrumStyle {
    points: Vec<Point>,
}

impl SpectrumStyle for LineSpectrumStyle {
    fn draw(&mut self, context: SpectrumStyleContext<'_>, renderer: &mut dyn RenderTarget) {
        self.points.clear();
        for (index, dbfs) in context
            .snapshot
            .spectrum_dbfs
            .iter()
            .copied()
            .enumerate()
            .skip(1)
        {
            let frequency = index as f32 * context.snapshot.fft.bin_hz;
            if frequency < context.coordinates.min_frequency_hz {
                continue;
            }
            if frequency > context.coordinates.max_frequency_hz {
                break;
            }
            self.points
                .push(context.coordinates.point(frequency, dbfs, context.rect));
        }
        renderer.polyline(
            context.rect,
            &self.points,
            Stroke {
                width: 1.5,
                color: Color::rgb(92, 230, 199),
            },
        );
    }
}

struct SpectrumModule {
    reader: SpectrumReader,
    style: Box<dyn SpectrumStyle>,
    coordinates: SpectrumCoordinates,
    latest: Option<Arc<SpectrumSnapshotSet>>,
    label_version: u64,
    label: String,
    status_line: String,
}

impl SpectrumModule {
    fn update_label(&mut self, snapshot: &SpectrumSnapshotSet) {
        if self.label_version == snapshot.result_version {
            return;
        }
        self.label_version = snapshot.result_version;
        self.label.clear();
        let first_bin = (self.coordinates.min_frequency_hz / snapshot.fft.bin_hz).ceil() as usize;
        let last_bin = (self.coordinates.max_frequency_hz / snapshot.fft.bin_hz).floor() as usize;
        let peak = snapshot
            .spectrum_dbfs
            .iter()
            .enumerate()
            .take(last_bin.saturating_add(1))
            .skip(first_bin)
            .max_by(|left, right| left.1.total_cmp(right.1));
        let channel = match snapshot.channel {
            crate::dataflow::ChannelSelection::Discrete(index) => {
                let _ = write!(self.label, "Channel {index}");
                None
            }
            selection => Some(selection.label()),
        };
        if let Some(channel) = channel {
            self.label.push_str(channel);
        }
        if let Some((peak_bin, peak_dbfs)) = peak {
            let _ = write!(
                self.label,
                " | FFT {}/{} periodic Hann | {:.1} Hz/bin | peak {:.1} Hz {:.1} dBFS | v{}",
                snapshot.fft.window_size,
                snapshot.fft.hop_size,
                snapshot.fft.bin_hz,
                peak_bin as f32 * snapshot.fft.bin_hz,
                peak_dbfs,
                snapshot.result_version,
            );
        }
    }

    fn draw_message(&self, frame: &mut ModuleFrame<'_>, text: &str, color: Color) {
        frame.renderer.text(
            frame.rect,
            Point {
                x: frame.rect.min.x + 8.0,
                y: frame.rect.min.y + 8.0,
            },
            text,
            TextStyle { size: 12.0, color },
        );
    }
}

impl Module for SpectrumModule {
    fn draw(&mut self, mut frame: ModuleFrame<'_>) -> ModuleResponse {
        let requested_channel = self.reader.requested_channel();
        let requested_revision = self.reader.requested_revision();
        self.latest = self.reader.latest().filter(|snapshot| {
            snapshot.channel == requested_channel && snapshot.config_revision == requested_revision
        });
        if let Some(error) = self.reader.analysis_error() {
            self.status_line.clear();
            let _ = write!(self.status_line, "Channel error: {error}");
            frame.renderer.text(
                frame.rect,
                Point {
                    x: frame.rect.min.x + 8.0,
                    y: frame.rect.min.y + 8.0,
                },
                &self.status_line,
                TextStyle {
                    size: 12.0,
                    color: Color::rgb(255, 110, 110),
                },
            );
            return ModuleResponse {
                repaint_after: Some(Duration::from_millis(100)),
            };
        }

        if let Some(snapshot) = self.latest.clone() {
            self.coordinates.max_frequency_hz =
                20_000.0_f32.min(snapshot.descriptor.sample_rate_hz as f32 * 0.5);
            self.update_label(&snapshot);
            self.style.draw(
                SpectrumStyleContext {
                    snapshot: &snapshot,
                    coordinates: &self.coordinates,
                    rect: frame.rect,
                    monotonic_time: frame.monotonic_time,
                },
                frame.renderer,
            );
            let status = self.reader.source_status();
            let color = match status.state {
                SourceState::Running => Color::rgb(190, 205, 215),
                SourceState::Paused => Color::rgb(255, 210, 100),
                SourceState::DeviceLost | SourceState::Stopped => Color::rgb(255, 110, 110),
                SourceState::Starting => Color::rgb(190, 205, 215),
            };
            let state = match status.state {
                SourceState::Starting => "STARTING",
                SourceState::Running => "RUNNING",
                SourceState::Paused => "PAUSED",
                SourceState::DeviceLost => "DEVICE LOST",
                SourceState::Stopped => "STOPPED",
            };
            self.status_line.clear();
            let _ = write!(
                self.status_line,
                "{state} | {} Hz | {} ch | {}",
                snapshot.descriptor.sample_rate_hz, snapshot.descriptor.channel_count, self.label,
            );
            if status.dropped_blocks != 0 {
                let _ = write!(self.status_line, " | dropped {}", status.dropped_blocks);
            }
            frame.renderer.text(
                frame.rect,
                Point {
                    x: frame.rect.min.x + 8.0,
                    y: frame.rect.min.y + 8.0,
                },
                &self.status_line,
                TextStyle { size: 12.0, color },
            );
        } else {
            self.draw_message(
                &mut frame,
                "WAITING FOR SYSTEM AUDIO",
                Color::rgb(190, 205, 215),
            );
        }

        ModuleResponse {
            repaint_after: Some(Duration::from_millis(16)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        thread,
        time::{Duration, Instant},
    };

    use super::*;
    use crate::{
        audio::{
            AudioDescriptor, ChannelLayout, Generation, SourceId, SourceState, SourceTelemetry,
            create_block_transport,
        },
        dataflow::{ChannelSelection, FftConfig, SpectrumDataflow, SpectrumRequest},
        module::{ModuleFrame, ModuleServices, Registry},
        render::{Rect, Stroke},
    };

    #[derive(Default)]
    struct RecordingTarget {
        polylines: usize,
        text: Vec<String>,
    }

    impl RenderTarget for RecordingTarget {
        fn polyline(&mut self, _clip: Rect, _points: &[Point], _stroke: Stroke) {
            self.polylines += 1;
        }

        fn text(&mut self, _clip: Rect, _position: Point, text: &str, _style: TextStyle) {
            self.text.push(text.to_owned());
        }
    }

    fn frame<'a>(renderer: &'a mut RecordingTarget) -> ModuleFrame<'a> {
        ModuleFrame {
            rect: Rect {
                min: Point { x: 0.0, y: 0.0 },
                max: Point { x: 800.0, y: 400.0 },
            },
            monotonic_time: Duration::ZERO,
            renderer,
        }
    }

    #[test]
    fn paused_channel_change_invalidates_the_cached_previous_channel_curve() {
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), 48_000, 2, ChannelLayout::Stereo)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, processing) =
            create_block_transport(2, 2, 32, Arc::clone(&telemetry)).unwrap();
        let dataflow = SpectrumDataflow::spawn(descriptor, processing, Arc::clone(&telemetry));
        let subscription = dataflow
            .subscribe(SpectrumRequest::new(
                ChannelSelection::Left,
                FftConfig::new(32, 32, -160.0).unwrap(),
            ))
            .unwrap();
        capture.set_worker(dataflow.worker_thread());
        let reader = subscription.reader();
        let controller = subscription.controller();
        capture.write(&[0.25_f32; 64]);
        let deadline = Instant::now() + Duration::from_secs(1);
        while reader.latest().is_none() {
            assert!(Instant::now() < deadline, "timed out waiting for spectrum");
            thread::sleep(Duration::from_millis(1));
        }
        telemetry.set_state(SourceState::Paused);

        let mut registry = Registry::default();
        register(&mut registry).unwrap();
        let mut module = registry
            .create_module(SPECTRUM_MODULE_ID, &ModuleServices { spectrum: reader })
            .unwrap();
        let mut rendering = RecordingTarget::default();
        module.draw(frame(&mut rendering));
        assert_eq!(rendering.polylines, 1);
        assert!(rendering.text.iter().any(|text| text.contains("Left")));

        controller.set_channel(ChannelSelection::Right).unwrap();
        rendering.polylines = 0;
        rendering.text.clear();
        module.draw(frame(&mut rendering));
        assert_eq!(rendering.polylines, 0);
        assert!(
            rendering
                .text
                .iter()
                .any(|text| text == "WAITING FOR SYSTEM AUDIO")
        );
    }
}
