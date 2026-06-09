#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChannelMode {
    Mono,
    Left,
    Right,
}

impl ChannelMode {
    pub fn label(&self) -> &'static str {
        match self {
            ChannelMode::Mono => "Mono",
            ChannelMode::Left => "Left",
            ChannelMode::Right => "Right",
        }
    }
}
