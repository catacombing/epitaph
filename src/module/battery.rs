//! Battery status and capacity.

use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::Duration;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{Interest, LoopHandle, Mode, PostAction};
use udev::{Enumerator, MonitorBuilder};

use crate::module::{Alignment, Module};
use crate::panel::ModuleRun;
use crate::text::Svg;
use crate::{Result, State};

/// Refresh interval for capacity updates.
const UPDATE_INTERVAL: Duration = Duration::from_secs(60);

pub struct Battery {
    charging: Rc<AtomicBool>,
    capacity: Rc<AtomicU8>,
}

impl Battery {
    pub fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        let charging = Rc::new(AtomicBool::new(false));
        let capacity = Rc::new(AtomicU8::new(100));

        // Store all the shared state.
        let battery = Self { charging: charging.clone(), capacity: capacity.clone() };

        // Create Udev device enumerator.
        let mut socket_enumerator = Enumerator::new()?;
        socket_enumerator.match_subsystem("power_supply")?;
        let mut timer_enumerator = Enumerator::new()?;
        timer_enumerator.match_subsystem("power_supply")?;

        // Create udev socket event source.
        let udev_socket = MonitorBuilder::new()?.match_subsystem("power_supply")?.listen()?;
        let udev_source = Generic::new(udev_socket, Interest::READ, Mode::Edge);

        // Register udev socket for charging status changes.
        let socket_charging = charging.clone();
        let socket_capacity = capacity.clone();
        event_loop.insert_source(udev_source, move |_, _, state| {
            Self::update(&mut socket_enumerator, &socket_charging, &socket_capacity);

            // Request new frame.
            state.request_frame();

            Ok(PostAction::Continue)
        })?;

        // Register timer for battery capacity updates.
        event_loop.insert_source(Timer::immediate(), move |now, _, _| {
            Self::update(&mut timer_enumerator, &charging, &capacity);

            // NOTE: Clock takes care of redraw here, to avoid redrawing twice per minute.

            TimeoutAction::ToInstant(now + UPDATE_INTERVAL)
        })?;

        Ok(battery)
    }

    /// Update battery status from udev attributes.
    fn update(enumerator: &mut Enumerator, charging: &AtomicBool, capacity: &AtomicU8) {
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
        if let Some((new_capacity, new_charging)) = battery {
            capacity.store(new_capacity, Ordering::Relaxed);
            charging.store(new_charging, Ordering::Relaxed);
        }
    }
}

impl Module for Battery {
    fn alignment(&self) -> Option<Alignment> {
        Some(Alignment::Right)
    }

    fn panel_insert(&self, run: &mut ModuleRun) {
        let charging = self.charging.load(Ordering::Relaxed);
        let capacity = self.capacity.load(Ordering::Relaxed);

        let svg = match (charging, capacity) {
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
        };
        run.batch_svg(svg);
    }
}
