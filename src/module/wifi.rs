//! WiFi status and signal strength.

use std::mem;
use std::process::{Command, Output};
use std::str::FromStr;
use std::time::{Duration, UNIX_EPOCH};

use calloop::timer::{TimeoutAction, Timer};
use calloop::LoopHandle;

use crate::module::{Alignment, DrawerModule, Module, PanelModule, PanelModuleContent, Toggle};
use crate::text::Svg;
use crate::{reaper, Result, State};

/// Refresh interval for this module.
const UPDATE_INTERVAL: Duration = Duration::from_secs(5);

/// Seconds after toggling status until updates are resumed.
const TOGGLE_COOLDOWN: u64 = 10;

/// IP to ping for checking network connectivity.
const PING_IP: &str = "1.1.1.1";

#[derive(Default, Debug)]
pub struct Wifi {
    signal_strength: i32,
    last_toggle: u64,
    connected: bool,
    disabled: bool,
}

impl Wifi {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        // Schedule module updates.
        event_loop.insert_source(Timer::immediate(), move |now, _, state| {
            // Temporarily suspend updates after toggling status.
            let secs_since_toggle = unix_secs() - state.modules.wifi.last_toggle;
            if let Some(remaining) =
                TOGGLE_COOLDOWN.checked_sub(secs_since_toggle).filter(|x| *x != 0)
            {
                return TimeoutAction::ToDuration(Duration::from_secs(remaining + 1));
            }

            // Setup signal strength updates.
            let mut iw = Command::new("iw");
            iw.args(&["dev", "wlan0", "link"]);
            state.reaper.watch(iw, Box::new(Self::iw_callback));

            // Setup internet connectivity updates.
            let mut ping = Command::new("ping");
            ping.args(&["-c", "1", PING_IP]);
            state.reaper.watch(ping, Box::new(Self::ping_callback));

            TimeoutAction::ToInstant(now + UPDATE_INTERVAL)
        })?;

        Ok(Self { signal_strength: 0, last_toggle: 0, connected: false, disabled: false })
    }

    /// Handle `ping` command completion.
    fn ping_callback(state: &mut State, output: Output) {
        let new_connected = output.status.success();
        let old_connected = mem::replace(&mut state.modules.wifi.connected, new_connected);

        // Redraw if value changed.
        if new_connected != old_connected {
            state.request_frame();
        }
    }

    /// Handle `iw` command completion.
    fn iw_callback(state: &mut State, output: Output) {
        let output = String::from_utf8_lossy(&output.stdout);

        let start_offset = match output.find("signal: ") {
            Some(start) => start + "signal: ".len(),
            None => {
                // Mark wifi as disabled when there is no active connection.
                let old_disabled = mem::replace(&mut state.modules.wifi.disabled, true);

                // Redraw if value changed.
                if !old_disabled {
                    state.request_frame();
                }

                return;
            },
        };
        let end_offset = match output[start_offset..].find(' ') {
            Some(end) => start_offset + end,
            None => return,
        };

        if let Ok(new_strength) = i32::from_str(&output[start_offset..end_offset]) {
            let old_strength = mem::replace(&mut state.modules.wifi.signal_strength, new_strength);
            let old_disabled = mem::take(&mut state.modules.wifi.disabled);

            // Redraw if value changed.
            if new_strength != old_strength || old_disabled {
                state.request_frame();
            }
        }
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
        if self.disabled {
            return PanelModuleContent::Svg(Svg::WifiDisabled);
        }

        PanelModuleContent::Svg(match (self.connected, self.signal_strength) {
            (true, -40..) => Svg::WifiConnected100,
            (true, -60..=-41) => Svg::WifiConnected75,
            (true, -75..=-61) => Svg::WifiConnected50,
            (true, _) => Svg::WifiConnected25,
            (false, -40..) => Svg::WifiDisconnected100,
            (false, -60..=-41) => Svg::WifiDisconnected75,
            (false, -75..=-61) => Svg::WifiDisconnected50,
            (false, _) => Svg::WifiDisconnected25,
        })
    }
}

impl Toggle for Wifi {
    fn toggle(&mut self) {
        // Temporarily block updates after toggling.
        self.last_toggle = unix_secs();

        // Immediately change icon for better UX.
        self.disabled = !self.disabled;

        // Set device wifi state.
        let status = if self.disabled { "off" } else { "on" };
        let _ = reaper::daemon("nmcli", ["radio", "wifi", status]);
    }

    fn svg(&self) -> Svg {
        if self.disabled {
            Svg::WifiDisabled
        } else {
            Svg::WifiConnected100
        }
    }

    fn enabled(&self) -> bool {
        !self.disabled
    }
}

/// Seconds since unix epoch.
fn unix_secs() -> u64 {
    UNIX_EPOCH.elapsed().unwrap().as_secs()
}
