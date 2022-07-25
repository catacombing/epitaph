//! Battery status and capacity.

use std::fs;
use std::str::FromStr;

use crate::module::{Module, ModuleRun};
use crate::text::Svg;

/// Sysfs path for retrieving battery capacity.
const CAPACITY_PATH: &str = "/sys/devices/platform/soc/1f03400.rsb/sunxi-rsb-3a3/\
                             axp20x-battery-power-supply/power_supply/axp20x-battery/capacity";

/// Sysfs path for retrieving battery charging status.
const STATUS_PATH: &str = "/sys/devices/platform/soc/1f03400.rsb/sunxi-rsb-3a3/\
                           axp20x-battery-power-supply/power_supply/axp20x-battery/status";

pub struct Battery;

impl Module for Battery {
    fn insert(&self, run: &mut ModuleRun) {
        let charging =
            fs::read_to_string(STATUS_PATH).map_or(false, |status| status.trim() == "Charging");
        let capacity = fs::read_to_string(CAPACITY_PATH)
            .ok()
            .and_then(|capacity| u8::from_str(capacity.trim()).ok())
            .unwrap_or(0);

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
