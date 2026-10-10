//! Theme: named color roles, a base text size and the UI tone. Colors resolve as
//! global preset → workspace edits of the global roles → Module type style →
//! instance overrides. Instances never copy the whole theme; they keep only their
//! explicit overrides, and the Workspace hands each one its resolved [`Palette`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::render::Color;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColorRole {
    /// Window and plot background.
    Background,
    /// Panels drawn on the background (meter troughs).
    Surface,
    /// Layout dividers.
    Divider,
    /// Selection, hover and group highlights.
    Accent,
    /// Readouts.
    Text,
    /// Secondary text.
    Muted,
    /// Reference lines (zero line, axes).
    Guide,
    /// Main data trace.
    Primary,
    /// Secondary trace (peak curve).
    Secondary,
    /// Intensity ramp: low, middle and high levels (bars, heat map, meters).
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

    fn index(self) -> usize {
        self as usize
    }
}

/// Explicit color choices of one scope (global edits, a type style or an instance).
pub type RoleOverrides = BTreeMap<ColorRole, Color>;

/// A fully resolved set of colors; copied into each Module instance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    colors: [Color; ColorRole::ALL.len()],
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

    /// Intensity ramp for `value` in [0, 1]: Low below 0.45, Mid below 0.8, High.
    pub fn level(&self, value: f32) -> Color {
        if value < 0.45 {
            self.color(ColorRole::Low)
        } else if value < 0.8 {
            self.color(ColorRole::Mid)
        } else {
            self.color(ColorRole::High)
        }
    }

    /// Continuous heat map: transparent → Low → Mid → High, opacity rising with
    /// `value` in [0, 1].
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
        Preset::Dark.palette()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    Dark,
    Light,
}

impl Preset {
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
            Self::Dark => (
                [
                    Color::rgb(13, 18, 24),
                    Color::rgba(20, 28, 34, 220),
                    Color::rgb(46, 56, 68),
                    Color::rgb(90, 160, 230),
                    Color::rgb(235, 240, 245),
                    Color::rgb(150, 160, 170),
                    Color::rgba(110, 130, 145, 90),
                    Color::rgb(92, 230, 199),
                    Color::rgba(255, 210, 100, 180),
                    Color::rgb(80, 180, 240),
                    Color::rgb(165, 90, 245),
                    Color::rgb(255, 70, 120),
                ],
                true,
            ),
            Self::Light => (
                [
                    Color::rgb(244, 246, 248),
                    Color::rgba(214, 220, 226, 230),
                    Color::rgb(190, 198, 206),
                    Color::rgb(30, 110, 200),
                    Color::rgb(25, 30, 36),
                    Color::rgb(95, 105, 115),
                    Color::rgba(90, 105, 120, 110),
                    Color::rgb(0, 140, 120),
                    Color::rgba(210, 120, 0, 200),
                    Color::rgb(40, 120, 210),
                    Color::rgb(130, 60, 200),
                    Color::rgb(220, 40, 90),
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

/// The theme part of a workspace: which preset, the workspace's edits of the
/// global roles, the base text size and the per-Module-type styles.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeSettings {
    #[serde(default = "default_preset")]
    pub preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_size: Option<f32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub colors: RoleOverrides,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub type_styles: BTreeMap<String, RoleOverrides>,
}

fn default_preset() -> String {
    Preset::Dark.id().to_owned()
}

/// Smallest and largest base text size the theme accepts.
pub const TEXT_SIZE_RANGE: (f32, f32) = (8.0, 24.0);

impl ThemeSettings {
    pub fn dark() -> Self {
        Self {
            preset: default_preset(),
            ..Self::default()
        }
    }

    /// The preset in use; unknown presets (a newer build's) fall back to Dark.
    pub fn preset(&self) -> Preset {
        Preset::from_id(&self.preset).unwrap_or(Preset::Dark)
    }

    /// Global roles: preset plus the workspace's edits.
    pub fn global_palette(&self) -> Palette {
        let mut palette = self.preset().palette();
        palette.apply(&self.colors);
        if let Some(size) = self.text_size {
            palette.text_size = size.clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
        }
        palette
    }

    /// Colors of one instance of `type_id` with its explicit `overrides`.
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
            Preset::Dark.palette().color(ColorRole::Low)
        );

        theme.preset = Preset::Light.id().into();
        let palette = theme.resolve("spectrum", &overrides);
        assert_eq!(palette.color(ColorRole::Primary), red, "override kept");
        assert_eq!(
            palette.color(ColorRole::Secondary),
            green,
            "type style kept"
        );
        assert_eq!(
            palette.color(ColorRole::Low),
            Preset::Light.palette().color(ColorRole::Low),
            "unset role follows the new global theme"
        );
        assert!(!palette.dark);
    }

    #[test]
    fn heat_map_passes_through_the_ramp_and_rises_in_opacity() {
        let palette = Preset::Dark.palette();
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
}
