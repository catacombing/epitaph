//! Catacomb output scale.

use catacomb_ipc::{self, IpcMessage, WindowScale};

use crate::Result;
use crate::module::{DrawerModule, Module, Slider};
use crate::text::Svg;

pub struct Scale {
    scale: f64,
}

impl Scale {
    pub fn new() -> Self {
        Self { scale: 2. }
    }
}

impl Module for Scale {
    fn drawer_module(&mut self) -> Option<DrawerModule<'_>> {
        Some(DrawerModule::Slider(self))
    }
}

impl Slider for Scale {
    fn set_value(&mut self, value: f64) -> Result<()> {
        // Map from `0..=1` to `1..=3`.
        let mut scale = value * 2. + 1.;

        // Round to nearest multiple of .5.
        scale = (scale * 2.).round() / 2.;

        // Update internal scale value.
        self.scale = scale;

        Ok(())
    }

    fn on_touch_up(&mut self) -> Result<()> {
        // Update Catacomb's scale.
        let msg = IpcMessage::Scale { scale: WindowScale::Fixed(self.scale), app_id: None };
        catacomb_ipc::send_message(&msg)?;
        Ok(())
    }

    fn value(&self) -> f64 {
        // Map back from `1..=3` to `0..=1`.
        (self.scale - 1.) / 2.
    }

    fn svg(&self) -> Svg {
        Svg::Scale
    }
}
