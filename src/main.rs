use std::error::Error;
use std::ptr::NonNull;
use std::result::Result as StdResult;
use std::time::Instant;
use std::{env, process};

use calloop::ping::{self, Ping};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use catacomb_ipc::{self, CliToggle, IpcMessage};
use configory::{EventHandler as ConfigEventHandler, Manager, Options as ManagerOptions};
use glutin::display::{Display, DisplayApiPreference};
use raw_window_handle::{RawDisplayHandle, WaylandDisplayHandle};
use smallvec::SmallVec;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::{self, GlobalList};
use smithay_client_toolkit::reexports::client::protocol::wl_output::{Transform, WlOutput};
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_seat,
    delegate_touch, registry_handlers,
};
use tracing::{error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};

use crate::config::{Config, ConfigPanelModule, ConfigWrapper};
use crate::drawer::{Drawer, HANDLE_HEIGHT};
use crate::module::battery::Battery;
use crate::module::brightness::Brightness;
use crate::module::cellular::Cellular;
use crate::module::clock::Clock;
use crate::module::date::Date;
use crate::module::flashlight::Flashlight;
use crate::module::gps::Gps;
use crate::module::orientation::Orientation;
use crate::module::scale::Scale;
use crate::module::volume::Volume;
use crate::module::wifi::Wifi;
use crate::module::{Alignment, Module, PanelModule};
use crate::panel::Panel;
use crate::protocols::fractional_scale::{FractionalScaleHandler, FractionalScaleManager};
use crate::protocols::viewporter::Viewporter;
use crate::reaper::Reaper;

mod config;
mod dbus;
mod drawer;
mod geometry;
mod module;
mod panel;
mod protocols;
mod reaper;
mod renderer;
mod text;
mod vertex;

mod gl {
    #![allow(clippy::all, unsafe_op_in_unsafe_fn)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

/// Convenience result wrapper.
pub type Result<T> = StdResult<T, Box<dyn Error>>;

/// Module count; used for smallvec stack storage.
const MODULE_COUNT: usize = 11;

#[tokio::main]
async fn main() {
    // Setup logging.
    let directives = env::var("RUST_LOG").unwrap_or("warn,epitaph=info".into());
    let env_filter = EnvFilter::builder().parse_lossy(directives);
    FmtSubscriber::builder().with_env_filter(env_filter).with_line_number(true).init();

    info!("Started Epitaph");

    // Initialize Wayland connection.
    let connection = match Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(err) => {
            error!("Error: {err}");
            process::exit(1);
        },
    };
    let (globals, queue) =
        globals::registry_queue_init(&connection).expect("initialize registry queue");

    // Initialize calloop event loop.
    let mut event_loop = EventLoop::try_new().expect("initialize event loop");

    // Initialize configuration manager.
    let (ping, config_source) = ping::make_ping().expect("create config source");
    let config_notify = ConfigNotify { ping };
    let config_options = ManagerOptions::new("epitaph").notify(true);
    let config_manager =
        Manager::with_options(&config_options, config_notify).expect("config init");
    event_loop
        .handle()
        .insert_source(config_source, |_, _, state: &mut State| {
            if let Some(config) = load_config(&state.config_manager) {
                state.config = config;
                state.config_changed();
            }
            state.unstall();
        })
        .expect("register config source");

    // Setup shared state.
    let mut state = State::new(config_manager, &connection, &globals, &queue, event_loop.handle())
        .expect("state setup");

    // Insert wayland source into calloop loop.
    let wayland_source = WaylandSource::new(connection, queue);
    wayland_source.insert(event_loop.handle()).expect("wayland source registration");

    // Start event loop.
    while !state.terminated {
        // Dispatch Wayland & Calloop event queue.
        event_loop.dispatch(None, &mut state).expect("event dispatch");
    }
}

/// Wayland protocol handler state.
pub struct State {
    event_loop: LoopHandle<'static, Self>,
    protocol_states: ProtocolStates,
    modules: Modules,
    terminated: bool,
    reaper: Reaper,

    tap_timeout: Option<RegistrationToken>,
    active_touch: Option<i32>,
    drawer_height: Option<u32>,
    last_tap: Option<Instant>,
    touch_start: (f64, f64),
    drawer_opening: bool,
    last_touch_y: f64,

    touch: Option<WlTouch>,
    drawer: Drawer,
    panel: Panel,

    config_manager: Manager<ConfigNotify>,
    orientation: Transform,
    config: ConfigWrapper,
}

impl State {
    fn new(
        config_manager: Manager<ConfigNotify>,
        connection: &Connection,
        globals: &GlobalList,
        queue: &EventQueue<Self>,
        event_loop: LoopHandle<'static, Self>,
    ) -> Result<Self> {
        // Setup globals.
        let queue_handle = queue.handle();
        let protocol_states = ProtocolStates::new(globals, &queue_handle);

        // Initialize panel modules.
        let config = load_config(&config_manager).unwrap_or_default();
        let startup_config = config.orientation(Transform::Normal);
        let modules = Modules::new(startup_config, &event_loop)?;

        // Create process reaper.
        let reaper = Reaper::new(&event_loop)?;

        // Get EGL display.
        let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
        let wayland_display = WaylandDisplayHandle::new(display);
        let raw_display = RawDisplayHandle::Wayland(wayland_display);
        let egl_display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl)? };

        // Setup windows.
        let panel = Panel::new(
            startup_config,
            queue.handle(),
            connection.clone(),
            event_loop.clone(),
            &protocol_states,
            egl_display.clone(),
        );
        let drawer = Drawer::new(
            startup_config,
            queue.handle(),
            connection.clone(),
            &protocol_states,
            egl_display.clone(),
        );

        Ok(Self {
            protocol_states,
            config_manager,
            event_loop,
            modules,
            config,
            drawer,
            reaper,
            panel,
            orientation: Transform::Normal,
            drawer_opening: Default::default(),
            drawer_height: Default::default(),
            active_touch: Default::default(),
            last_touch_y: Default::default(),
            tap_timeout: Default::default(),
            touch_start: Default::default(),
            terminated: Default::default(),
            last_tap: Default::default(),
            touch: Default::default(),
        })
    }

    /// Draw window associated with the surface.
    fn draw(&mut self, surface: &WlSurface) {
        let config = self.config.orientation(self.orientation);
        if self.panel.owns_surface(surface) {
            self.panel.draw(config, &self.modules);
        } else if self.drawer.owns_surface(surface) {
            let compositor = &self.protocol_states.compositor;
            self.drawer.draw(config, compositor, &mut self.modules, self.drawer_opening);
        }
    }

    /// Unstall all renderers.
    fn unstall(&mut self) {
        let config = self.config.orientation(self.orientation);

        let compositor = &self.protocol_states.compositor;
        self.drawer.unstall(config, compositor, &mut self.modules, self.drawer_opening);

        self.panel.unstall(config, &self.modules);
    }

    /// Set drawer status without animation.
    fn set_drawer_status(&mut self, open: bool) {
        if open {
            let config = self.config.orientation(self.orientation);

            // Show drawer on panel single-tap with drawer closed.
            self.drawer.offset = self.drawer.max_offset();
            let compositor = &self.protocol_states.compositor;
            self.drawer.unstall(config, compositor, &mut self.modules, self.drawer_opening);
        } else {
            // Hide drawer on single-tap of panel or drawer handle.
            self.drawer.offset = 0.;
            self.drawer.hide();
        }
    }

    /// Remove the panel's background activity bar.
    fn clear_background_activity(&mut self) {
        self.panel.clear_background_activity();
        self.unstall();
    }

    /// Propagate configuration updates.
    fn config_changed(&mut self) {
        let config = self.config.orientation(self.orientation);
        if let Err(err) = self.modules.update_panel_modules(config, &self.event_loop) {
            error!("Failed to reload panel modules: {err}");
        }
        self.panel.update_config(config);
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![OutputState, SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _factor: i32,
    ) {
        // NOTE: We exclusively use fractional scaling.
    }

    fn frame(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        surface: &WlSurface,
        _time: u32,
    ) {
        self.draw(surface);
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        transform: Transform,
    ) {
        // Handle screen orientation changes.
        if transform != self.orientation {
            self.orientation = transform;
            self.config_changed();
        }
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlSurface,
        _: &WlOutput,
    ) {
    }
}

impl FractionalScaleHandler for State {
    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        surface: &WlSurface,
        factor: f64,
    ) {
        let config = self.config.orientation(self.orientation);

        if self.panel.owns_surface(surface) {
            self.panel.set_scale_factor(factor);

            self.panel.unstall(config, &self.modules);
        } else if self.drawer.owns_surface(surface) {
            self.drawer.set_scale_factor(factor);

            let compositor = &self.protocol_states.compositor;
            self.drawer.unstall(config, compositor, &mut self.modules, self.drawer_opening);
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.protocol_states.output
    }

    fn new_output(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _output: WlOutput,
    ) {
    }
}

impl LayerShellHandler for State {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.terminated = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _queue: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let config = self.config.orientation(self.orientation);
        let surface = layer.wl_surface();
        if self.panel.owns_surface(surface) {
            self.panel.set_size(&self.protocol_states.compositor, configure.new_size.into());

            self.panel.unstall(config, &self.modules);
        } else if self.drawer.owns_surface(surface) {
            self.drawer_height = Some(configure.new_size.1);
            self.drawer.set_size(configure.new_size.into());

            let compositor = &self.protocol_states.compositor;
            self.drawer.unstall(config, compositor, &mut self.modules, self.drawer_opening);
        }
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.protocol_states.seat
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}

    fn new_capability(
        &mut self,
        _connection: &Connection,
        queue: &QueueHandle<Self>,
        seat: WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Touch && self.touch.is_none() {
            self.touch = self.protocol_states.seat.get_touch(queue, &seat).ok();
        }
    }

    fn remove_capability(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _seat: WlSeat,
        capability: Capability,
    ) {
        if capability != Capability::Touch
            && let Some(touch) = self.touch.take()
        {
            touch.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlSeat) {}
}

impl TouchHandler for State {
    fn down(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        surface: WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        if self.active_touch.is_none() && self.panel.owns_surface(&surface) {
            self.drawer.show();

            self.last_touch_y = position.1;
            self.touch_start = position;
            self.active_touch = Some(id);
            self.drawer_opening = true;
        } else if self.drawer.owns_surface(&surface) {
            let config = self.config.orientation(self.orientation);
            let touch_start =
                self.drawer.touch_down(config, id, position.into(), &mut self.modules);

            // Check drawer touch status.
            if !touch_start.module_touched {
                // Initiate closing drawer if no module was touched.
                self.last_touch_y = position.1;
                self.touch_start = position;
                self.active_touch = Some(id);
                self.drawer_opening = false;
            } else if touch_start.requires_redraw {
                // Redraw if slider was touched.
                self.unstall();
            }
        }
    }

    fn up(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        id: i32,
    ) {
        // Handle non-module touch events.
        if self.active_touch == Some(id) {
            let last_tap = self.last_tap.take();
            self.active_touch = None;

            // Handle short taps.
            let config = self.config.orientation(self.orientation);
            if !self.drawer.offsetting {
                let multi_tap_interval = *config.input.multi_tap_interval;
                if last_tap.is_some_and(|tap| tap.elapsed() <= multi_tap_interval) {
                    // Remove delayed single-tap callback.
                    if let Some(source) = self.tap_timeout.take() {
                        self.event_loop.remove(source);
                    }

                    // Turn off display on panel double-tap.
                    if self.touch_start.1 <= config.geometry.height as f64 {
                        let msg = IpcMessage::Dpms { state: Some(CliToggle::Off) };
                        let _ = catacomb_ipc::send_message(&msg);
                    }
                } else if self.touch_start.1 <= config.geometry.height as f64 {
                    // Stage delayed single-tap for taps on the top panel.
                    let drawer_opening = self.drawer_opening;
                    let timer = Timer::from_duration(multi_tap_interval);
                    let source = self.event_loop.insert_source(timer, move |_, _, state| {
                        state.set_drawer_status(drawer_opening);
                        TimeoutAction::Drop
                    });
                    self.tap_timeout = source.ok();
                } else if self.drawer_height.is_some_and(|drawer_height| {
                    self.touch_start.1 >= drawer_height as f64 - HANDLE_HEIGHT as f64
                }) {
                    // Immediately close drawer, since handle has no double-tap.
                    self.set_drawer_status(false);
                }

                self.last_tap = Some(Instant::now());
            // Handle drawer dragging.
            } else {
                self.drawer.start_animation();

                let compositor = &self.protocol_states.compositor;
                self.drawer.unstall(config, compositor, &mut self.modules, self.drawer_opening);
            }
        // Handle module touch events.
        } else {
            let dirty = self.drawer.touch_up(id, &mut self.modules);
            if dirty {
                self.unstall();
            }
        }
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _time: u32,
        id: i32,
        position: (f64, f64),
    ) {
        let config = self.config.orientation(self.orientation);

        if self.active_touch == Some(id) {
            // Ignore touch motion until drag threshold is reached.
            let x_delta = position.0 - self.touch_start.0;
            let y_delta = position.1 - self.touch_start.1;
            if x_delta.powi(2) + y_delta.powi(2) <= config.input.max_tap_distance {
                return;
            }

            let delta = position.1 - self.last_touch_y;

            self.drawer.offsetting = true;
            self.drawer.offset += delta;

            let compositor = &self.protocol_states.compositor;
            self.drawer.unstall(config, compositor, &mut self.modules, self.drawer_opening);

            self.last_touch_y = position.1;
        } else {
            let dirty = self.drawer.touch_motion(config, id, position.into(), &mut self.modules);

            if dirty {
                self.unstall();
            }
        }
    }

    fn cancel(&mut self, _connection: &Connection, _queue: &QueueHandle<Self>, _touch: &WlTouch) {}

    fn shape(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _major: f64,
        _minor: f64,
    ) {
    }

    fn orientation(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _id: i32,
        _orientation: f64,
    ) {
    }
}

delegate_compositor!(State);
delegate_output!(State);
delegate_layer!(State);
delegate_seat!(State);
delegate_touch!(State);

delegate_registry!(State);

#[derive(Debug)]
struct ProtocolStates {
    fractional_scale: FractionalScaleManager,
    compositor: CompositorState,
    registry: RegistryState,
    viewporter: Viewporter,
    output: OutputState,
    layer: LayerShell,
    seat: SeatState,
}

impl ProtocolStates {
    fn new(globals: &GlobalList, queue: &QueueHandle<State>) -> Self {
        Self {
            registry: RegistryState::new(globals),
            fractional_scale: FractionalScaleManager::new(globals, queue)
                .expect("missing wp_fractional_scale"),
            compositor: CompositorState::bind(globals, queue).expect("missing wl_compositor"),
            viewporter: Viewporter::new(globals, queue).expect("missing wp_viewporter"),
            layer: LayerShell::bind(globals, queue).expect("missing wlr_layer_shell"),
            output: OutputState::new(globals, queue),
            seat: SeatState::new(globals, queue),
        }
    }
}

/// Panel modules.
pub struct Modules {
    orientation: Orientation,
    brightness: Brightness,
    flashlight: Flashlight,
    volume: Volume,
    gps: Gps,

    panel_order: Vec<ConfigPanelModule>,
    cellular: Option<Cellular>,
    battery: Option<Battery>,
    clock: Option<Clock>,
    date: Option<Date>,
    wifi: Option<Wifi>,

    scale: Option<Scale>,
}

impl Modules {
    fn new(config: &Config, event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        let mut modules = Self {
            volume: Volume::new(event_loop)?,
            orientation: Orientation::new(),
            brightness: Brightness::new()?,
            flashlight: Flashlight::new(),
            gps: Gps::new(event_loop)?,
            scale: Scale::new(),
            panel_order: Default::default(),
            cellular: Default::default(),
            battery: Default::default(),
            clock: Default::default(),
            wifi: Default::default(),
            date: Default::default(),
        };
        modules.update_panel_modules(config, event_loop)?;

        Ok(modules)
    }

    /// Initialize or update panel modules according to the configured layout.
    pub fn update_panel_modules(
        &mut self,
        config: &Config,
        event_loop: &LoopHandle<'static, State>,
    ) -> Result<()> {
        self.panel_order.clear();

        // Ensure unused modules are dropped.
        let mut old_cellular = self.cellular.take();
        let mut old_battery = self.battery.take();
        let mut old_clock = self.clock.take();
        let mut old_date = self.date.take();
        let mut old_wifi = self.wifi.take();

        let mut assign_module = |config_modules: &[ConfigPanelModule], alignment| -> Result<()> {
            for module in config_modules {
                let panel_module: &mut dyn PanelModule = match module {
                    ConfigPanelModule::Cellular if self.cellular.is_none() => {
                        let cellular = match old_cellular.take() {
                            Some(cellular) => cellular,
                            None => Cellular::new(event_loop, alignment)?,
                        };
                        self.cellular.insert(cellular)
                    },
                    ConfigPanelModule::Battery if self.battery.is_none() => {
                        let battery = match old_battery.take() {
                            Some(battery) => battery,
                            None => Battery::new(event_loop, alignment)?,
                        };
                        self.battery.insert(battery)
                    },
                    ConfigPanelModule::Clock if self.clock.is_none() => {
                        let clock_format = config.modules.clock_format.clone();
                        let mut clock = match old_clock.take() {
                            Some(clock) => clock,
                            None => Clock::new(event_loop, alignment, clock_format.clone())?,
                        };
                        clock.set_format(clock_format);
                        self.clock.insert(clock)
                    },
                    ConfigPanelModule::Date if self.date.is_none() => {
                        let date_format = config.modules.date_format.clone();
                        let mut date = match old_date.take() {
                            Some(date) => date,
                            None => Date::new(alignment, date_format.clone()),
                        };
                        date.set_format(date_format);
                        self.date.insert(date)
                    },
                    ConfigPanelModule::Wifi if self.wifi.is_none() => {
                        let wifi = match old_wifi.take() {
                            Some(wifi) => wifi,
                            None => Wifi::new(event_loop, alignment)?,
                        };
                        self.wifi.insert(wifi)
                    },
                    _ => continue,
                };
                panel_module.set_alignment(alignment);

                self.panel_order.push(*module);
            }
            Ok(())
        };

        assign_module(&config.modules.left, Alignment::Left)?;
        assign_module(&config.modules.center, Alignment::Center)?;
        assign_module(&config.modules.right, Alignment::Right)?;

        Ok(())
    }

    /// Get modules as an immutable vector.
    pub fn as_vec(&self) -> SmallVec<[&dyn Module; MODULE_COUNT]> {
        let mut vec: SmallVec<[&dyn Module; MODULE_COUNT]> = SmallVec::new();

        vec.push(&self.volume);

        // Add drawer sliders at the top.
        vec.push(&self.brightness);
        if let Some(scale) = &self.scale {
            vec.push(scale);
        }

        // Insert panel modules using an intermediate array for sorting.

        let mut panel_modules: SmallVec<[&dyn Module; MODULE_COUNT]> = SmallVec::new();
        if let Some(clock) = &self.clock {
            panel_modules.push(clock);
        }
        if let Some(cellular) = &self.cellular {
            panel_modules.push(cellular);
        }
        if let Some(wifi) = &self.wifi {
            panel_modules.push(wifi);
        }
        if let Some(battery) = &self.battery {
            panel_modules.push(battery);
        }
        if let Some(date) = &self.date {
            panel_modules.push(date);
        }

        panel_modules.sort_by_cached_key(|module| {
            module.panel_module().and_then(|module| {
                self.panel_order.iter().position(|variant| *variant == module.config_variant())
            })
        });

        for module in panel_modules {
            vec.push(module);
        }

        // Add drawer buttons.
        vec.push(&self.gps);
        vec.push(&self.orientation);
        vec.push(&self.flashlight);

        // Ensure module count is up to date.
        assert!(!vec.spilled());

        vec
    }

    /// Get modules as a mutable vector.
    pub fn as_vec_mut(&mut self) -> SmallVec<[&mut dyn Module; MODULE_COUNT]> {
        let mut vec: SmallVec<[&mut dyn Module; MODULE_COUNT]> = SmallVec::new();

        vec.push(&mut self.volume);

        // Add drawer sliders at the top.
        vec.push(&mut self.brightness);
        if let Some(scale) = &mut self.scale {
            vec.push(scale);
        }

        // Insert panel modules using an intermediate array for sorting.

        let mut panel_modules: SmallVec<[&mut dyn Module; MODULE_COUNT]> = SmallVec::new();
        if let Some(clock) = &mut self.clock {
            panel_modules.push(clock);
        }
        if let Some(cellular) = &mut self.cellular {
            panel_modules.push(cellular);
        }
        if let Some(wifi) = &mut self.wifi {
            panel_modules.push(wifi);
        }
        if let Some(battery) = &mut self.battery {
            panel_modules.push(battery);
        }
        if let Some(date) = &mut self.date {
            panel_modules.push(date);
        }

        panel_modules.sort_by_cached_key(|module| {
            module.panel_module().and_then(|module| {
                self.panel_order.iter().position(|variant| *variant == module.config_variant())
            })
        });

        for module in panel_modules {
            vec.push(module);
        }

        // Add drawer buttons.
        vec.push(&mut self.gps);
        vec.push(&mut self.orientation);
        vec.push(&mut self.flashlight);

        vec
    }
}

/// Configuration file update handler.
struct ConfigNotify {
    ping: Ping,
}

impl ConfigEventHandler for ConfigNotify {
    type MessageData = ();

    fn file_changed(&self, _config: &configory::Config) {
        self.ping.ping();
    }

    fn file_error(&self, _config: &configory::Config, err: configory::Error) {
        error!("Configuration file error: {err}");
    }
}

/// Reload the configuration.
fn load_config(config_manager: &Manager<ConfigNotify>) -> Option<ConfigWrapper> {
    match config_manager.get::<&str, ConfigWrapper>(&[]) {
        Ok(config) => Some(config.unwrap_or_default()),
        // Avoid resetting active config on error.
        Err(err) => {
            error!("Config error: {err}");
            None
        },
    }
}
