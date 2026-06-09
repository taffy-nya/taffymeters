use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crate::error::AudioError;
use super::buffer::AudioProducer;

pub struct AudioCapture {
    _stream: cpal::Stream,
    pub num_channels: usize,
    pub sample_rate: u32,
}

impl AudioCapture {
    pub fn new(mut producer: AudioProducer) -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(AudioError::NoOutputDevice)?;
        let config = device.default_output_config().map_err(AudioError::DefaultConfig)?;

        let num_channels = config.channels() as usize;
        let sample_rate = config.sample_rate();
        let stream_cfg: cpal::StreamConfig = config.into();

        let stream = device.build_input_stream(
            &stream_cfg,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for frame in data.chunks(producer.num_channels) {
                    producer.push_frame(frame);
                }
            },
            |err| eprintln!("{}", AudioError::Stream(err)),
            None,
        ).map_err(AudioError::BuildStream)?;

        stream.play().map_err(AudioError::PlayStream)?;

        Ok(Self { _stream: stream, num_channels, sample_rate })
    }
}
