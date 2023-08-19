//! WiFi status and signal strength.

use calloop::channel::Event;
use calloop::LoopHandle;

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

            let old_svg = state.modules.wifi.svg();

            // Update connection status.
            state.modules.wifi.connection = connection;

            // Request redraw only if SVG changed.
            if old_svg != state.modules.wifi.svg() {
                state.request_frame();
            }
        })?;

        Ok(Self { connection: WifiConnection::default(), desired_enabled: true })
    }
}

impl Module for Wifi {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }

    fn drawer_module(&mut self) -> Option<DrawerModule> {
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
            (true, 0..=25) => Svg::WifiConnected25,
            (true, 26..=50) => Svg::WifiConnected50,
            (true, 51..=75) => Svg::WifiConnected75,
            (true, 76..) => Svg::WifiConnected100,
            (false, 0..=25) => Svg::WifiDisconnected25,
            (false, 26..=50) => Svg::WifiDisconnected50,
            (false, 51..=75) => Svg::WifiDisconnected75,
            (false, 76..) => Svg::WifiDisconnected100,
        }
    }

    fn enabled(&self) -> bool {
        self.desired_enabled
    }
}
