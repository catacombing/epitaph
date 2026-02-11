//! Panel modules.

use crate::Result;
use crate::config::{Color, Config, ConfigPanelModule};
use crate::text::Svg;

pub mod battery;
pub mod brightness;
pub mod cellular;
pub mod clock;
pub mod date;
pub mod flashlight;
pub mod gps;
pub mod orientation;
pub mod scale;
pub mod volume;
pub mod wifi;

/// Panel module.
pub trait Module {
    /// Panel module implementation.
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        None
    }

    /// Panel background module implementation.
    fn panel_background_module(&self) -> Option<&dyn PanelBackgroundModule> {
        None
    }

    /// Drawer module implementation.
    fn drawer_module(&mut self) -> Option<DrawerModule<'_>> {
        None
    }
}

/// Module alignment.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

/// Module in the panel.
pub trait PanelModule {
    /// Module alignment.
    fn alignment(&self) -> Alignment;

    /// Renderable panel content.
    fn content(&self) -> PanelModuleContent;

    /// Get the corresponding config enum variant.
    fn config_variant(&self) -> ConfigPanelModule;
}

/// Panel module renderable.
pub enum PanelModuleContent {
    Text(String),
    Svg(Svg),
}

/// Panel background module.
///
/// These modules use the panel background to indicate new activity of a module
/// between 0% and 100% by partially changing the panel's background color.
pub trait PanelBackgroundModule {
    /// Current status between 0.0 and 1.0.
    fn value(&self) -> f64;

    /// Background color.
    fn color(&self, config: &Config) -> Color;
}

/// Module in the drawer.
pub enum DrawerModule<'a> {
    Toggle(&'a mut dyn Toggle),
    Slider(&'a mut dyn Slider),
}

/// Drawer slider module.
pub trait Slider {
    /// Handle slider updates.
    fn set_value(&mut self, value: f64) -> Result<()>;

    /// Handle touch release.
    ///
    /// This can be used to debounce slider updates until the user is done
    /// inputting the desired value.
    fn on_touch_up(&mut self) -> Result<()> {
        Ok(())
    }

    /// Get current slider value.
    fn value(&self) -> f64;

    /// Get symbol for this slider.
    fn svg(&self) -> Svg;
}

/// Drawer toggle button module.
pub trait Toggle {
    /// Toggle button status.
    fn toggle(&mut self) -> Result<()>;

    /// Get button status.
    fn enabled(&self) -> bool;

    /// Get renderable SVG.
    fn svg(&self) -> Svg;
}
