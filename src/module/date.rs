//! Current date.

use std::sync::Arc;

use chrono::offset::Local;

use crate::config::ConfigPanelModule;
use crate::module::{Alignment, Module, PanelModule, PanelModuleContent};

pub struct Date {
    alignment: Alignment,
    format: Arc<String>,
}

impl Date {
    pub fn new(alignment: Alignment, format: Arc<String>) -> Self {
        Self { alignment, format }
    }

    /// Update the date format string.
    pub fn set_format(&mut self, format: Arc<String>) {
        self.format = format;
    }
}

impl Module for Date {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }
}

impl PanelModule for Date {
    fn alignment(&self) -> Alignment {
        self.alignment
    }

    fn set_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
    }

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Text(Local::now().format(&self.format).to_string())
    }

    fn config_variant(&self) -> ConfigPanelModule {
        ConfigPanelModule::Date
    }
}
