use cpal;

#[derive(Debug)]
pub enum AudioError {
    NoOutputDevice,
    DefaultConfig(cpal::DefaultStreamConfigError),
    BuildStream(cpal::BuildStreamError),
    PlayStream(cpal::PlayStreamError),
    Stream(cpal::StreamError),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AudioError::NoOutputDevice => write!(f, "No default output device found"),
            AudioError::DefaultConfig(e) => write!(f, "Failed to get default output configuration: {e}"),
            AudioError::BuildStream(e) => write!(f, "Failed to build audio stream: {e}"),
            AudioError::PlayStream(e) => write!(f, "Failed to play audio stream: {e}"),
            AudioError::Stream(e) => write!(f, "Audio stream error: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}
