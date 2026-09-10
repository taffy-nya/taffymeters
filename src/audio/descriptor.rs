use std::{error::Error, fmt};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Generation(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelLayout {
    Mono,
    Stereo,
    UnknownDiscrete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioDescriptor {
    pub source_id: SourceId,
    pub generation: Generation,
    pub sample_rate_hz: u32,
    pub channel_count: u16,
    pub channel_layout: ChannelLayout,
}

impl AudioDescriptor {
    pub fn new(
        source_id: SourceId,
        generation: Generation,
        sample_rate_hz: u32,
        channel_count: u16,
        channel_layout: ChannelLayout,
    ) -> Result<Self, DescriptorError> {
        if sample_rate_hz == 0 {
            return Err(DescriptorError::ZeroSampleRate);
        }
        if channel_count == 0 {
            return Err(DescriptorError::ZeroChannels);
        }
        match channel_layout {
            ChannelLayout::Mono if channel_count != 1 => {
                return Err(DescriptorError::LayoutChannelMismatch);
            }
            ChannelLayout::Stereo if channel_count != 2 => {
                return Err(DescriptorError::LayoutChannelMismatch);
            }
            _ => {}
        }
        Ok(Self {
            source_id,
            generation,
            sample_rate_hz,
            channel_count,
            channel_layout,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorError {
    ZeroSampleRate,
    ZeroChannels,
    LayoutChannelMismatch,
}

impl fmt::Display for DescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSampleRate => f.write_str("sample rate must be positive"),
            Self::ZeroChannels => f.write_str("channel count must be positive"),
            Self::LayoutChannelMismatch => {
                f.write_str("channel layout does not match channel count")
            }
        }
    }
}

impl Error for DescriptorError {}
