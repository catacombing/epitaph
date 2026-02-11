//! Current date.

use chrono::offset::Local;

use crate::config::ConfigPanelModule;
use crate::module::{Alignment, Module, PanelModule, PanelModuleContent};

pub struct Date {
    alignment: Alignment,
}

impl Date {
    pub fn new(alignment: Alignment) -> Self {
        Self { alignment }
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
        PanelModuleContent::Text(Local::now().format("%a. %-d").to_string())
    }

    fn config_variant(&self) -> ConfigPanelModule {
        ConfigPanelModule::Date
    }
}
