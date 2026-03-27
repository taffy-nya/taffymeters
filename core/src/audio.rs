use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::traits::Producer;
use crate::buffer::AudioProducer;

pub struct AudioStream {
    _stream: cpal::Stream,
}

impl AudioStream {
    pub fn new(mut producer: AudioProducer) -> Self {
        let host = cpal::default_host();
        let device = host.default_output_device().expect("找不到输出设备");
        let config = device.default_output_config().expect("无法获取默认输出配置");
        let channels = config.channels() as usize;
        let config: cpal::StreamConfig = config.into();
        let stream = device.build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                for chunk in data.chunks(channels) {
                    let mono_sample = chunk.iter().sum::<f32>() / channels as f32;
                    let _ = producer.try_push(mono_sample);
                }
            },
            move |err| {
                eprintln!("音频流错误: {:?}", err);
            },
            None
        ).expect("无法创建音频流");

        stream.play().expect("无法启动音频流");
        Self { _stream: stream }
    }
}