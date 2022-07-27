//! Nice clock.

use chrono::offset::Local;

use crate::module::{Module, ModuleRun};

pub struct Clock;

impl Module for Clock {
    fn insert(&self, run: &mut ModuleRun) {
        let time = Local::now();
        run.batch_string(&time.format("%H:%M").to_string());
    }
}
