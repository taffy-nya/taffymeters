use ringbuf::{HeapRb, HeapProd, HeapCons};
use ringbuf::traits::Split;

pub type AudioProducer = HeapProd<f32>;
pub type AudioConsumer = HeapCons<f32>;

pub fn create_ring_buffer(capacity: usize) -> (AudioProducer, AudioConsumer) {
    let rb = HeapRb::<f32>::new(capacity);
    rb.split()
}