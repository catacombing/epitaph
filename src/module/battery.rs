//! Battery status and capacity.

use std::str::FromStr;
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use udev::{Enumerator, MonitorBuilder};

use crate::config::ConfigPanelModule;
use crate::module::{Alignment, Module, PanelModule, PanelModuleContent};
use crate::text::Svg;
use crate::{Result, State};

/// Refresh interval for capacity updates.
const UPDATE_INTERVAL: Duration = Duration::from_secs(60);

pub struct Battery {
    alignment: Alignment,
    charging: bool,
    capacity: u8,
}

impl Battery {
    pub fn new(event_loop: &LoopHandle<'static, State>, alignment: Alignment) -> Result<Self> {
        // Create Udev device enumerator.
        let mut socket_enumerator = Enumerator::new()?;
        socket_enumerator.match_subsystem("power_supply")?;
        let mut timer_enumerator = Enumerator::new()?;
        timer_enumerator.match_subsystem("power_supply")?;

        // Create udev socket event source.
        let udev_socket = MonitorBuilder::new()?.match_subsystem("power_supply")?.listen()?;
        let udev_source = Generic::new(udev_socket, Interest::READ, Mode::Edge);

        // Register udev socket for charging status changes.
        event_loop.insert_source(udev_source, move |_, _, state| {
            Self::update(&mut socket_enumerator, state);

            // Request new frame.
            state.unstall();

            Ok(PostAction::Continue)
        })?;

        // Register timer for battery capacity updates.
        event_loop.insert_source(Timer::immediate(), move |now, _, state| {
            Self::update(&mut timer_enumerator, state);

            // NOTE: Clock takes care of redraw here, to avoid redrawing twice per minute.

            TimeoutAction::ToInstant(now + UPDATE_INTERVAL)
        })?;

        Ok(Self { alignment, charging: false, capacity: 100 })
    }

    /// Update battery status from udev attributes.
    fn update(enumerator: &mut Enumerator, state: &mut State) {
        // Get all `power_supply` devices.
        let devices = match enumerator.scan_devices() {
            Ok(devices) => devices,
            Err(_) => return,
        };

        // Find first device with `capacity` and `status` attributes.
        let battery = devices.into_iter().find_map(|device| {
            let new_capacity = device
                .attribute_value("capacity")
                .and_then(|capacity| u8::from_str(&capacity.to_string_lossy()).ok());

            let new_charging = device.attribute_value("status").map(|status| status == "Charging");

            new_capacity.zip(new_charging)
        });

        // Update charging status.
        if let Some(((new_capacity, new_charging), battery)) =
            battery.zip(state.modules.battery.as_mut())
        {
            battery.capacity = new_capacity;
            battery.charging = new_charging;
        }
    }
}

impl Module for Battery {
    fn panel_module(&self) -> Option<&dyn PanelModule> {
        Some(self)
    }
}

impl PanelModule for Battery {
    fn alignment(&self) -> Alignment {
        self.alignment
    }

    fn set_alignment(&mut self, alignment: Alignment) {
        self.alignment = alignment;
    }

    fn content(&self) -> PanelModuleContent {
        PanelModuleContent::Svg(match (self.charging, self.capacity) {
            (true, 80..) => Svg::BatteryCharging100,
            (true, 60..=79) => Svg::BatteryCharging80,
            (true, 40..=59) => Svg::BatteryCharging60,
            (true, 20..=39) => Svg::BatteryCharging40,
            (true, 0..=19) => Svg::BatteryCharging20,
            (false, 80..) => Svg::Battery100,
            (false, 60..=79) => Svg::Battery80,
            (false, 40..=59) => Svg::Battery60,
            (false, 20..=39) => Svg::Battery40,
            (false, 0..=19) => Svg::Battery20,
        })
    }

    fn config_variant(&self) -> ConfigPanelModule {
        ConfigPanelModule::Battery
    }
}
