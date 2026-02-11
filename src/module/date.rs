//! Current date.

use chrono::offset::Local;

use crate::config::ConfigPanelModule;
use crate::module::{Alignment, Module, PanelModule, PanelModuleContent};

pub struct Date {
    alignment: Alignment,
    format: String,
}

impl Date {
    pub fn new(alignment: Alignment, format: String) -> Self {
        Self { alignment, format }
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

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Text(Local::now().format(&self.format).to_string())
    }

    fn config_variant(&self) -> ConfigPanelModule {
        ConfigPanelModule::Date
    }
}
