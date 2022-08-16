//! Panel modules.

use crate::text::Svg;
use crate::Result;

pub mod battery;
pub mod brightness;
pub mod cellular;
pub mod clock;
pub mod flashlight;
pub mod wifi;

/// Panel module.
pub trait Module {
    /// Panel module implementation.
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        None
    }

    /// Drawer module implementation.
    fn drawer_module(&mut self) -> Option<DrawerModule> {
        None
    }
}

/// Module alignment.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Alignment {
    Center,
    Right,
}

/// Module in the panel.
pub trait PanelModule {
    /// Module alignment.
    fn alignment(&self) -> Alignment;

    /// Renderable panel content.
    fn content(&self) -> PanelModuleContent;
}

/// Panel module renderable.
pub enum PanelModuleContent {
    Text(String),
    Svg(Svg),
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

    /// Get current slider value.
    fn get_value(&self) -> f64;

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
