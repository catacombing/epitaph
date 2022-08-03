//! WiFi status and signal strength.

use std::process::Command;
use std::str::FromStr;

use crate::module::{Alignment, Module};
use crate::panel::ModuleRun;
use crate::text::Svg;

#[derive(Default)]
pub struct Cellular {
    enabled: bool,
}

impl Cellular {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Module for Cellular {
    fn alignment(&self) -> Option<Alignment> {
        Some(Alignment::Right)
    }

    fn panel_insert(&self, run: &mut ModuleRun) {
        let iw = Command::new("mmcli").args(&["-m", "0", "--signal-get"]).output();
        let stdout = match iw {
            Ok(iw) => iw.stdout,
            Err(err) => {
                eprintln!("Cellular module error: {:?}", err);
                return;
            },
        };

        let output = match String::from_utf8(stdout) {
            Ok(output) => output,
            Err(_) => return,
        };

        let start_offset = match output.find("rssi: ") {
            Some(start) => start + "rssi: ".len(),
            None => {
                run.batch_svg(Svg::CellularDisabled);
                return;
            },
        };
        let end_offset = match output[start_offset..].find(' ') {
            Some(end) => start_offset + end,
            None => return,
        };
        let signal_strength = match f32::from_str(&output[start_offset..end_offset]) {
            Ok(signal_strength) => signal_strength as i32,
            Err(_) => return,
        };

        let svg = match signal_strength {
            -40.. => Svg::Cellular100,
            -60..=-41 => Svg::Cellular80,
            -70..=-61 => Svg::Cellular60,
            -80..=-71 => Svg::Cellular40,
            -90..=-81 => Svg::Cellular20,
            _ => Svg::Cellular0,
        };
        run.batch_svg(svg);
    }

    fn drawer_button(&self) -> Option<(Svg, bool)> {
        let svg = if self.enabled { Svg::Cellular100 } else { Svg::CellularDisabled };
        Some((svg, self.enabled))
    }

    fn toggle(&mut self) {
        self.enabled = !self.enabled;

        // Set device cellular state.
        let status = if self.enabled { "-e" } else { "-d" };
        let _ = Command::new("mmcli").args(&["-m", "0", status]).spawn();
    }
}
