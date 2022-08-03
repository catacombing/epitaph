//! Panel modules.

use crate::panel::ModuleRun;
use crate::text::Svg;

pub mod battery;
pub mod cellular;
pub mod clock;
pub mod wifi;

/// Panel module.
pub trait Module {
    /// Alignment for panel modules.
    fn alignment(&self) -> Option<Alignment> {
        None
    }

    /// Insert module into panel.
    fn panel_insert(&self, _run: &mut ModuleRun) {}

    /// Drawer button content.
    fn drawer_button(&self) -> Option<(Svg, bool)> {
        None
    }

    /// Toggle module state.
    fn toggle(&mut self) {}
}

/// Module alignment.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Alignment {
    Center,
    Right,
}
