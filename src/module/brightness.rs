//! Screen brightness.

use std::str::FromStr;

use udev::Enumerator;

use crate::Result;
use crate::module::{DrawerModule, Module, Slider};
use crate::text::Svg;

pub struct Brightness {
    brightness: f64,
}

impl Brightness {
    pub fn new() -> Result<Self> {
        Ok(Self { brightness: Self::get_brightness()? })
    }

    /// Get device backlight brightness.
    fn get_brightness() -> Result<f64> {
        // Get all backlight devices.
        let mut enumerator = Enumerator::new()?;
        enumerator.match_subsystem("backlight")?;
        let devices = enumerator.scan_devices()?;

        // Find first device with `actual_brightness` and `max_brightness` attributes.
        let brightness = devices.into_iter().find_map(|device| {
            let brightness = device
                .attribute_value("actual_brightness")
                .and_then(|brightness| u32::from_str(&brightness.to_string_lossy()).ok());

            let max_brightness = device
                .attribute_value("max_brightness")
                .and_then(|max_brightness| u32::from_str(&max_brightness.to_string_lossy()).ok());

            brightness.zip(max_brightness)
        });

        Ok(brightness
            .map(|(brightness, max_brightness)| brightness as f64 / max_brightness as f64)
            .unwrap_or(1.))
    }
}

impl Module for Brightness {
    fn drawer_module(&mut self) -> Option<DrawerModule<'_>> {
        Some(DrawerModule::Slider(self))
    }
}

impl Slider for Brightness {
    /// Set device backlight brightness.
    fn set_value(&mut self, mut value: f64) -> Result<()> {
        // Convert to nearest multiple of .05.
        value = (value * 20.).round() / 20.;

        // Get all backlight devices.
        let mut enumerator = Enumerator::new()?;
        enumerator.match_subsystem("backlight")?;
        let mut devices = enumerator.scan_devices()?;

        for mut device in &mut devices {
            let max_brightness = match device
                .attribute_value("max_brightness")
                .and_then(|max_brightness| u32::from_str(&max_brightness.to_string_lossy()).ok())
            {
                Some(brightness) => brightness,
                None => continue,
            };

            // Calculate target brightness integer value.
            let brightness = ((max_brightness as f64 * value) as u32).max(1);

            // Update screen brightness.
            let _ = device.set_attribute_value("brightness", brightness.to_string());
        }

        // Update internal brightness value.
        self.brightness = value;

        Ok(())
    }

    fn value(&self) -> f64 {
        self.brightness
    }

    fn svg(&self) -> Svg {
        Svg::Brightness
    }
}
