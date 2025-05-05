//! Configuration options.

/// Font configuration.
pub mod font {
    /// Font description.
    pub const FONT: &str = "Sans";

    /// Font size.
    pub const FONT_SIZE: f32 = 12.;
}

/// Color configuration.
pub mod colors {
    /// Primary background color.
    pub const BG: Color = Color { r: 24, g: 24, b: 24 };

    /// Color of slider handle and active buttons,
    pub const MODULE_ACTIVE: Color = Color { r: 85, g: 85, b: 85 };

    /// Color of the slider tray and inactive buttons.
    pub const MODULE_INACTIVE: Color = Color { r: 51, g: 51, b: 51 };

    /// RGB color.
    #[derive(Copy, Clone)]
    pub struct Color {
        pub r: u8,
        pub g: u8,
        pub b: u8,
    }

    impl Color {
        pub const fn as_u8(&self) -> [u8; 4] {
            [self.r, self.g, self.b, 255]
        }

        pub const fn as_f32(&self) -> [f32; 3] {
            [self.r as f32 / 255., self.g as f32 / 255., self.b as f32 / 255.]
        }
    }
}

/// Input configuration.
pub mod input {
    use std::time::Duration;

    /// Square of the maximum distance before touch input is considered a drag.
    pub const MAX_TAP_DISTANCE: f64 = 400.;

    /// Maximum time between taps to be considered a double-tap.
    pub const MAX_DOUBLE_TAP_DURATION: Duration = Duration::from_millis(200);
}
