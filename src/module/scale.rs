//! Catacomb output scale.

use catacomb_ipc::{self, IpcMessage, WindowScale};

use crate::Result;
use crate::module::{DrawerModule, Module, Slider};
use crate::text::Svg;

pub struct Scale {
    default_scale: f64,
    scale: f64,
}

impl Scale {
    pub fn new() -> Option<Self> {
        let msg = IpcMessage::Scale { scale: None, app_id: None };
        let scale = match catacomb_ipc::send_message(&msg) {
            Ok(Some(IpcMessage::ScaleReply { scale: WindowScale::Fixed(scale) })) => scale,
            _ => return None,
        };
        Some(Self { scale, default_scale: scale })
    }
}

impl Module for Scale {
    fn drawer_module(&mut self) -> Option<DrawerModule<'_>> {
        Some(DrawerModule::Slider(self))
    }
}

impl Slider for Scale {
    fn set_value(&mut self, value: f64) -> Result<()> {
        // Limit scale to within one above/below the default.
        let mut scale = value * 2. + self.default_scale - 1.;

        // Round to nearest multiple of .5.
        scale = (scale * 2.).round() / 2.;

        // Update internal scale value.
        self.scale = scale;

        Ok(())
    }

    fn on_touch_up(&mut self) -> Result<()> {
        // Update Catacomb's scale.
        let msg = IpcMessage::Scale { scale: Some(WindowScale::Fixed(self.scale)), app_id: None };
        catacomb_ipc::send_message(&msg)?;
        Ok(())
    }

    fn value(&self) -> f64 {
        // Map back to within one above/below the default.
        (self.scale - self.default_scale + 1.) / 2.
    }

    fn svg(&self) -> Svg {
        Svg::Scale
    }
}
