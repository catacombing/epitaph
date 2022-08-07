//! Cellular status and signal strength.

use std::mem;
use std::process::{Command, Output};
use std::str::FromStr;
use std::time::{Duration, UNIX_EPOCH};

use calloop::timer::{TimeoutAction, Timer};
use calloop::LoopHandle;

use crate::module::{Alignment, Module};
use crate::panel::ModuleRun;
use crate::text::Svg;
use crate::{reaper, Result, State};

/// Refresh interval for this module.
const UPDATE_INTERVAL: Duration = Duration::from_secs(5);

/// Seconds after toggling status until updates are resumed.
const TOGGLE_COOLDOWN: u64 = 10;

#[derive(Default)]
pub struct Cellular {
    signal_strength: i32,
    last_toggle: u64,
    disabled: bool,
}

impl Cellular {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        // Store all the shared state.
        let cellular = Self { signal_strength: 0, disabled: false, last_toggle: 0 };

        // Schedule module updates.
        event_loop.insert_source(Timer::immediate(), move |now, _, state| {
            // Temporarily suspend updates after toggling status.
            let secs_since_toggle = unix_secs() - state.modules.cellular.last_toggle;
            if let Some(remaining) =
                TOGGLE_COOLDOWN.checked_sub(secs_since_toggle).filter(|x| *x != 0)
            {
                return TimeoutAction::ToDuration(Duration::from_secs(remaining + 1));
            }

            // Setup signal strength updates.
            let mut mmcli = Command::new("mmcli");
            mmcli.args(&["-m", "0", "--signal-get"]);
            state.reaper.watch(mmcli, Box::new(Self::mmcli_callback));

            TimeoutAction::ToInstant(now + UPDATE_INTERVAL)
        })?;

        Ok(cellular)
    }

    /// Handle `mmcli` command completion.
    fn mmcli_callback(state: &mut State, output: Output) {
        let output = String::from_utf8_lossy(&output.stdout);

        let start_offset = match output.find("rssi: ") {
            Some(start) => start + "rssi: ".len(),
            None => {
                // Mark cellular as disabled when there is no active connection.
                let old_disabled = mem::replace(&mut state.modules.cellular.disabled, true);

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

        if let Ok(strength) = f32::from_str(&output[start_offset..end_offset]) {
            let new_strength = strength as i32;
            let old_strength =
                mem::replace(&mut state.modules.cellular.signal_strength, new_strength);
            let old_disabled = mem::take(&mut state.modules.cellular.disabled);

            // Redraw if value changed.
            if new_strength != old_strength || old_disabled {
                state.request_frame();
            }
        }
    }
}

impl Module for Cellular {
    fn alignment(&self) -> Option<Alignment> {
        Some(Alignment::Right)
    }

    fn panel_insert(&self, run: &mut ModuleRun) {
        // Check if cellular is completely turned off.
        if self.disabled {
            run.batch_svg(Svg::CellularDisabled);
            return;
        }

        // Batch SVG for current signal strength.
        let svg = match self.signal_strength {
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
        let svg = if self.disabled { Svg::CellularDisabled } else { Svg::Cellular100 };
        Some((svg, !self.disabled))
    }

    fn toggle(&mut self) {
        // Temporarily block updates after toggling.
        self.last_toggle = unix_secs();

        // Immediately change icon for better UX.
        self.disabled = !self.disabled;

        // Set device cellular state.
        let status = if self.disabled { "-d" } else { "-e" };
        let _ = reaper::daemon("mmcli", ["-m", "0", status]);
    }
}

/// Seconds since unix epoch.
fn unix_secs() -> u64 {
    UNIX_EPOCH.elapsed().unwrap().as_secs()
}
