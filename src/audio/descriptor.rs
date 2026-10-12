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

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(
        sample_rate_hz: u32,
        channel_count: u16,
        layout: ChannelLayout,
    ) -> Result<AudioDescriptor, DescriptorError> {
        AudioDescriptor::new(
            SourceId(1),
            Generation(2),
            sample_rate_hz,
            channel_count,
            layout,
        )
    }

    #[test]
    fn valid_descriptor_keeps_every_field() {
        let descriptor = descriptor(48_000, 2, ChannelLayout::Stereo).unwrap();

        assert_eq!(descriptor.source_id, SourceId(1));
        assert_eq!(descriptor.generation, Generation(2));
        assert_eq!(descriptor.sample_rate_hz, 48_000);
        assert_eq!(descriptor.channel_count, 2);
        assert_eq!(descriptor.channel_layout, ChannelLayout::Stereo);
    }

    #[test]
    fn zero_sample_rate_is_rejected_for_every_layout() {
        assert_eq!(
            descriptor(0, 1, ChannelLayout::Mono),
            Err(DescriptorError::ZeroSampleRate)
        );
        assert_eq!(
            descriptor(0, 2, ChannelLayout::Stereo),
            Err(DescriptorError::ZeroSampleRate)
        );
        assert_eq!(
            descriptor(0, 6, ChannelLayout::UnknownDiscrete),
            Err(DescriptorError::ZeroSampleRate)
        );
    }

    #[test]
    fn zero_channels_are_rejected_for_every_layout() {
        for layout in [
            ChannelLayout::Mono,
            ChannelLayout::Stereo,
            ChannelLayout::UnknownDiscrete,
        ] {
            assert_eq!(
                descriptor(48_000, 0, layout),
                Err(DescriptorError::ZeroChannels),
                "{layout:?}"
            );
        }
    }

    #[test]
    fn mono_layout_requires_exactly_one_channel() {
        assert!(descriptor(44_100, 1, ChannelLayout::Mono).is_ok());
        assert_eq!(
            descriptor(44_100, 2, ChannelLayout::Mono),
            Err(DescriptorError::LayoutChannelMismatch)
        );
        assert_eq!(
            descriptor(44_100, 6, ChannelLayout::Mono),
            Err(DescriptorError::LayoutChannelMismatch)
        );
    }

    #[test]
    fn stereo_layout_requires_exactly_two_channels() {
        assert!(descriptor(44_100, 2, ChannelLayout::Stereo).is_ok());
        assert_eq!(
            descriptor(44_100, 1, ChannelLayout::Stereo),
            Err(DescriptorError::LayoutChannelMismatch)
        );
        assert_eq!(
            descriptor(44_100, 3, ChannelLayout::Stereo),
            Err(DescriptorError::LayoutChannelMismatch)
        );
    }

    #[test]
    fn unknown_discrete_layout_accepts_any_positive_channel_count() {
        for channels in [1, 2, 3, 8, u16::MAX] {
            let descriptor = descriptor(48_000, channels, ChannelLayout::UnknownDiscrete).unwrap();

            assert_eq!(descriptor.channel_count, channels);
        }
    }
}
