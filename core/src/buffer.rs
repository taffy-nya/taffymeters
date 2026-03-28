use ringbuf::{HeapRb, HeapProd, HeapCons};
use ringbuf::traits::{Split, Producer, Consumer};

pub struct AudioProducer {
    channels: Vec<HeapProd<f32>>,
    pub num_channels: usize,
}

pub struct AudioConsumer {
    channels: Vec<HeapCons<f32>>,
    pub num_channels: usize,
}

impl AudioProducer {
    pub fn push_frame(&mut self, frame: &[f32]) {
        for (ch, &sample) in frame.iter().enumerate() {
            if ch < self.num_channels {
                let _ = self.channels[ch].try_push(sample);
            }
        }
    }
}

impl AudioConsumer {
    pub fn pop_into(&mut self, buffers: &mut [Vec<f32>]) -> bool {
        let mut any_data = false;
        for (ch, buf) in buffers.iter_mut().enumerate() {
            if ch >= self.num_channels { break; }
            let before = buf.len();
            buf.extend(self.channels[ch].pop_iter());
            if buf.len() > before { any_data = true; }
        }
        any_data
    }
}

pub fn create_ring_buffer(capacity: usize, num_channels: usize) -> (AudioProducer, AudioConsumer) {
    let mut prods = Vec::with_capacity(num_channels);
    let mut cons  = Vec::with_capacity(num_channels);

    for _ in 0..num_channels {
        let rb = HeapRb::<f32>::new(capacity);
        let (p, c) = rb.split();
        prods.push(p);
        cons.push(c);
    }

    (
        AudioProducer { channels: prods, num_channels },
        AudioConsumer { channels: cons,  num_channels },
    )
}