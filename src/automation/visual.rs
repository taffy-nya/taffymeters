use std::{
    error::Error,
    fmt,
    fs::File,
    future::Future,
    io::BufReader,
    path::{Path, PathBuf},
    pin::pin,
    sync::{Arc, Mutex, mpsc},
    task::{Context as TaskContext, Poll, Wake, Waker},
    time::{Duration, Instant},
};

use eframe::{egui, egui_wgpu, wgpu};
use image::{ImageFormat, ImageReader};
use serde::Serialize;

const GPU_DEADLINE: Duration = Duration::from_secs(5);
const PROFILE_ID: &str = "dx12-low-power-rgba8unorm-predictable-v1";

#[derive(Debug)]
pub(crate) enum VisualError {
    Initialization {
        stage: &'static str,
        message: String,
    },
    Render {
        message: String,
    },
    Png {
        path: PathBuf,
        message: String,
    },
    InvalidDimensions {
        width: u32,
        height: u32,
    },
    InvalidRgba {
        width: u32,
        height: u32,
        expected_bytes: usize,
        actual_bytes: usize,
    },
}

impl fmt::Display for VisualError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Initialization { stage, message } => {
                write!(
                    f,
                    "offscreen GPU initialization failed at {stage}: {message}"
                )
            }
            Self::Render { message } => write!(f, "offscreen rendering failed: {message}"),
            Self::Png { path, message } => {
                write!(f, "PNG operation failed for {}: {message}", path.display())
            }
            Self::InvalidDimensions { width, height } => {
                write!(f, "invalid image dimensions {width}x{height}")
            }
            Self::InvalidRgba {
                width,
                height,
                expected_bytes,
                actual_bytes,
            } => write!(
                f,
                "RGBA8 data for {width}x{height} requires {expected_bytes} bytes, got {actual_bytes}"
            ),
        }
    }
}

impl Error for VisualError {}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RendererFacts {
    pub(crate) profile: String,
    pub(crate) requested_backend: String,
    pub(crate) requested_power_preference: String,
    pub(crate) compatible_surface: bool,
    pub(crate) color_format: String,
    pub(crate) sample_count: u32,
    pub(crate) adapter_name: String,
    pub(crate) adapter_vendor: u32,
    pub(crate) adapter_device: u32,
    pub(crate) adapter_device_type: String,
    pub(crate) adapter_backend: String,
    pub(crate) adapter_driver: String,
    pub(crate) adapter_driver_info: String,
    pub(crate) renderer_options: RendererOptionsFacts,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RendererOptionsFacts {
    pub(crate) msaa_samples: u32,
    pub(crate) depth_stencil_format: Option<String>,
    pub(crate) dithering: bool,
    pub(crate) predictable_texture_filtering: bool,
}

#[derive(Debug)]
pub(crate) struct Rendered {
    pub(crate) logical_size: [u32; 2],
    pub(crate) pixel_size: [u32; 2],
    pub(crate) pixels_per_point: f32,
    pub(crate) egui_frame_nr: u64,
    pub(crate) egui_pass_nr: u64,
    pub(crate) submission_id: u64,
    pub(crate) rgba8: Vec<u8>,
    pub(crate) renderer: RendererFacts,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Rgba8View<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba8: &'a [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rgba8Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba8: Vec<u8>,
}

impl Rgba8Image {
    pub(crate) fn as_view(&self) -> Rgba8View<'_> {
        Rgba8View {
            width: self.width,
            height: self.height,
            rgba8: &self.rgba8,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct DiffBoundingBox {
    /// Inclusive left edge in pixels.
    pub(crate) min_x: u32,
    /// Inclusive top edge in pixels.
    pub(crate) min_y: u32,
    /// Inclusive right edge in pixels.
    pub(crate) max_x: u32,
    /// Inclusive bottom edge in pixels.
    pub(crate) max_y: u32,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ComparisonMetrics {
    pub(crate) dimensions_match: bool,
    pub(crate) expected_size: [u32; 2],
    pub(crate) actual_size: [u32; 2],
    /// `None` when dimensions differ, because the pixel grids are not comparable.
    pub(crate) changed_pixels: Option<u64>,
    /// `None` when dimensions differ.
    pub(crate) total_pixels: Option<u64>,
    /// `None` when dimensions differ.
    pub(crate) max_channel_delta: Option<u8>,
    /// RGBA channel order; `None` when dimensions differ.
    pub(crate) mean_absolute_error: Option<[f64; 4]>,
    pub(crate) diff_bbox: Option<DiffBoundingBox>,
}

/// An offscreen DX12 renderer with persistent egui texture state.
///
/// Feed every `FullOutput` from a scene's warm-up and event frames through this object in
/// sequence. egui emits its font atlas only when it changes, so constructing a renderer only
/// after the last context frame would omit textures created by earlier frames.
pub(crate) struct OffscreenRenderer {
    _instance: wgpu::Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: egui_wgpu::Renderer,
    facts: RendererFacts,
    uncaptured_errors: Arc<Mutex<Vec<String>>>,
    next_submission_id: u64,
}

impl OffscreenRenderer {
    pub(crate) fn new() -> Result<Self, VisualError> {
        if !wgpu::Instance::enabled_backend_features().contains(wgpu::Backends::DX12) {
            return Err(VisualError::Initialization {
                stage: "DX12 backend availability",
                message: "this build does not enable the DX12 wgpu backend".to_owned(),
            });
        }

        let init_deadline = Instant::now() + GPU_DEADLINE;
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = wgpu::Backends::DX12;
        let instance = wgpu::Instance::new(instance_descriptor);

        let adapters = block_on_deadline(
            instance.enumerate_adapters(wgpu::Backends::DX12),
            init_deadline,
        )
        .map_err(|message| VisualError::Initialization {
            stage: "DX12 adapter enumeration",
            message,
        })?;
        let enumerated = adapters
            .iter()
            .map(|adapter| format_adapter_info(&adapter.get_info()))
            .collect::<Vec<_>>();
        if adapters.is_empty() {
            return Err(VisualError::Initialization {
                stage: "DX12 adapter enumeration",
                message: "no DX12 adapters were enumerated; attempted backend=DX12, compatible_surface=None, power_preference=LowPower".to_owned(),
            });
        }

        let adapter_options = wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            force_fallback_adapter: false,
            compatible_surface: None,
        };
        let adapter = block_on_deadline(instance.request_adapter(&adapter_options), init_deadline)
            .map_err(|message| VisualError::Initialization {
                stage: "DX12 LowPower adapter request timeout",
                message: format!("{message}; enumerated adapters: {enumerated:?}"),
            })?
            .map_err(|error| VisualError::Initialization {
                stage: "DX12 LowPower adapter request",
                message: format!("{error}; enumerated adapters: {enumerated:?}"),
            })?;
        let adapter_info = adapter.get_info();
        if adapter_info.backend != wgpu::Backend::Dx12 {
            return Err(VisualError::Initialization {
                stage: "adapter backend verification",
                message: format!(
                    "requested DX12 but selected backend {:?}; enumerated adapters: {enumerated:?}",
                    adapter_info.backend
                ),
            });
        }
        let target_features = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba8Unorm);
        let target_usage = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC;
        if !target_features.allowed_usages.contains(target_usage) {
            return Err(VisualError::Initialization {
                stage: "Rgba8Unorm target capability",
                message: format!(
                    "selected adapter does not support {target_usage:?} for Rgba8Unorm; adapter: {}",
                    format_adapter_info(&adapter_info)
                ),
            });
        }

        let device_descriptor = wgpu::DeviceDescriptor {
            label: Some("taffymeters-automation-offscreen"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        };
        let (device, queue) =
            block_on_deadline(adapter.request_device(&device_descriptor), init_deadline)
                .map_err(|message| VisualError::Initialization {
                    stage: "DX12 device request timeout",
                    message: format!("{message}; adapter: {}", format_adapter_info(&adapter_info)),
                })?
                .map_err(|error| VisualError::Initialization {
                    stage: "DX12 device request",
                    message: format!("{error}; adapter: {}", format_adapter_info(&adapter_info)),
                })?;

        let uncaptured_errors = Arc::new(Mutex::new(Vec::new()));
        let error_sink = Arc::clone(&uncaptured_errors);
        device.on_uncaptured_error(Arc::new(move |error| {
            if let Ok(mut errors) = error_sink.lock() {
                errors.push(error.to_string());
            }
        }));

        let options = egui_wgpu::RendererOptions::PREDICTABLE;
        let renderer = egui_wgpu::Renderer::new(&device, wgpu::TextureFormat::Rgba8Unorm, options);
        let facts = RendererFacts {
            profile: PROFILE_ID.to_owned(),
            requested_backend: "DX12".to_owned(),
            requested_power_preference: "LowPower".to_owned(),
            compatible_surface: false,
            color_format: "Rgba8Unorm".to_owned(),
            sample_count: 1,
            adapter_name: adapter_info.name,
            adapter_vendor: adapter_info.vendor,
            adapter_device: adapter_info.device,
            adapter_device_type: format!("{:?}", adapter_info.device_type),
            adapter_backend: format!("{:?}", adapter_info.backend),
            adapter_driver: adapter_info.driver,
            adapter_driver_info: adapter_info.driver_info,
            renderer_options: RendererOptionsFacts {
                msaa_samples: options.msaa_samples,
                depth_stencil_format: options
                    .depth_stencil_format
                    .map(|format| format!("{format:?}")),
                dithering: options.dithering,
                predictable_texture_filtering: options.predictable_texture_filtering,
            },
        };

        device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| render_error(format!("wgpu initialization poll failed: {error}")))?;
        if let Some(error) = take_uncaptured_error(&uncaptured_errors)? {
            return Err(VisualError::Render {
                message: format!("egui-wgpu renderer initialization validation error: {error}"),
            });
        }

        Ok(Self {
            _instance: instance,
            device,
            queue,
            renderer,
            facts,
            uncaptured_errors,
            next_submission_id: 1,
        })
    }

    /// Render one complete egui frame and read it back as tightly packed RGBA8.
    pub(crate) fn render(
        &mut self,
        context: &egui::Context,
        output: egui::FullOutput,
        logical_width: u32,
        logical_height: u32,
    ) -> Result<Rendered, VisualError> {
        let pixels_per_point = output.pixels_per_point;
        let pixel_size = physical_size(logical_width, logical_height, pixels_per_point)?;
        if pixel_size[0] > self.device.limits().max_texture_dimension_2d
            || pixel_size[1] > self.device.limits().max_texture_dimension_2d
        {
            return Err(VisualError::Render {
                message: format!(
                    "offscreen target {}x{} exceeds the device's 2D texture limit {}",
                    pixel_size[0],
                    pixel_size[1],
                    self.device.limits().max_texture_dimension_2d
                ),
            });
        }
        let rgba_len = rgba8_len(pixel_size[0], pixel_size[1])?;
        let unpadded_bytes_per_row = pixel_size[0]
            .checked_mul(4)
            .ok_or_else(|| render_error("RGBA8 row byte count overflow"))?;
        let alignment = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded_bytes_per_row
            .checked_add(alignment - 1)
            .ok_or_else(|| render_error("padded readback row byte count overflow"))?
            / alignment
            * alignment;
        let padded_buffer_size = u64::from(padded_bytes_per_row)
            .checked_mul(u64::from(pixel_size[1]))
            .ok_or_else(|| render_error("readback buffer size overflow"))?;
        if padded_buffer_size > self.device.limits().max_buffer_size {
            return Err(VisualError::Render {
                message: format!(
                    "readback buffer size {padded_buffer_size} exceeds device limit {}",
                    self.device.limits().max_buffer_size
                ),
            });
        }

        if let Some(error) = take_uncaptured_error(&self.uncaptured_errors)? {
            return Err(render_error(format!(
                "prior wgpu validation error: {error}"
            )));
        }
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("taffymeters-automation-rgba8-target"),
            size: wgpu::Extent3d {
                width: pixel_size[0],
                height: pixel_size[1],
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("taffymeters-automation-rgba8-readback"),
            size: padded_buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let paint_jobs = context.tessellate(output.shapes, pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: pixel_size,
            pixels_per_point,
        };
        for (texture_id, image_delta) in &output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *texture_id, image_delta);
        }

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("taffymeters-automation-render-encoder"),
            });
        let callback_commands = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("taffymeters-automation-egui-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut render_pass = render_pass.forget_lifetime();
            self.renderer
                .render(&mut render_pass, &paint_jobs, &screen_descriptor);
        }
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(pixel_size[1]),
                },
            },
            wgpu::Extent3d {
                width: pixel_size[0],
                height: pixel_size[1],
                depth_or_array_layers: 1,
            },
        );

        let deadline = Instant::now() + GPU_DEADLINE;
        let submission = self
            .queue
            .submit(callback_commands.into_iter().chain([encoder.finish()]));
        let submission_id = self.next_submission_id;
        self.next_submission_id = self
            .next_submission_id
            .checked_add(1)
            .ok_or_else(|| render_error("renderer submission ID overflow"))?;
        for texture_id in output.textures_delta.free {
            self.renderer.free_texture(&texture_id);
        }

        let (map_sender, map_receiver) = mpsc::sync_channel(1);
        readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = map_sender.send(result);
            });
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(render_error(
                "GPU readback exceeded the 5 second wall-time deadline",
            ));
        }
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(remaining),
            })
            .map_err(|error| {
                render_error(format!("GPU submission/readback poll failed: {error}"))
            })?;
        let map_result = map_receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => {
                    render_error("GPU map callback exceeded the 5 second wall-time deadline")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    render_error("GPU map callback disconnected without a result")
                }
            })?;
        map_result.map_err(|error| render_error(format!("GPU readback map failed: {error}")))?;

        if Instant::now() >= deadline {
            return Err(render_error(
                "GPU readback exceeded the 5 second wall-time deadline",
            ));
        }
        let mapped = readback.slice(..).get_mapped_range();
        if mapped.len()
            < usize::try_from(padded_buffer_size).map_err(|_| {
                render_error("readback buffer size cannot be represented on this platform")
            })?
        {
            drop(mapped);
            readback.unmap();
            return Err(render_error(
                "mapped readback buffer is shorter than requested",
            ));
        }
        let row_bytes = usize::try_from(unpadded_bytes_per_row)
            .map_err(|_| render_error("RGBA8 row size cannot be represented on this platform"))?;
        let padded_row_bytes = usize::try_from(padded_bytes_per_row)
            .map_err(|_| render_error("padded row size cannot be represented on this platform"))?;
        let mut rgba8 = Vec::new();
        rgba8
            .try_reserve_exact(rgba_len)
            .map_err(|error| render_error(format!("could not allocate RGBA8 readback: {error}")))?;
        rgba8.resize(rgba_len, 0);
        for row in 0..pixel_size[1] as usize {
            if Instant::now() >= deadline {
                return Err(render_error(
                    "GPU readback exceeded the 5 second wall-time deadline",
                ));
            }
            let source_start = row
                .checked_mul(padded_row_bytes)
                .ok_or_else(|| render_error("readback source row offset overflow"))?;
            let source_end = source_start
                .checked_add(row_bytes)
                .ok_or_else(|| render_error("readback source row end overflow"))?;
            let destination_start = row
                .checked_mul(row_bytes)
                .ok_or_else(|| render_error("RGBA8 destination row offset overflow"))?;
            let destination_end = destination_start
                .checked_add(row_bytes)
                .ok_or_else(|| render_error("RGBA8 destination row end overflow"))?;
            rgba8[destination_start..destination_end]
                .copy_from_slice(&mapped[source_start..source_end]);
        }
        drop(mapped);
        readback.unmap();
        if Instant::now() >= deadline {
            return Err(render_error(
                "GPU readback exceeded the 5 second wall-time deadline",
            ));
        }

        if let Some(error) = take_uncaptured_error(&self.uncaptured_errors)? {
            return Err(render_error(error));
        }

        Ok(Rendered {
            logical_size: [logical_width, logical_height],
            pixel_size,
            pixels_per_point,
            egui_frame_nr: context.cumulative_frame_nr(),
            egui_pass_nr: context.cumulative_pass_nr(),
            submission_id,
            rgba8,
            renderer: self.facts.clone(),
        })
    }
}

/// Decode a PNG as RGBA8 without resizing or converting through a display color space.
pub(crate) fn load_png(path: &Path) -> Result<Rgba8Image, VisualError> {
    let dimensions = png_reader(path)?
        .into_dimensions()
        .map_err(|error| png_error(path, error))?;
    let expected_bytes =
        rgba8_len(dimensions.0, dimensions.1).map_err(|error| VisualError::Png {
            path: path.to_owned(),
            message: error.to_string(),
        })?;
    let reader = png_reader(path)?;
    let decoded = reader
        .decode()
        .map_err(|error| png_error(path, error))?
        .to_rgba8();
    if decoded.dimensions() != dimensions || decoded.as_raw().len() != expected_bytes {
        return Err(VisualError::Png {
            path: path.to_owned(),
            message: format!(
                "decoded RGBA8 length/dimensions are inconsistent: expected {}x{} with {expected_bytes} bytes, got {}x{} with {} bytes",
                dimensions.0,
                dimensions.1,
                decoded.width(),
                decoded.height(),
                decoded.as_raw().len()
            ),
        });
    }
    Ok(Rgba8Image {
        width: dimensions.0,
        height: dimensions.1,
        rgba8: decoded.into_raw(),
    })
}

fn png_reader(path: &Path) -> Result<ImageReader<BufReader<File>>, VisualError> {
    let file = File::open(path).map_err(|error| png_error(path, error))?;
    let mut reader = ImageReader::with_format(BufReader::new(file), ImageFormat::Png);
    reader.limits(image::Limits::default());
    Ok(reader)
}

/// Compare two PNG files after decoding both to RGBA8. Size changes are returned as a
/// non-matching comparison; unreadable/corrupt PNG files are errors.
pub(crate) fn compare_png_paths(
    expected_path: &Path,
    actual_path: &Path,
) -> Result<ComparisonMetrics, VisualError> {
    let expected = load_png(expected_path)?;
    let actual = load_png(actual_path)?;
    compare_rgba(expected.as_view(), actual.as_view())
}

/// Compare equal-sized images exactly, including alpha. Pixels with any differing channel are
/// counted once. A dimension mismatch is reported without scaling or comparing unlike grids.
pub(crate) fn compare_rgba(
    expected: Rgba8View<'_>,
    actual: Rgba8View<'_>,
) -> Result<ComparisonMetrics, VisualError> {
    validate_rgba(expected)?;
    validate_rgba(actual)?;
    let expected_size = [expected.width, expected.height];
    let actual_size = [actual.width, actual.height];
    if expected_size != actual_size {
        return Ok(ComparisonMetrics {
            dimensions_match: false,
            expected_size,
            actual_size,
            changed_pixels: None,
            total_pixels: None,
            max_channel_delta: None,
            mean_absolute_error: None,
            diff_bbox: None,
        });
    }

    let total_pixels = u64::from(expected.width) * u64::from(expected.height);
    let mut changed_pixels = 0_u64;
    let mut max_channel_delta = 0_u8;
    let mut channel_error_sums = [0_u128; 4];
    let mut diff_bbox: Option<DiffBoundingBox> = None;
    for (pixel_index, (expected_pixel, actual_pixel)) in expected
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.rgba8.as_chunks::<4>().0.iter())
        .enumerate()
    {
        let mut changed = false;
        for channel in 0..4 {
            let delta = expected_pixel[channel].abs_diff(actual_pixel[channel]);
            if delta != 0 {
                changed = true;
                max_channel_delta = max_channel_delta.max(delta);
                channel_error_sums[channel] += u128::from(delta);
            }
        }
        if changed {
            changed_pixels += 1;
            let pixel_index = u64::try_from(pixel_index)
                .map_err(|_| render_error("pixel index cannot be represented as u64"))?;
            let x = u32::try_from(pixel_index % u64::from(expected.width))
                .map_err(|_| render_error("pixel x coordinate cannot be represented as u32"))?;
            let y = u32::try_from(pixel_index / u64::from(expected.width))
                .map_err(|_| render_error("pixel y coordinate cannot be represented as u32"))?;
            match &mut diff_bbox {
                Some(bbox) => {
                    bbox.min_x = bbox.min_x.min(x);
                    bbox.min_y = bbox.min_y.min(y);
                    bbox.max_x = bbox.max_x.max(x);
                    bbox.max_y = bbox.max_y.max(y);
                }
                None => {
                    diff_bbox = Some(DiffBoundingBox {
                        min_x: x,
                        min_y: y,
                        max_x: x,
                        max_y: y,
                    });
                }
            }
        }
    }
    let mean_absolute_error = if total_pixels == 0 {
        [0.0; 4]
    } else {
        channel_error_sums.map(|sum| sum as f64 / total_pixels as f64)
    };
    Ok(ComparisonMetrics {
        dimensions_match: true,
        expected_size,
        actual_size,
        changed_pixels: Some(changed_pixels),
        total_pixels: Some(total_pixels),
        max_channel_delta: Some(max_channel_delta),
        mean_absolute_error: Some(mean_absolute_error),
        diff_bbox,
    })
}

/// Save tightly packed RGBA8 bytes as PNG. This never rescales or changes channel order.
pub(crate) fn save_png(
    path: &Path,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<(), VisualError> {
    validate_rgba(Rgba8View {
        width,
        height,
        rgba8,
    })?;
    image::save_buffer_with_format(
        path,
        rgba8,
        width,
        height,
        image::ColorType::Rgba8,
        ImageFormat::Png,
    )
    .map_err(|error| png_error(path, error))
}

/// Save a full-size visual diff: changed pixels are red, pixels present in only one image are
/// magenta, and unchanged pixels are black.
pub(crate) fn save_diff_png(
    path: &Path,
    expected: Rgba8View<'_>,
    actual: Rgba8View<'_>,
) -> Result<(), VisualError> {
    let _ = compare_rgba(expected, actual)?;
    let width = expected.width.max(actual.width);
    let height = expected.height.max(actual.height);
    let mut diff = Vec::new();
    let diff_len = rgba8_len(width, height)?;
    diff.try_reserve_exact(diff_len)
        .map_err(|error| render_error(format!("could not allocate diff image: {error}")))?;
    diff.resize(diff_len, 0);

    for y in 0..height {
        for x in 0..width {
            let expected_pixel = rgba_pixel(expected, x, y);
            let actual_pixel = rgba_pixel(actual, x, y);
            let color = match (expected_pixel, actual_pixel) {
                (Some(expected), Some(actual)) if expected == actual => [0, 0, 0, 255],
                (Some(_), Some(_)) => [255, 0, 0, 255],
                _ => [255, 0, 255, 255],
            };
            let offset = ((y as usize * width as usize) + x as usize) * 4;
            diff[offset..offset + 4].copy_from_slice(&color);
        }
    }
    save_png(path, width, height, &diff)
}

fn rgba_pixel(image: Rgba8View<'_>, x: u32, y: u32) -> Option<&[u8]> {
    if x >= image.width || y >= image.height {
        return None;
    }
    let index = ((y as usize * image.width as usize) + x as usize) * 4;
    Some(&image.rgba8[index..index + 4])
}

fn validate_rgba(image: Rgba8View<'_>) -> Result<(), VisualError> {
    let expected_bytes = rgba8_len(image.width, image.height)?;
    if image.rgba8.len() != expected_bytes {
        return Err(VisualError::InvalidRgba {
            width: image.width,
            height: image.height,
            expected_bytes,
            actual_bytes: image.rgba8.len(),
        });
    }
    Ok(())
}

fn rgba8_len(width: u32, height: u32) -> Result<usize, VisualError> {
    if width == 0 || height == 0 {
        return Err(VisualError::InvalidDimensions { width, height });
    }
    usize::try_from(width)
        .ok()
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| {
            render_error(format!(
                "RGBA8 allocation size overflow for {width}x{height}"
            ))
        })
}

fn physical_size(
    logical_width: u32,
    logical_height: u32,
    pixels_per_point: f32,
) -> Result<[u32; 2], VisualError> {
    if logical_width == 0 || logical_height == 0 {
        return Err(VisualError::InvalidDimensions {
            width: logical_width,
            height: logical_height,
        });
    }
    if !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return Err(render_error(format!(
            "FullOutput has invalid pixels_per_point={pixels_per_point}"
        )));
    }
    let scale = |logical: u32| {
        let pixels = f64::from(logical) * f64::from(pixels_per_point);
        if !pixels.is_finite() || pixels < 1.0 || pixels > f64::from(u32::MAX) {
            return None;
        }
        let rounded = pixels.round();
        (rounded >= 1.0 && rounded <= f64::from(u32::MAX)).then_some(rounded as u32)
    };
    let width = scale(logical_width).ok_or_else(|| {
        render_error("logical width multiplied by pixels_per_point is out of range")
    })?;
    let height = scale(logical_height).ok_or_else(|| {
        render_error("logical height multiplied by pixels_per_point is out of range")
    })?;
    Ok([width, height])
}

fn format_adapter_info(info: &wgpu::AdapterInfo) -> String {
    format!(
        "name={:?}, vendor={}, device={}, device_type={:?}, backend={:?}, driver={:?}, driver_info={:?}",
        info.name,
        info.vendor,
        info.device,
        info.device_type,
        info.backend,
        info.driver,
        info.driver_info
    )
}

fn take_uncaptured_error(errors: &Mutex<Vec<String>>) -> Result<Option<String>, VisualError> {
    let mut errors = errors
        .lock()
        .map_err(|_| render_error("wgpu validation error storage was poisoned"))?;
    Ok((!errors.is_empty()).then(|| errors.drain(..).collect::<Vec<_>>().join("; ")))
}

fn png_error(path: &Path, error: impl fmt::Display) -> VisualError {
    VisualError::Png {
        path: path.to_owned(),
        message: error.to_string(),
    }
}

fn render_error(message: impl Into<String>) -> VisualError {
    VisualError::Render {
        message: message.into(),
    }
}

struct ChannelWaker(mpsc::SyncSender<()>);

impl Wake for ChannelWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let _ = self.0.try_send(());
    }
}

fn block_on_deadline<F: Future>(future: F, deadline: Instant) -> Result<F::Output, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let waker = Waker::from(Arc::new(ChannelWaker(sender)));
    let mut context = TaskContext::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return Ok(output);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("operation exceeded the 5 second wall-time deadline".to_owned());
        }
        match receiver.recv_timeout(remaining) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return Err("operation exceeded the 5 second wall-time deadline".to_owned());
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("operation future stopped waking its executor".to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    #[test]
    fn rgba_comparison_reports_equal_image_and_exact_one_pixel_difference() {
        let expected = Rgba8Image {
            width: 2,
            height: 2,
            rgba8: vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
        };
        let same = compare_rgba(expected.as_view(), expected.as_view()).unwrap();
        assert!(same.dimensions_match && same.changed_pixels == Some(0));
        assert_eq!(same.changed_pixels, Some(0));
        assert_eq!(same.total_pixels, Some(4));
        assert_eq!(same.max_channel_delta, Some(0));
        assert_eq!(same.mean_absolute_error, Some([0.0; 4]));
        assert_eq!(same.diff_bbox, None);

        let actual = Rgba8Image {
            rgba8: vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 12, 12, 255],
            ..expected.clone()
        };
        let changed = compare_rgba(expected.as_view(), actual.as_view()).unwrap();
        assert!(!(changed.dimensions_match && changed.changed_pixels == Some(0)));
        assert_eq!(changed.changed_pixels, Some(1));
        assert_eq!(changed.max_channel_delta, Some(1));
        assert_eq!(changed.mean_absolute_error, Some([0.0, 0.25, 0.0, 0.0]));
        assert_eq!(
            changed.diff_bbox,
            Some(DiffBoundingBox {
                min_x: 1,
                min_y: 1,
                max_x: 1,
                max_y: 1,
            })
        );
    }

    #[test]
    fn rgba_comparison_reports_size_mismatch_without_scaling() {
        let expected = Rgba8Image {
            width: 2,
            height: 2,
            rgba8: vec![0; 16],
        };
        let actual = Rgba8Image {
            width: 1,
            height: 4,
            rgba8: vec![0; 16],
        };
        let comparison = compare_rgba(expected.as_view(), actual.as_view()).unwrap();
        assert!(!comparison.dimensions_match);
        assert_eq!(comparison.expected_size, [2, 2]);
        assert_eq!(comparison.actual_size, [1, 4]);
        assert_eq!(comparison.changed_pixels, None);
        assert_eq!(comparison.total_pixels, None);
        assert_eq!(comparison.max_channel_delta, None);
        assert_eq!(comparison.mean_absolute_error, None);
        assert_eq!(comparison.diff_bbox, None);
    }

    #[test]
    fn png_comparison_rejects_corrupt_file() {
        static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "taffymeters-corrupt-{}-{}.png",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, b"not a PNG").unwrap();
        let result = compare_png_paths(&path, &path);
        let _ = fs::remove_file(&path);
        assert!(matches!(result, Err(VisualError::Png { .. })));
    }
}
