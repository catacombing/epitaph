//! Current date.

use chrono::offset::Local;

use crate::Result;
use crate::module::{Alignment, Module, PanelModule, PanelModuleContent};

pub struct Date;

impl Date {
    pub fn new() -> Result<Self> {
        Ok(Self)
    }
}

impl Module for Date {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }
}

impl PanelModule for Date {
    fn alignment(&self) -> Alignment {
        Alignment::Left
    }

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Text(Local::now().format("%a. %-d").to_string())
    }
}
