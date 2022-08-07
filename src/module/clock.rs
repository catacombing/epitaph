//! Nice clock.

use std::time::{Duration, UNIX_EPOCH};

use calloop::timer::{TimeoutAction, Timer};
use calloop::LoopHandle;
use chrono::offset::Local;

use crate::module::{Alignment, Module};
use crate::panel::ModuleRun;
use crate::{Result, State};

pub struct Clock {
    _new: (),
}

impl Clock {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        event_loop.insert_source(Timer::immediate(), move |now, _, state| {
            state.request_frame();

            // Calculate seconds until next minute.
            let total_secs = UNIX_EPOCH.elapsed().unwrap().as_secs();
            let remaining = Duration::from_secs(60 - (total_secs % 60));

            TimeoutAction::ToInstant(now + remaining)
        })?;

        Ok(Self { _new: () })
    }
}

impl Module for Clock {
    fn alignment(&self) -> Option<Alignment> {
        Some(Alignment::Center)
    }

    fn panel_insert(&self, run: &mut ModuleRun) {
        let time = Local::now();
        run.batch_string(&time.format("%H:%M").to_string());
    }
}
