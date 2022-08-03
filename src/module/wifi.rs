//! WiFi status and signal strength.

use std::process::{Command, Stdio};
use std::str::FromStr;

use crate::module::{Alignment, Module};
use crate::panel::ModuleRun;
use crate::text::Svg;

#[derive(Default)]
pub struct Wifi {
    enabled: bool,
}

impl Wifi {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for Wifi {
    fn alignment(&self) -> Option<Alignment> {
        Some(Alignment::Right)
    }

    fn panel_insert(&self, run: &mut ModuleRun) {
        let iw = Command::new("iw").args(&["dev", "wlan0", "link"]).output();
        let stdout = match iw {
            Ok(iw) => iw.stdout,
            Err(err) => {
                eprintln!("Wifi module error: {:?}", err);
                return;
            },
        };

        let output = match String::from_utf8(stdout) {
            Ok(output) => output,
            Err(_) => return,
        };

        let start_offset = match output.find("signal: ") {
            Some(start) => start + "signal: ".len(),
            None => {
                run.batch_svg(Svg::WifiDisabled);
                return;
            },
        };
        let end_offset = match output[start_offset..].find(' ') {
            Some(end) => start_offset + end,
            None => return,
        };
        let signal_strength = match i32::from_str(&output[start_offset..end_offset]) {
            Ok(signal_strength) => signal_strength,
            Err(_) => return,
        };

        let connected = Command::new("ping")
            .args(&["-c", "1", "1.1.1.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_or(false, |ping| ping.success());

        let svg = match (connected, signal_strength) {
            (true, -40..) => Svg::WifiConnected100,
            (true, -60..=-41) => Svg::WifiConnected75,
            (true, -75..=-61) => Svg::WifiConnected50,
            (true, _) => Svg::WifiConnected25,
            (false, -40..) => Svg::WifiDisconnected100,
            (false, -60..=-41) => Svg::WifiDisconnected75,
            (false, -75..=-61) => Svg::WifiDisconnected50,
            (false, _) => Svg::WifiDisconnected25,
        };
        run.batch_svg(svg);
    }

    fn drawer_button(&self) -> Option<(Svg, bool)> {
        let svg = if self.enabled { Svg::WifiConnected100 } else { Svg::WifiDisabled };
        Some((svg, self.enabled))
    }

    fn toggle(&mut self) {
        self.enabled = !self.enabled;

        // Set device wifi state.
        let status = if self.enabled { "on" } else { "off" };
        let _ = Command::new("nmcli").args(&["radio", "wifi", status]).spawn();
    }
}
