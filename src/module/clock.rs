//! Nice clock.

use std::time::{Duration, UNIX_EPOCH};

use calloop::LoopHandle;
use calloop::timer::{TimeoutAction, Timer};
use chrono::offset::Local;

use crate::config::ConfigPanelModule;
use crate::module::{Alignment, Module, PanelModule, PanelModuleContent};
use crate::{Result, State};

pub struct Clock {
    alignment: Alignment,
    format: String,
}

impl Clock {
    pub fn new(
        event_loop: &LoopHandle<'static, State>,
        alignment: Alignment,
        format: String,
    ) -> Result<Self> {
        event_loop.insert_source(Timer::immediate(), move |now, _, state| {
            state.unstall();

            // Calculate seconds until next minute. We add one second just to be sure.
            let total_secs = UNIX_EPOCH.elapsed().unwrap().as_secs();
            let remaining = Duration::from_secs(60 - (total_secs % 60) + 1);

            TimeoutAction::ToInstant(now + remaining)
        })?;

        Ok(Self { alignment, format })
    }
}

impl Module for Clock {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }
}

impl PanelModule for Clock {
    fn alignment(&self) -> Alignment {
        self.alignment
    }

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Text(Local::now().format(&self.format).to_string())
    }

    fn config_variant(&self) -> ConfigPanelModule {
        ConfigPanelModule::Clock
    }
}
