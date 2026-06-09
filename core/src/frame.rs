use crate::channel::ChannelMode;

#[derive(Clone)]
pub struct AudioFrame {
    pub mono: Vec<f32>,
    pub channels: Vec<Vec<f32>>,
    pub fft: Vec<f32>,
    pub sample_rate: f32,
    pub num_channels: usize,
    pub new_sample_count: usize,
}

impl AudioFrame {
    pub fn new(sample_rate: f32, num_channels: usize, window_size: usize) -> Self {
        Self {
            mono: vec![0.0; window_size],
            channels: vec![vec![0.0; window_size]; num_channels],
            fft: Vec::new(),
            sample_rate,
            num_channels,
            new_sample_count: 0,
        }
    }

    pub fn channel_data(&self, mode: ChannelMode) -> &[f32] {
        match mode {
            ChannelMode::Mono => &self.mono,
            ChannelMode::Left => self.channels.first().map(|v| v.as_slice()).unwrap_or(&self.mono),
            ChannelMode::Right => self.channels.get(1).map(|v| v.as_slice()).unwrap_or(&self.mono),
        }
    }
}
