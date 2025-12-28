//! GPS toggle.

use calloop::LoopHandle;
use calloop::channel::Event;

use crate::dbus::modem_manager;
use crate::module::{DrawerModule, Module, PanelModule, Toggle};
use crate::text::Svg;
use crate::{Result, State};

pub struct Gps {
    /// Current GPS state.
    enabled: bool,

    /// Desired connectivity state.
    desired_enabled: bool,
}

impl Gps {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        // Subscribe to modem GPS DBus events.
        let rx = modem_manager::gps_listener();
        event_loop.insert_source(rx, move |event, _, state| {
            let enabled = match event {
                Event::Msg(enabled) => enabled,
                Event::Closed => return,
            };

            let module = &mut state.modules.gps;
            if enabled != module.enabled {
                module.desired_enabled = enabled;
                module.enabled = enabled;
                state.unstall();
            }
        })?;

        Ok(Self { enabled: false, desired_enabled: false })
    }
}

impl Module for Gps {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        None
    }

    fn drawer_module(&mut self) -> Option<DrawerModule<'_>> {
        Some(DrawerModule::Toggle(self))
    }
}

impl Toggle for Gps {
    fn toggle(&mut self) -> Result<()> {
        self.desired_enabled = !self.desired_enabled;
        modem_manager::set_gps_enabled(self.desired_enabled);
        Ok(())
    }

    fn svg(&self) -> Svg {
        if self.enabled { Svg::GpsOn } else { Svg::GpsOff }
    }

    #[allow(clippy::misnamed_getters)]
    fn enabled(&self) -> bool {
        self.desired_enabled
    }
}
