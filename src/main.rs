use std::error::Error;
use std::ptr::NonNull;
use std::result::Result as StdResult;
use std::time::Instant;
use std::{env, process};

use calloop::ping::{self, Ping};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use catacomb_ipc::{self, DpmsState, IpcMessage};
use configory::{EventHandler as ConfigEventHandler, Manager, Options as ManagerOptions};
use glutin::display::{Display, DisplayApiPreference};
use raw_window_handle::{RawDisplayHandle, WaylandDisplayHandle};
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

use crate::config::Config;
use crate::drawer::{Drawer, HANDLE_HEIGHT};
use crate::module::Module;
use crate::module::battery::Battery;
use crate::module::brightness::Brightness;
use crate::module::cellular::Cellular;
use crate::module::clock::Clock;
use crate::module::date::Date;
use crate::module::flashlight::Flashlight;
use crate::module::orientation::Orientation;
use crate::module::scale::Scale;
use crate::module::volume::Volume;
use crate::module::wifi::Wifi;
use crate::panel::{PANEL_HEIGHT, Panel};
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

fn main() {
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
    panel_height: Option<u32>,
    last_tap: Option<Instant>,
    touch_start: (f64, f64),
    drawer_opening: bool,
    last_touch_y: f64,

    touch: Option<WlTouch>,
    drawer: Drawer,
    panel: Panel,

    config_manager: Manager,
    config: Config,
}

impl State {
    fn new(
        config_manager: Manager,
        connection: &Connection,
        globals: &GlobalList,
        queue: &EventQueue<Self>,
        event_loop: LoopHandle<'static, Self>,
    ) -> Result<Self> {
        // Setup globals.
        let queue_handle = queue.handle();
        let protocol_states = ProtocolStates::new(globals, &queue_handle);

        // Initialize panel modules.
        let modules = Modules::new(&event_loop)?;

        // Create process reaper.
        let reaper = Reaper::new(&event_loop)?;

        // Get EGL display.
        let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
        let wayland_display = WaylandDisplayHandle::new(display);
        let raw_display = RawDisplayHandle::Wayland(wayland_display);
        let egl_display = unsafe { Display::new(raw_display, DisplayApiPreference::Egl)? };

        // Setup windows.
        let config = load_config(&config_manager).unwrap_or_default();
        let panel = Panel::new(
            &config,
            queue.handle(),
            connection.clone(),
            event_loop.clone(),
            &protocol_states,
            egl_display.clone(),
        );
        let drawer = Drawer::new(
            &config,
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
            drawer_opening: Default::default(),
            active_touch: Default::default(),
            panel_height: Default::default(),
            last_touch_y: Default::default(),
            touch_start: Default::default(),
            tap_timeout: Default::default(),
            terminated: Default::default(),
            last_tap: Default::default(),
            touch: Default::default(),
        })
    }

    /// Draw window associated with the surface.
    fn draw(&mut self, surface: &WlSurface) {
        if self.panel.owns_surface(surface) {
            self.panel.draw(&self.config, &self.modules.as_slice());
        } else if self.drawer.owns_surface(surface) {
            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            self.drawer.draw(&self.config, compositor, modules, self.drawer_opening);
        }
    }

    /// Unstall all renderers.
    fn unstall(&mut self) {
        let compositor = &self.protocol_states.compositor;
        let modules = &mut self.modules.as_slice_mut();
        self.drawer.unstall(&self.config, compositor, modules, self.drawer_opening);

        self.panel.unstall(&self.config, &self.modules.as_slice());
    }

    /// Set drawer status without animation.
    fn set_drawer_status(&mut self, open: bool) {
        if open {
            // Show drawer on panel single-tap with drawer closed.
            self.drawer.offset = self.drawer.max_offset();
            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            self.drawer.unstall(&self.config, compositor, modules, self.drawer_opening);
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
        _: Transform,
    ) {
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
        if self.panel.owns_surface(surface) {
            self.panel.set_scale_factor(factor);

            self.panel.unstall(&self.config, &self.modules.as_slice());
        } else if self.drawer.owns_surface(surface) {
            self.drawer.set_scale_factor(factor);

            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            self.drawer.unstall(&self.config, compositor, modules, self.drawer_opening);
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
        let surface = layer.wl_surface();
        if self.panel.owns_surface(surface) {
            self.panel.set_size(&self.protocol_states.compositor, configure.new_size.into());

            self.panel.unstall(&self.config, &self.modules.as_slice());
        } else if self.drawer.owns_surface(surface) {
            self.panel_height = Some(configure.new_size.1);
            self.drawer.set_size(configure.new_size.into());

            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            self.drawer.unstall(&self.config, compositor, modules, self.drawer_opening);
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
        if capability != Capability::Touch {
            if let Some(touch) = self.touch.take() {
                touch.release();
            }
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
            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            self.drawer.show(&self.config, compositor, modules, self.drawer_opening);

            self.last_touch_y = position.1;
            self.touch_start = position;
            self.active_touch = Some(id);
            self.drawer_opening = true;
        } else if self.drawer.owns_surface(&surface) {
            let touch_start =
                self.drawer.touch_down(id, position.into(), &mut self.modules.as_slice_mut());

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
            if !self.drawer.offsetting {
                let multi_tap_interval = self.config.input.multi_tap_interval;
                if last_tap.is_some_and(|tap| tap.elapsed() <= multi_tap_interval) {
                    // Remove delayed single-tap callback.
                    if let Some(source) = self.tap_timeout.take() {
                        self.event_loop.remove(source);
                    }

                    // Turn off display on panel double-tap.
                    if self.touch_start.1 <= PANEL_HEIGHT as f64 {
                        let msg = IpcMessage::Dpms { state: Some(DpmsState::Off) };
                        let _ = catacomb_ipc::send_message(&msg);
                    }
                } else if self.touch_start.1 <= PANEL_HEIGHT as f64 {
                    // Stage delayed single-tap for taps on the top panel.
                    let drawer_opening = self.drawer_opening;
                    let timer = Timer::from_duration(multi_tap_interval);
                    let source = self.event_loop.insert_source(timer, move |_, _, state| {
                        state.set_drawer_status(drawer_opening);
                        TimeoutAction::Drop
                    });
                    self.tap_timeout = source.ok();
                } else if self.panel_height.is_some_and(|panel_height| {
                    self.touch_start.1 >= panel_height as f64 - HANDLE_HEIGHT as f64
                }) {
                    // Immediately close drawer, since handle has no double-tap.
                    self.set_drawer_status(false);
                }

                self.last_tap = Some(Instant::now());
            // Handle drawer dragging.
            } else {
                self.drawer.start_animation();

                let compositor = &self.protocol_states.compositor;
                let modules = &mut self.modules.as_slice_mut();
                self.drawer.unstall(&self.config, compositor, modules, self.drawer_opening);
            }
        // Handle module touch events.
        } else {
            let dirty = self.drawer.touch_up(id, &mut self.modules.as_slice_mut());
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
        if self.active_touch == Some(id) {
            // Ignore touch motion until drag threshold is reached.
            let x_delta = position.0 - self.touch_start.0;
            let y_delta = position.1 - self.touch_start.1;
            if x_delta.powi(2) + y_delta.powi(2) <= self.config.input.max_tap_distance {
                return;
            }

            let delta = position.1 - self.last_touch_y;

            self.drawer.offsetting = true;
            self.drawer.offset += delta;

            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            self.drawer.unstall(&self.config, compositor, modules, self.drawer_opening);

            self.last_touch_y = position.1;
        } else {
            let dirty =
                self.drawer.touch_motion(id, position.into(), &mut self.modules.as_slice_mut());

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
struct Modules {
    orientation: Orientation,
    brightness: Brightness,
    flashlight: Flashlight,
    cellular: Cellular,
    battery: Battery,
    volume: Volume,
    scale: Scale,
    clock: Clock,
    wifi: Wifi,
    date: Date,
}

impl Modules {
    fn new(event_loop: &LoopHandle<'static, State>) -> Result<Self> {
        Ok(Self {
            orientation: Orientation::new(),
            brightness: Brightness::new()?,
            flashlight: Flashlight::new(),
            cellular: Cellular::new(event_loop)?,
            battery: Battery::new(event_loop)?,
            volume: Volume::new(event_loop)?,
            clock: Clock::new(event_loop)?,
            wifi: Wifi::new(event_loop)?,
            scale: Scale::new(),
            date: Date::new()?,
        })
    }

    /// Get all modules as sorted immutable slice.
    fn as_slice(&self) -> [&dyn Module; 10] {
        [
            &self.brightness,
            &self.scale,
            &self.clock,
            &self.cellular,
            &self.wifi,
            &self.battery,
            &self.orientation,
            &self.flashlight,
            &self.date,
            &self.volume,
        ]
    }

    /// Get all modules as sorted mutable slice.
    fn as_slice_mut(&mut self) -> [&mut dyn Module; 10] {
        [
            &mut self.brightness,
            &mut self.scale,
            &mut self.clock,
            &mut self.cellular,
            &mut self.wifi,
            &mut self.battery,
            &mut self.orientation,
            &mut self.flashlight,
            &mut self.date,
            &mut self.volume,
        ]
    }
}

/// Configuration file update handler.
struct ConfigNotify {
    ping: Ping,
}

impl ConfigEventHandler<()> for ConfigNotify {
    fn file_changed(&self, _config: &configory::Config) {
        self.ping.ping();
    }
}

/// Reload the configuration.
fn load_config(config_manager: &Manager) -> Option<Config> {
    match config_manager.get::<&str, Config>(&[]) {
        Ok(config) => Some(config.unwrap_or_default()),
        // Avoid resetting active config on error.
        Err(err) => {
            error!("Config error: {err}");
            None
        },
    }
}
