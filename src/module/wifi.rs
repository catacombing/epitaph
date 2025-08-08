//! WiFi status and signal strength.

use calloop::LoopHandle;
use calloop::channel::Event;

use crate::dbus::network_manager::{self, WifiConnection};
use crate::module::{Alignment, DrawerModule, Module, PanelModule, PanelModuleContent, Toggle};
use crate::text::Svg;
use crate::{Result, State};

#[derive(Debug)]
pub struct Wifi {
    /// Current connection state.
    connection: WifiConnection,

    /// Desired connectivity state.
    desired_enabled: bool,
}

impl Wifi {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        // Subscribe to NetworkManager DBus events.
        let rx = network_manager::wifi_listener()?;
        event_loop.insert_source(rx, move |event, _, state| {
            let connection = match event {
                Event::Msg(connection) => connection,
                Event::Closed => return,
            };

            // Ignore updates that change nothing.
            let module = &mut state.modules.wifi;
            if connection == module.connection {
                return;
            }

            let old_enabled = module.desired_enabled;
            let old_svg = module.svg();

            // Update connection status.
            module.desired_enabled = connection.enabled;
            module.connection = connection;

            // Request redraw only if SVG changed.
            if old_svg != state.modules.wifi.svg() || old_enabled != connection.enabled {
                state.request_frame();
            }
        })?;

        Ok(Self { connection: WifiConnection::default(), desired_enabled: false })
    }
}

impl Module for Wifi {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }

    fn drawer_module(&mut self) -> Option<DrawerModule<'_>> {
        Some(DrawerModule::Toggle(self))
    }
}

impl PanelModule for Wifi {
    fn alignment(&self) -> Alignment {
        Alignment::Right
    }

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Svg(self.svg())
    }
}

impl Toggle for Wifi {
    fn toggle(&mut self) -> Result<()> {
        self.desired_enabled = !self.desired_enabled;
        network_manager::set_enabled(self.desired_enabled);
        Ok(())
    }

    /// Current wifi status SVG.
    fn svg(&self) -> Svg {
        if !self.connection.enabled {
            return Svg::WifiDisabled;
        }

        match (self.connection.connected, self.connection.strength) {
            (true, 88..) => Svg::WifiConnected100,
            (true, 63..) => Svg::WifiConnected75,
            (true, 38..) => Svg::WifiConnected50,
            (true, 13..) => Svg::WifiConnected25,
            (true, _) => Svg::WifiConnected0,
            (false, 88..) => Svg::WifiDisconnected100,
            (false, 63..) => Svg::WifiDisconnected75,
            (false, 38..) => Svg::WifiDisconnected50,
            (false, 13..) => Svg::WifiDisconnected25,
            (false, _) => Svg::WifiDisconnected0,
        }
    }

    fn enabled(&self) -> bool {
        self.desired_enabled
    }
}
