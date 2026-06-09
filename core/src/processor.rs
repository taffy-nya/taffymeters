use crate::audio::buffer::AudioConsumer;
use crate::dsp::fft::FftProcessor;
use crate::frame::AudioFrame;

pub struct AudioProcessor {
    consumer: AudioConsumer,
    fft: FftProcessor,
    window_size: usize,
    frame: AudioFrame,
    scratch: Vec<Vec<f32>>,
}

impl AudioProcessor {
    pub fn new(
        consumer: AudioConsumer,
        sample_rate: f32,
        num_channels: usize,
        window_size: usize,
    ) -> Self {
        Self {
            consumer,
            fft: FftProcessor::new(window_size),
            window_size,
            frame: AudioFrame::new(sample_rate, num_channels, window_size),
            scratch: vec![Vec::with_capacity(window_size * 4); num_channels],
        }
    }

    pub fn tick(&mut self) -> bool {
        for b in &mut self.scratch { b.clear(); }
        if !self.consumer.pop_into(&mut self.scratch) { return false; }

        let ws = self.window_size;
        for (ch, new_ch) in self.scratch.iter().enumerate() {
            if new_ch.is_empty() { continue; }
            let win = &mut self.frame.channels[ch];
            let n = new_ch.len();
            if n >= ws { win.copy_from_slice(&new_ch[n - ws..]); }
            else { win.rotate_left(n); win[ws - n..].copy_from_slice(new_ch); }
        }

        self.frame.mono.fill(0.0);
        for ch in &self.frame.channels {
            for (mono, &sample) in self.frame.mono.iter_mut().zip(ch) {
                *mono += sample;
            }
        }
        if self.frame.num_channels > 0 {
            let nc = self.frame.num_channels as f32;
            for sample in &mut self.frame.mono {
                *sample /= nc;
            }
        }
        self.fft.compute_into(&self.frame.mono, &mut self.frame.fft);
        self.frame.new_sample_count =
            self.scratch.iter().map(|b| b.len()).max().unwrap_or(0);
        true
    }

    pub fn frame(&self) -> &AudioFrame { &self.frame }
    pub fn sample_rate(&self) -> f32 { self.frame.sample_rate }
    pub fn num_channels(&self) -> usize { self.frame.num_channels }
}
