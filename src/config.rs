//! Configuration options.

use std::fmt::{self, Formatter};
use std::time::Duration;

use serde::de::Visitor;
use serde::{Deserialize, Deserializer};

#[derive(Deserialize, Default, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: Font,
    pub colors: Colors,
    pub input: Input,
}

/// Font configuration.
#[derive(Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Font {
    /// Font family.
    pub family: String,
    /// Font size.
    pub size: f32,
}

impl Default for Font {
    fn default() -> Self {
        Self { family: "sans".into(), size: 12. }
    }
}

/// Color configuration.
#[derive(Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    /// Background color.
    pub bg: Color,

    // Active module background.
    pub module_active: Color,
    /// Inactive module background.
    pub module_inactive: Color,

    /// Volume overlay background.
    pub volume_bg: Color,
    /// Volume overlay background when over 100%.
    pub volume_bad_bg: Color,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            bg: Color::new(24, 24, 24),

            module_active: Color::new(85, 85, 85),
            module_inactive: Color::new(51, 51, 51),

            volume_bg: Color::new(117, 42, 42),
            volume_bad_bg: Color::new(255, 0, 0),
        }
    }
}

/// Input configuration.
#[derive(Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    /// Square of the maximum distance before touch input is considered a drag.
    pub max_tap_distance: f64,

    /// Maximum time between taps to be considered a double-tap.
    #[serde(deserialize_with = "duration_ms")]
    pub multi_tap_interval: Duration,
}

impl Default for Input {
    fn default() -> Self {
        Self { multi_tap_interval: Duration::from_millis(200), max_tap_distance: 400. }
    }
}

/// RGB color.
#[derive(Copy, Clone, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn as_u8(&self) -> [u8; 4] {
        [self.r, self.g, self.b, 255]
    }

    pub const fn as_f32(&self) -> [f32; 3] {
        [self.r as f32 / 255., self.g as f32 / 255., self.b as f32 / 255.]
    }
}

/// Deserialize rgb color from a hex string.
impl<'de> Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorVisitor;

        impl Visitor<'_> for ColorVisitor {
            type Value = Color;

            fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str("hex color like #ff00ff")
            }

            fn visit_str<E>(self, value: &str) -> Result<Color, E>
            where
                E: serde::de::Error,
            {
                let channels = match value.strip_prefix('#') {
                    Some(channels) => channels,
                    None => {
                        return Err(E::custom(format!("color {value:?} is missing leading '#'")));
                    },
                };

                let digits = channels.len();
                if digits != 6 {
                    let msg = format!("color {value:?} has {digits} digits; expected 6");
                    return Err(E::custom(msg));
                }

                match u32::from_str_radix(channels, 16) {
                    Ok(mut color) => {
                        let b = (color & 0xFF) as u8;
                        color >>= 8;
                        let g = (color & 0xFF) as u8;
                        color >>= 8;
                        let r = color as u8;

                        Ok(Color::new(r, g, b))
                    },
                    Err(_) => Err(E::custom(format!("color {value:?} contains non-hex digits"))),
                }
            }
        }

        deserializer.deserialize_str(ColorVisitor)
    }
}

/// Deserialize rgb color from a hex string.
fn duration_ms<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let ms = u64::deserialize(deserializer)?;
    Ok(Duration::from_millis(ms))
}
