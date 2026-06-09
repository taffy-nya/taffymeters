pub struct LogSpectrumMapper {
    pub bands: usize,
    pub min_freq: f32,
    pub max_freq: f32,
}

impl LogSpectrumMapper {
    pub fn new(bands: usize) -> Self {
        Self { bands, min_freq: 20.0, max_freq: 20_000.0 }
    }

    pub fn map(&self, fft: &[f32], sample_rate: f32) -> Vec<f32> {
        let mut out = Vec::new();
        self.map_into(fft, sample_rate, &mut out);
        out
    }

    pub fn map_into(&self, fft: &[f32], sample_rate: f32, out: &mut Vec<f32>) {
        out.clear();
        out.reserve(self.bands);

        if fft.is_empty() {
            out.resize(self.bands, 0.0);
            return;
        }

        let hz_per_bin = sample_rate / (fft.len() * 2) as f32;

        out.extend((0..self.bands).map(|i| {
            let t0 = i as f32 / self.bands as f32;
            let t1 = (i + 1) as f32 / self.bands as f32;
            let f0 = self.min_freq * (self.max_freq / self.min_freq).powf(t0);
            let f1 = self.min_freq * (self.max_freq / self.min_freq).powf(t1);
            let b0 = f0 / hz_per_bin;
            let b1 = f1 / hz_per_bin;
            Self::avg_bins(fft, b0, b1)
        }));
    }

    pub fn to_db(amp: f32) -> f32 {
        (amp * 200.0 + 1.0).log10().max(0.0)
    }

    fn avg_bins(data: &[f32], b0: f32, b1: f32) -> f32 {
        if b1 - b0 < 1.0 {
            Self::interpolate(data, (b0 + b1) / 2.0)
        } else {
            let start = b0.ceil() as usize;
            let end = (b1.floor() as usize).min(data.len());
            if start < end {
                let sum: f32 = data[start..end].iter().sum();
                sum / (end - start) as f32
            } else {
                Self::interpolate(data, (b0 + b1) / 2.0)
            }
        }
    }

    fn interpolate(data: &[f32], idx: f32) -> f32 {
        let i0 = idx.floor() as usize;
        let i1 = i0 + 1;
        if i0 >= data.len() { return 0.0; }
        if i1 >= data.len() { return data[i0]; }
        let t = idx - i0 as f32;
        data[i0] * (1.0 - t) + data[i1] * t
    }
}
