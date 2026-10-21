//! Colors resolve as global preset → workspace edits → Module type style → instance
//! overrides. Instances keep only explicit overrides; the Workspace supplies resolved palettes.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::render::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ColorRole {
    Background,
    Surface,
    Divider,
    Accent,
    Text,
    /// Also used for unavailable content; set globally, never declared by a Module type.
    Muted,
    Guide,
    Primary,
    /// Peak trace or marker (Spectrum curves or marks, Level Meter hold).
    Secondary,
    Low,
    Mid,
    High,
}

impl ColorRole {
    pub const ALL: [Self; 12] = [
        Self::Background,
        Self::Surface,
        Self::Divider,
        Self::Accent,
        Self::Text,
        Self::Muted,
        Self::Guide,
        Self::Primary,
        Self::Secondary,
        Self::Low,
        Self::Mid,
        Self::High,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Background => "Background",
            Self::Surface => "Surface",
            Self::Divider => "Divider",
            Self::Accent => "Accent",
            Self::Text => "Text",
            Self::Muted => "Muted text",
            Self::Guide => "Guide lines",
            Self::Primary => "Primary trace",
            Self::Secondary => "Secondary trace",
            Self::Low => "Level low",
            Self::Mid => "Level middle",
            Self::High => "Level high",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Surface => "surface",
            Self::Divider => "divider",
            Self::Accent => "accent",
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Guide => "guide",
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Low => "low",
            Self::Mid => "mid",
            Self::High => "high",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.id() == id)
    }

    fn index(self) -> usize {
        self as usize
    }
}

impl Serialize for ColorRole {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for ColorRole {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::from_id(&id).ok_or_else(|| de::Error::custom(format!("unknown color role {id:?}")))
    }
}

pub type RoleOverrides = BTreeMap<ColorRole, Color>;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    colors: [Color; ColorRole::ALL.len()],
    /// Logical points.
    pub text_size: f32,
    pub dark: bool,
}

impl Palette {
    pub fn color(&self, role: ColorRole) -> Color {
        self.colors[role.index()]
    }

    fn apply(&mut self, overrides: &RoleOverrides) {
        for (role, color) in overrides {
            self.colors[role.index()] = *color;
        }
    }

    /// Expects an intensity in [0, 1].
    pub fn level(&self, value: f32) -> Color {
        if value < 0.45 {
            self.color(ColorRole::Low)
        } else if value < 0.8 {
            self.color(ColorRole::Mid)
        } else {
            self.color(ColorRole::High)
        }
    }

    /// Input intensity in [0, 1]; values outside that range are clamped.
    pub fn heat(&self, value: f32) -> Color {
        let value = value.clamp(0.0, 1.0);
        let scaled = value * 3.0;
        let (from, to, t) = if scaled < 1.0 {
            (
                self.color(ColorRole::Low).with_alpha(0),
                self.color(ColorRole::Low),
                scaled,
            )
        } else if scaled < 2.0 {
            (
                self.color(ColorRole::Low),
                self.color(ColorRole::Mid),
                scaled - 1.0,
            )
        } else {
            (
                self.color(ColorRole::Mid),
                self.color(ColorRole::High),
                scaled - 2.0,
            )
        };
        let mut color = from.lerp(to, t);
        color.alpha = (value * 255.0) as u8;
        color
    }
}

impl Default for Palette {
    fn default() -> Self {
        ThemePreset::Dark.palette()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ThemePreset {
    Dark,
    Light,
}

impl ThemePreset {
    pub const ALL: [Self; 2] = [Self::Dark, Self::Light];

    pub fn id(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|preset| preset.id() == id)
    }

    pub fn palette(self) -> Palette {
        let (colors, dark) = match self {
            // Catppuccin Mocha, with darker surfaces and backgrounds.
            Self::Dark => (
                [
                    Color::rgb(0x0f, 0x0f, 0x15),       // base
                    Color::rgb(0x1b, 0x1b, 0x24),       // surface0
                    Color::rgb(0x30, 0x30, 0x3d),       // surface2
                    Color::rgb(0x89, 0xb4, 0xfa),       // blue
                    Color::rgb(0xcd, 0xd6, 0xf4),       // text
                    Color::rgb(0x9f, 0xa6, 0xc0),       // subtext0
                    Color::rgba(0x79, 0x7f, 0x95, 110), // overlay1
                    Color::rgb(0x94, 0xe2, 0xd5),       // teal
                    Color::rgba(0xf9, 0xe2, 0xaf, 180), // yellow
                    Color::rgb(0x74, 0xc7, 0xec),       // sapphire
                    Color::rgb(0xcb, 0xa6, 0xf7),       // mauve
                    Color::rgb(0xf3, 0x8b, 0xa8),       // red
                ],
                true,
            ),
            // Catppuccin Latte, the light counterpart of the Dark mapping.
            Self::Light => (
                [
                    Color::rgb(0xef, 0xf1, 0xf5),       // base
                    Color::rgb(0xdc, 0xe0, 0xe8),       // crust
                    Color::rgb(0xbc, 0xc0, 0xcc),       // surface1
                    Color::rgb(0x1e, 0x66, 0xf5),       // blue
                    Color::rgb(0x4c, 0x4f, 0x69),       // text
                    Color::rgb(0x6c, 0x6f, 0x85),       // subtext0
                    Color::rgba(0x7c, 0x7f, 0x93, 140), // overlay2
                    Color::rgb(0x17, 0x92, 0x99),       // teal
                    Color::rgba(0xdf, 0x8e, 0x1d, 200), // yellow
                    Color::rgb(0x20, 0x9f, 0xb5),       // sapphire
                    Color::rgb(0x88, 0x39, 0xef),       // mauve
                    Color::rgb(0xd2, 0x0f, 0x39),       // red
                ],
                false,
            ),
        };
        Palette {
            colors,
            text_size: 11.0,
            dark,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeSettings {
    #[serde(default = "default_theme_preset")]
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_size: Option<f32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: RoleOverrides,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub type_styles: BTreeMap<String, RoleOverrides>,
}

fn default_theme_preset() -> String {
    ThemePreset::Dark.id().to_owned()
}

pub const TEXT_SIZE_RANGE: (f32, f32) = (8.0, 24.0);

impl ThemeSettings {
    pub fn dark() -> Self {
        Self {
            preset: default_theme_preset(),
            ..Self::default()
        }
    }

    pub fn preset(&self) -> ThemePreset {
        ThemePreset::from_id(&self.preset).unwrap_or(ThemePreset::Dark)
    }

    pub fn global_palette(&self) -> Palette {
        let mut palette = self.preset().palette();
        palette.apply(&self.colors);
        if let Some(size) = self.text_size {
            palette.text_size = size.clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
        }
        palette
    }

    pub fn resolve(&self, type_id: &str, overrides: &RoleOverrides) -> Palette {
        let mut palette = self.global_palette();
        if let Some(style) = self.type_styles.get(type_id) {
            palette.apply(style);
        }
        palette.apply(overrides);
        palette
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_overrides_win_and_unset_roles_follow_the_global_theme() {
        let mut theme = ThemeSettings::dark();
        let red = Color::rgb(255, 0, 0);
        let green = Color::rgb(0, 255, 0);
        theme.type_styles.insert(
            "spectrum".into(),
            RoleOverrides::from([(ColorRole::Secondary, green)]),
        );
        let overrides = RoleOverrides::from([(ColorRole::Primary, red)]);
        let palette = theme.resolve("spectrum", &overrides);
        assert_eq!(palette.color(ColorRole::Primary), red);
        assert_eq!(palette.color(ColorRole::Secondary), green);
        assert_eq!(
            palette.color(ColorRole::Low),
            ThemePreset::Dark.palette().color(ColorRole::Low)
        );

        theme.preset = ThemePreset::Light.id().into();
        let palette = theme.resolve("spectrum", &overrides);
        assert_eq!(palette.color(ColorRole::Primary), red, "override kept");
        assert_eq!(
            palette.color(ColorRole::Secondary),
            green,
            "type style kept"
        );
        assert_eq!(
            palette.color(ColorRole::Low),
            ThemePreset::Light.palette().color(ColorRole::Low),
            "unset role follows the new global theme"
        );
        assert!(!palette.dark);
    }

    #[test]
    fn heat_map_passes_through_the_ramp_and_rises_in_opacity() {
        let palette = ThemePreset::Dark.palette();
        assert_eq!(palette.heat(0.0).alpha, 0);
        let low = palette.heat(1.0 / 3.0);
        assert_eq!((low.red, low.green, low.blue), {
            let c = palette.color(ColorRole::Low);
            (c.red, c.green, c.blue)
        });
        let high = palette.heat(1.0);
        let c = palette.color(ColorRole::High);
        assert_eq!(
            (high.red, high.green, high.blue, high.alpha),
            (c.red, c.green, c.blue, 255)
        );
    }

    #[test]
    fn heat_map_blends_low_into_mid_between_one_and_two_thirds() {
        for preset in [ThemePreset::Dark, ThemePreset::Light] {
            let palette = preset.palette();
            let low = palette.color(ColorRole::Low);
            let mid = palette.color(ColorRole::Mid);
            let channels = |color: Color| [color.red, color.green, color.blue];
            let (low_rgb, mid_rgb) = (channels(low), channels(mid));
            let distance = |color: Color, to: [u8; 3]| -> u32 {
                channels(color)
                    .iter()
                    .zip(to)
                    .map(|(a, b)| u32::from(a.abs_diff(b)))
                    .sum()
            };

            let halfway = palette.heat(0.5);
            for ((got, from), to) in channels(halfway).into_iter().zip(low_rgb).zip(mid_rgb) {
                let middle = (i32::from(from) + i32::from(to)) / 2;
                assert!(
                    (i32::from(got) - middle).abs() <= 1,
                    "{preset:?}: channel {got} is not halfway between {from} and {to}"
                );
            }
            let alphas: Vec<u8> = [0.4, 0.5, 0.6]
                .iter()
                .map(|&value| palette.heat(value).alpha)
                .collect();
            assert!(
                alphas.windows(2).all(|pair| pair[0] < pair[1]),
                "opacity must rise with the value: {alphas:?}"
            );

            let steps = [0.36, 0.42, 0.5, 0.58, 0.64];
            let from_low: Vec<u32> = steps
                .iter()
                .map(|&value| distance(palette.heat(value), low_rgb))
                .collect();
            let to_mid: Vec<u32> = steps
                .iter()
                .map(|&value| distance(palette.heat(value), mid_rgb))
                .collect();
            assert!(
                from_low.windows(2).all(|pair| pair[0] <= pair[1]),
                "{from_low:?}"
            );
            assert!(
                to_mid.windows(2).all(|pair| pair[0] >= pair[1]),
                "{to_mid:?}"
            );
            assert!(from_low[0] < from_low[4] && to_mid[0] > to_mid[4]);

            // Allow one rounding step per RGB channel.
            let end = palette.heat(2.0 / 3.0);
            assert!(
                distance(end, mid_rgb) <= 3,
                "{preset:?}: {end:?} is not Mid"
            );
        }
    }
}
