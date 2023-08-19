//! Cellular status and signal strength.

use calloop::channel::Event;
use calloop::LoopHandle;

use crate::dbus::modem_manager::{self, ModemConnection};
use crate::module::{Alignment, DrawerModule, Module, PanelModule, PanelModuleContent, Toggle};
use crate::text::Svg;
use crate::{Result, State};

pub struct Cellular {
    /// Current connection state.
    connection: ModemConnection,

    /// Desired connectivity state.
    desired_enabled: bool,
}

impl Cellular {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        // Subscribe to ModemManager DBus events.
        let rx = modem_manager::modem_listener()?;
        event_loop.insert_source(rx, move |event, _, state| {
            let connection = match event {
                Event::Msg(connection) => connection,
                Event::Closed => return,
            };

            let old_svg = state.modules.cellular.svg();

            // Update connection status.
            state.modules.cellular.connection = connection;

            // Request redraw only if SVG changed.
            if old_svg != state.modules.cellular.svg() {
                state.request_frame();
            }
        })?;

        Ok(Self { connection: ModemConnection::default(), desired_enabled: true })
    }
}

impl Module for Cellular {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }

    fn drawer_module(&mut self) -> Option<DrawerModule> {
        Some(DrawerModule::Toggle(self))
    }
}

impl PanelModule for Cellular {
    fn alignment(&self) -> Alignment {
        Alignment::Right
    }

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Svg(self.svg())
    }
}

impl Toggle for Cellular {
    fn toggle(&mut self) -> Result<()> {
        self.desired_enabled = !self.desired_enabled;
        modem_manager::set_enabled(self.desired_enabled);
        Ok(())
    }

    /// Current cellular status SVG.
    fn svg(&self) -> Svg {
        if !self.connection.enabled {
            return Svg::CellularDisabled;
        }

        if !self.connection.registered {
            return Svg::Cellular0;
        }

        match self.connection.strength {
            90.. => Svg::Cellular100,
            70.. => Svg::Cellular80,
            50.. => Svg::Cellular60,
            30.. => Svg::Cellular40,
            10.. => Svg::Cellular20,
            _ => Svg::Cellular0,
        }
    }

    fn enabled(&self) -> bool {
        self.desired_enabled
    }
}
