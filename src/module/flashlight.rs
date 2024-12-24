//! Screen brightness.

use std::ops::{Deref, DerefMut};
use std::str::FromStr;

use udev::{Device, Enumerator};

use crate::module::{DrawerModule, Module, Toggle};
use crate::text::Svg;
use crate::Result;

#[derive(Default)]
pub struct Flashlight {
    enabled: bool,
}

impl Flashlight {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for Flashlight {
    fn drawer_module(&mut self) -> Option<DrawerModule> {
        Some(DrawerModule::Toggle(self))
    }
}

impl Toggle for Flashlight {
    fn toggle(&mut self) -> Result<()> {
        self.enabled = !self.enabled;

        // Get all LED devices.
        let mut enumerator = Enumerator::new()?;
        enumerator.match_subsystem("leds")?;
        enumerator.match_sysname("white:flash")?;
        let devices = enumerator.scan_devices()?;

        // Find any flashlight device.
        let mut flash = match devices.into_iter().find_map(Flash::from_device) {
            Some(flash) => flash,
            None => return Ok(()),
        };

        // Toggle flashlight brightness.
        let new_value = if flash.enabled() { 0 } else { flash.max_brightness };
        flash.set_attribute_value("brightness", new_value.to_string())?;

        Ok(())
    }

    fn svg(&self) -> Svg {
        if self.enabled {
            Svg::FlashlightOn
        } else {
            Svg::FlashlightOff
        }
    }

    fn enabled(&self) -> bool {
        self.enabled
    }
}

/// Flashlight udev device.
struct Flash {
    max_brightness: usize,
    brightness: usize,
    device: Device,
}

impl Flash {
    /// Check if flashlight is on.
    fn enabled(&self) -> bool {
        self.brightness > 0
    }

    /// Convert udev device to flashlight.
    fn from_device(device: Device) -> Option<Flash> {
        let max_brightness_str = device.attribute_value("max_brightness")?.to_string_lossy();
        let max_brightness = usize::from_str(&max_brightness_str).ok()?;
        let brightness_str = device.attribute_value("brightness")?.to_string_lossy();
        let brightness = usize::from_str(&brightness_str).ok()?;

        Some(Self { max_brightness, brightness, device })
    }
}

impl Deref for Flash {
    type Target = Device;

    fn deref(&self) -> &Self::Target {
        &self.device
    }
}

impl DerefMut for Flash {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.device
    }
}
