use std::{error::Error, fmt};

use crate::audio::{AudioDescriptor, ChannelLayout};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChannelSelection {
    Left,
    Right,
    MonoAverage,
    Discrete(u16),
}

impl ChannelSelection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Right => "Right",
            Self::MonoAverage => "Mono mix",
            Self::Discrete(_) => "Discrete channel",
        }
    }

    pub(crate) fn resolve(
        self,
        descriptor: &AudioDescriptor,
    ) -> Result<ResolvedChannel, ChannelError> {
        let result = match self {
            Self::MonoAverage => ResolvedChannel::AverageAll,
            Self::Discrete(index) if index < descriptor.channel_count => {
                ResolvedChannel::Index(index as usize)
            }
            Self::Discrete(index) => {
                return Err(ChannelError::MissingDiscreteChannel {
                    requested: index,
                    available: descriptor.channel_count,
                });
            }
            Self::Left if descriptor.channel_layout == ChannelLayout::Stereo => {
                ResolvedChannel::Index(0)
            }
            Self::Right if descriptor.channel_layout == ChannelLayout::Stereo => {
                ResolvedChannel::Index(1)
            }
            Self::Left | Self::Right
                if descriptor.channel_layout == ChannelLayout::UnknownDiscrete =>
            {
                return Err(ChannelError::UnknownSpeakerLayout);
            }
            Self::Left => return Err(ChannelError::MissingSpeaker("Left")),
            Self::Right => return Err(ChannelError::MissingSpeaker("Right")),
        };
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResolvedChannel {
    Index(usize),
    AverageAll,
}

impl ResolvedChannel {
    pub(crate) fn sample(self, frame: &[f32]) -> f32 {
        match self {
            Self::Index(index) => frame[index],
            Self::AverageAll => frame.iter().copied().sum::<f32>() / frame.len() as f32,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelError {
    MissingSpeaker(&'static str),
    UnknownSpeakerLayout,
    MissingDiscreteChannel { requested: u16, available: u16 },
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSpeaker(name) => write!(f, "source has no {name} channel"),
            Self::UnknownSpeakerLayout => {
                f.write_str("speaker layout is unknown; select a discrete channel")
            }
            Self::MissingDiscreteChannel {
                requested,
                available,
            } => write!(
                f,
                "discrete channel {requested} is unavailable (source has {available} channels)"
            ),
        }
    }
}

impl Error for ChannelError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Generation, SourceId};

    fn descriptor(channels: u16, layout: ChannelLayout) -> AudioDescriptor {
        AudioDescriptor::new(SourceId(1), Generation(1), 48_000, channels, layout).unwrap()
    }

    #[test]
    fn stereo_left_and_right_resolve_to_distinct_channels() {
        let descriptor = descriptor(2, ChannelLayout::Stereo);
        let frame = [0.25, -0.75];
        assert_eq!(
            ChannelSelection::Left
                .resolve(&descriptor)
                .unwrap()
                .sample(&frame),
            0.25
        );
        assert_eq!(
            ChannelSelection::Right
                .resolve(&descriptor)
                .unwrap()
                .sample(&frame),
            -0.75
        );
    }

    #[test]
    fn a_missing_right_channel_is_reported_instead_of_using_mono() {
        let descriptor = descriptor(1, ChannelLayout::Mono);
        assert_eq!(
            ChannelSelection::Right.resolve(&descriptor),
            Err(ChannelError::MissingSpeaker("Right"))
        );
    }

    #[test]
    fn unknown_layout_requires_an_explicit_index() {
        let descriptor = descriptor(4, ChannelLayout::UnknownDiscrete);
        assert_eq!(
            ChannelSelection::Left.resolve(&descriptor),
            Err(ChannelError::UnknownSpeakerLayout)
        );
        assert_eq!(
            ChannelSelection::Discrete(3)
                .resolve(&descriptor)
                .unwrap()
                .sample(&[1.0, 2.0, 3.0, 4.0]),
            4.0
        );
    }
}
