#[derive(Clone, Default)]
pub struct AudioData {
    pub mono: Vec<f32>,
    pub channels: Vec<Vec<f32>>,
    pub fft: Vec<f32>,
    pub sample_rate: f32,
    pub num_channels: usize,
    pub new_sample_count: usize,
}

impl AudioData {
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
}
