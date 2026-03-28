use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crate::buffer::AudioProducer;

pub struct AudioCapture {
    _stream: cpal::Stream,
    pub num_channels: usize,
    pub sample_rate: u32,
}

impl AudioCapture {
    pub fn new(mut producer: AudioProducer) -> Self {
        let host = cpal::default_host();
        let device = host.default_output_device().expect("找不到输出设备");
        let config = device.default_output_config().expect("无法获取默认输出配置");

        let num_channels = config.channels() as usize;
        let sample_rate = config.sample_rate();
        let stream_cfg: cpal::StreamConfig = config.into();

        let stream = device.build_input_stream(
            &stream_cfg,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // data 是交错的多声道采样（[L, R, L, R, ...]），按帧切分后逐帧推入
                for frame in data.chunks(producer.num_channels) {
                    producer.push_frame(frame);
                }
            },
            |err| eprintln!("音频流错误: {:?}", err),
            None,
        ).expect("无法创建音频流");

        stream.play().expect("无法启动音频流");

        Self { _stream: stream, num_channels, sample_rate }
    }
}
