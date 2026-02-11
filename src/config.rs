//! Configuration options.

use std::fmt::{self, Display, Formatter};
use std::ops::Deref;
use std::time::Duration;

use configory::docgen::{DocType, Docgen, Leaf};
use serde::de::Visitor;
use serde::{Deserialize, Deserializer};

/// # Epitaph
///
/// ## Syntax
///
/// Epitaph's configuration file uses the TOML format. The format's
/// specification can be found at _<https://toml.io/en/v1.0.0>_.
///
/// ## Location
///
/// Epitaph doesn't create the configuration file for you, but it looks for one
/// at <br> `${XDG_CONFIG_HOME:-$HOME/.config}/epitaph/epitaph.toml`.
///
/// ## Fields
#[derive(Docgen, Deserialize, Default, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub font: Font,
    pub colors: Colors,
    pub input: Input,
    pub geometry: Geometry,
    pub modules: Modules,
}

/// Font configuration.
#[derive(Docgen, Deserialize, Debug)]
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
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    /// Background color.
    #[serde(alias = "bg")]
    pub background: Color,

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
            background: Color::new(24, 24, 24),

            module_active: Color::new(85, 85, 85),
            module_inactive: Color::new(51, 51, 51),

            volume_bg: Color::new(117, 42, 42),
            volume_bad_bg: Color::new(255, 0, 0),
        }
    }
}

/// Input configuration.
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Input {
    /// Square of the maximum distance before touch input is considered a drag.
    pub max_tap_distance: f64,

    /// Maximum time between taps to be considered a double-tap.
    #[docgen(doc_type = "integer (milliseconds)", default = "750")]
    pub multi_tap_interval: MillisDuration,
}

impl Default for Input {
    fn default() -> Self {
        Self { multi_tap_interval: Duration::from_millis(200).into(), max_tap_distance: 400. }
    }
}

/// Panel geometry.
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Geometry {
    /// Height of the panel in pixels at scale 1.
    pub height: u32,
    /// Panel padding at the screen corners.
    pub padding: u32,
}

impl Default for Geometry {
    fn default() -> Self {
        Self { height: 20, padding: 5 }
    }
}

/// Panel modules.
#[derive(Docgen, Deserialize, Debug)]
#[serde(default, deny_unknown_fields)]
pub struct Modules {
    /// Left-aligned panel modules.
    pub left: Vec<ConfigPanelModule>,
    /// Center-aligned panel modules.
    pub center: Vec<ConfigPanelModule>,
    /// Right-aligned panel modules.
    pub right: Vec<ConfigPanelModule>,

    /// Format for the clock module.
    pub clock_format: String,
    /// Format for the date module.
    pub date_format: String,
}

impl Default for Modules {
    fn default() -> Self {
        Self {
            left: vec![ConfigPanelModule::Date],
            center: vec![ConfigPanelModule::Clock],
            right: vec![
                ConfigPanelModule::Cellular,
                ConfigPanelModule::Wifi,
                ConfigPanelModule::Battery,
            ],
            date_format: "%a. %-d".into(),
            clock_format: "%H:%M".into(),
        }
    }
}

/// Panel modules.
#[derive(Docgen, Deserialize, PartialEq, Eq, Copy, Clone, Debug)]
#[docgen(doc_type = "\"Cellular\" | \"Battery\" | \"Clock\" | \"Wifi\" | \"Date\"")]
pub enum ConfigPanelModule {
    Cellular,
    Battery,
    Clock,
    Wifi,
    Date,
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

impl Docgen for Color {
    fn doc_type() -> DocType {
        DocType::Leaf(Leaf::new("color"))
    }

    fn format(&self) -> String {
        format!("\"#{:0>2x}{:0>2x}{:0>2x}\"", self.r, self.g, self.b)
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

/// Config wrapper for millisecond-precision durations.
#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
pub struct MillisDuration(Duration);

impl Deref for MillisDuration {
    type Target = Duration;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MillisDuration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let ms = u64::deserialize(deserializer)?;
        Ok(Duration::from_millis(ms).into())
    }
}

impl From<Duration> for MillisDuration {
    fn from(duration: Duration) -> Self {
        Self(duration)
    }
}

impl Display for MillisDuration {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(f, "{}", self.0.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use configory::docgen::markdown::Markdown;

    use super::*;

    #[test]
    fn config_docs() {
        let mut formatter = Markdown::new();
        formatter.set_heading_size(3);
        let expected = formatter.format::<Config>();

        // Uncomment to update config documentation.
        // fs::write("./docs/config.md", &expected).unwrap();

        // Ensure documentation is up to date.
        let docs = fs::read_to_string("./docs/config.md").unwrap();
        assert_eq!(docs, expected);
    }
}
