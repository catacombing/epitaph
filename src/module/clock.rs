//! Nice clock.

use chrono::offset::Local;

use crate::module::{Alignment, Module};
use crate::panel::ModuleRun;

pub struct Clock;

impl Module for Clock {
    fn alignment(&self) -> Option<Alignment> {
        Some(Alignment::Center)
    }

    fn panel_insert(&self, run: &mut ModuleRun) {
        let time = Local::now();
        run.batch_string(&time.format("%H:%M").to_string());
    }
}
