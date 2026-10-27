use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    audio::{
        AudioDescriptor, CaptureEndpoint, ChannelLayout, Generation, SourceId, SourceReader,
        SourceTelemetry, create_block_transport,
    },
    dataflow::Dataflow,
    module::{Module, ModuleFrame, ModuleResponse, ModuleServices},
    render::{Circle, ColoredRect, Flow, Image, Point, Rect, RenderTarget, Stroke, TextStyle},
    theme::Palette,
};

/// An offset origin catches accidental absolute-coordinate assumptions.
pub(crate) const TEST_RECT: Rect = Rect {
    min: Point { x: 10.0, y: 20.0 },
    max: Point { x: 410.0, y: 220.0 },
};

/// Matches the 48 kHz publication interval (1/30 s), yielding one PCM result per block.
pub(crate) const BLOCK_FRAMES: usize = 1_600;

pub(crate) struct RecordedPolyline {
    pub points: Vec<Point>,
}

pub(crate) struct RecordedText {
    pub position: Point,
    pub text: String,
}

pub(crate) struct RecordedImage {
    pub rect: Rect,
    pub first_column: usize,
    pub flow: Flow,
    pub width: usize,
    pub height: usize,
    pub changed: Vec<usize>,
    /// Column-major copy of the pixels, top first.
    pub columns: Vec<Vec<crate::render::Color>>,
}

#[derive(Default)]
pub(crate) struct RecordingTarget {
    pub polylines: Vec<RecordedPolyline>,
    pub rects: Vec<ColoredRect>,
    pub circles: Vec<Circle>,
    pub texts: Vec<RecordedText>,
    pub images: Vec<RecordedImage>,
}

impl RecordingTarget {
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn non_empty_polylines(&self) -> usize {
        self.polylines
            .iter()
            .filter(|polyline| !polyline.points.is_empty())
            .count()
    }
}

impl RenderTarget for RecordingTarget {
    fn polyline(&mut self, _clip: Rect, points: &[Point], _stroke: Stroke) {
        self.polylines.push(RecordedPolyline {
            points: points.to_vec(),
        });
    }

    fn fill_rects(&mut self, _clip: Rect, rects: &[ColoredRect]) {
        self.rects.extend_from_slice(rects);
    }

    fn circles(&mut self, _clip: Rect, circles: &[Circle]) {
        self.circles.extend_from_slice(circles);
    }

    fn text(&mut self, _clip: Rect, position: Point, text: &str, _style: TextStyle) {
        self.texts.push(RecordedText {
            position,
            text: text.to_owned(),
        });
    }

    fn scrolling_image(
        &mut self,
        _clip: Rect,
        rect: Rect,
        image: &mut Image,
        first_column: usize,
        flow: Flow,
    ) {
        self.images.push(RecordedImage {
            rect,
            first_column,
            flow,
            width: image.width(),
            height: image.height(),
            changed: image.changed_columns().to_vec(),
            columns: (0..image.width())
                .map(|x| image.column(x).to_vec())
                .collect(),
        });
        image.mark_uploaded();
    }
}

/// Declare before its Modules so their subscriptions drop before the worker.
pub(crate) struct PcmFixture {
    capture: CaptureEndpoint,
    _dataflow: Dataflow,
    channels: usize,
    pub services: ModuleServices,
}

impl PcmFixture {
    pub(crate) fn new(channels: u16) -> Self {
        Self::with_rate(48_000, channels)
    }

    pub(crate) fn with_rate(sample_rate_hz: u32, channels: u16) -> Self {
        let layout = match channels {
            1 => ChannelLayout::Mono,
            2 => ChannelLayout::Stereo,
            _ => ChannelLayout::UnknownDiscrete,
        };
        let descriptor = Arc::new(
            AudioDescriptor::new(SourceId(1), Generation(1), sample_rate_hz, channels, layout)
                .unwrap(),
        );
        let telemetry = Arc::new(SourceTelemetry::new());
        let (mut capture, endpoint) = create_block_transport(
            usize::from(channels),
            8,
            BLOCK_FRAMES,
            Arc::clone(&telemetry),
        )
        .unwrap();
        let (dataflow, handle) =
            Dataflow::spawn(Arc::clone(&descriptor), endpoint, Arc::clone(&telemetry)).unwrap();
        capture.set_worker(dataflow.worker_thread());
        Self {
            capture,
            _dataflow: dataflow,
            channels: usize::from(channels),
            services: ModuleServices {
                source: SourceReader::new(descriptor, telemetry),
                dataflow: handle,
            },
        }
    }

    /// Wait for publication after each block so test draws do not race the worker.
    pub(crate) fn feed(&mut self, interleaved: &[f32]) {
        assert_eq!(interleaved.len() % self.channels, 0, "whole frames only");
        for piece in interleaved.chunks(self.channels * BLOCK_FRAMES) {
            let target = self.services.dataflow.processed_through_frame().unwrap()
                + (piece.len() / self.channels) as u64;
            self.capture.write(piece);
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.services.dataflow.processed_through_frame().unwrap() < target {
                assert!(
                    Instant::now() < deadline,
                    "worker never processed through frame {target}"
                );
                std::thread::yield_now();
            }
        }
    }
}

pub(crate) fn repeat_frame(frame: &[f32], frames: usize) -> Vec<f32> {
    frame
        .iter()
        .copied()
        .cycle()
        .take(frame.len() * frames)
        .collect()
}

pub(crate) fn draw(
    module: &mut dyn Module,
    target: &mut RecordingTarget,
    monotonic_time: Duration,
) -> ModuleResponse {
    module.draw(ModuleFrame {
        rect: TEST_RECT,
        monotonic_time,
        pointer: None,
        scroll_delta_y: 0.0,
        zoom_delta: 1.0,
        palette: &Palette::default(),
        // Full-scale reference: 1.0 reaches REFERENCE_HEIGHT of a signed view's half-height.
        reference_dbfs: 0.0,
        renderer: target,
    })
}
