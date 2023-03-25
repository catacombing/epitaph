//! Display orientation lock.

use catacomb_ipc::{self, IpcMessage};

use crate::module::{DrawerModule, Module, Toggle};
use crate::text::Svg;
use crate::Result;

pub struct Orientation {
    locked: bool,
}

impl Orientation {
    pub fn new() -> Self {
        Self { locked: true }
    }
}

impl Module for Orientation {
    fn drawer_module(&mut self) -> Option<DrawerModule> {
        Some(DrawerModule::Toggle(self))
    }
}

impl Toggle for Orientation {
    fn toggle(&mut self) -> Result<()> {
        self.locked = !self.locked;

        let msg = IpcMessage::Orientation { lock: None, unlock: !self.locked };
        catacomb_ipc::send_message(&msg)?;

        Ok(())
    }

    fn svg(&self) -> Svg {
        if self.locked {
            Svg::OrientationLocked
        } else {
            Svg::OrientationUnlocked
        }
    }

    fn enabled(&self) -> bool {
        self.locked
    }
}
