use std::error::Error;
use std::ffi::CString;
use std::ops::{Div, Mul};
use std::process;
use std::ptr::NonNull;
use std::result::Result as StdResult;
use std::time::Instant;

use calloop::ping::{self, Ping};
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, LoopHandle, RegistrationToken};
use calloop_wayland_source::WaylandSource;
use catacomb_ipc::{self, DpmsState, IpcMessage};
use configory::{EventHandler as ConfigEventHandler, Manager, Options as ManagerOptions};
use glutin::api::egl::display::Display;
use glutin::config::ConfigTemplateBuilder;
use glutin::prelude::*;
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
    // Initialize Wayland connection.
    let connection = match Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("Error: {err}");
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
            state.reload_config();
            state.request_frame();
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
    drawer: Option<Drawer>,
    panel: Option<Panel>,

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

        let mut state = Self {
            protocol_states,
            config_manager,
            event_loop,
            modules,
            reaper,
            drawer_opening: Default::default(),
            active_touch: Default::default(),
            panel_height: Default::default(),
            last_touch_y: Default::default(),
            touch_start: Default::default(),
            tap_timeout: Default::default(),
            terminated: Default::default(),
            last_tap: Default::default(),
            config: Default::default(),
            drawer: Default::default(),
            touch: Default::default(),
            panel: Default::default(),
        };

        // Load initial config.
        state.reload_config();

        state.init_windows(connection, queue)?;

        Ok(state)
    }

    /// Initialize the panel/drawer windows and their EGL surfaces.
    fn init_windows(&mut self, connection: &Connection, queue: &EventQueue<Self>) -> Result<()> {
        let display = NonNull::new(connection.backend().display_ptr().cast()).unwrap();
        let wayland_display = WaylandDisplayHandle::new(display);
        let raw_display_handle = RawDisplayHandle::Wayland(wayland_display);

        // Setup the OpenGL window.
        let gl_display = unsafe { Display::new(raw_display_handle)? };

        let template = ConfigTemplateBuilder::new()
            .with_alpha_size(8)
            .with_stencil_size(0)
            .with_depth_size(0)
            .build();

        let egl_config = unsafe {
            gl_display.find_configs(template)?.next().expect("no suitable EGL configs were found")
        };

        // Load the OpenGL symbols.
        gl::load_with(|symbol| {
            let symbol = CString::new(symbol).unwrap();
            gl_display.get_proc_address(symbol.as_c_str()).cast()
        });

        // Setup panel window.
        self.panel = Some(Panel::new(
            &self.config,
            queue.handle(),
            self.event_loop.clone(),
            &self.protocol_states,
            &egl_config,
        )?);

        // Setup drawer window.
        self.drawer =
            Some(Drawer::new(&self.config, queue.handle(), &self.protocol_states, &egl_config)?);

        Ok(())
    }

    /// Draw window associated with the surface.
    fn draw(&mut self, surface: &WlSurface) {
        if self.panel().owns_surface(surface) {
            if let Err(err) =
                self.panel.as_mut().unwrap().draw(&self.config, &self.modules.as_slice())
            {
                eprintln!("Panel rendering failed: {err}");
            }
        } else if self.drawer().owns_surface(surface) {
            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            let drawer = self.drawer.as_mut().unwrap();
            if let Err(err) = drawer.draw(&self.config, compositor, modules, self.drawer_opening) {
                eprintln!("Drawer rendering failed: {err}");
            }
        }
    }

    /// Request new frame for all windows.
    fn request_frame(&mut self) {
        self.drawer().request_frame();
        self.panel().request_frame();
    }

    /// Set drawer status without animation.
    fn set_drawer_status(&mut self, open: bool) {
        let drawer = self.drawer.as_mut().unwrap();
        if open {
            // Show drawer on panel single-tap with drawer closed.
            drawer.offset = drawer.max_offset();
            drawer.request_frame();
        } else {
            // Hide drawer on single-tap of panel or drawer handle.
            drawer.offset = 0.;
            drawer.hide();
        }
    }

    /// Remove the panel's background activity bar.
    fn clear_background_activity(&mut self) {
        self.panel().clear_background_activity();
        self.request_frame();
    }

    /// Reload the configuration.
    fn reload_config(&mut self) {
        match self.config_manager.get::<&str, Config>(&[]) {
            Ok(config) => self.config = config.unwrap_or_default(),
            // Avoid resetting active config on error.
            Err(err) => eprintln!("Config error: {err}"),
        }
    }

    fn drawer(&mut self) -> &mut Drawer {
        self.drawer.as_mut().expect("Drawer window access before initialization")
    }

    fn panel(&mut self) -> &mut Panel {
        self.panel.as_mut().expect("Panel window access before initialization")
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
        if self.panel().owns_surface(surface) {
            self.panel.as_mut().unwrap().set_scale_factor(&self.protocol_states.compositor, factor);
        } else if self.drawer().owns_surface(surface) {
            self.drawer().set_scale_factor(factor);
        }
        self.draw(surface);
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
        if self.panel().owns_surface(surface) {
            self.panel.as_mut().unwrap().reconfigure(&self.protocol_states.compositor, configure);
        } else if self.drawer().owns_surface(surface) {
            self.panel_height = Some(configure.new_size.1);
            self.drawer().reconfigure(configure);
        }
        self.draw(surface);
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
        let drawer = self.drawer.as_mut().unwrap();
        let panel = self.panel.as_ref().unwrap();

        if self.active_touch.is_none() && panel.owns_surface(&surface) {
            let compositor = &self.protocol_states.compositor;
            let modules = &mut self.modules.as_slice_mut();
            if let Err(err) = drawer.show(&self.config, compositor, modules, self.drawer_opening) {
                eprintln!("Drawer opening failed: {err}");
            }

            self.last_touch_y = position.1;
            self.touch_start = position;
            self.active_touch = Some(id);
            self.drawer_opening = true;
        } else if drawer.owns_surface(&surface) {
            let touch_start = drawer.touch_down(id, position, &mut self.modules.as_slice_mut());

            // Check drawer touch status.
            if !touch_start.module_touched {
                // Initiate closing drawer if no module was touched.
                self.last_touch_y = position.1;
                self.touch_start = position;
                self.active_touch = Some(id);
                self.drawer_opening = false;
            } else if touch_start.requires_redraw {
                // Redraw if slider was touched.
                self.request_frame();
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
        let drawer = self.drawer.as_mut().unwrap();

        // Handle non-module touch events.
        if self.active_touch == Some(id) {
            let last_tap = self.last_tap.take();
            self.active_touch = None;

            // Handle short taps.
            if !drawer.offsetting {
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
                drawer.start_animation();
            }
        // Handle module touch events.
        } else {
            let dirty = drawer.touch_up(id, &mut self.modules.as_slice_mut());

            if dirty {
                self.request_frame();
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

            let drawer = self.drawer();
            drawer.offsetting = true;
            drawer.offset += delta;
            drawer.request_frame();

            self.last_touch_y = position.1;
        } else {
            let dirty = self.drawer.as_mut().unwrap().touch_motion(
                id,
                position,
                &mut self.modules.as_slice_mut(),
            );

            if dirty {
                self.request_frame();
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

#[derive(Copy, Clone, Default, Debug)]
pub struct Size<T = i32> {
    pub width: T,
    pub height: T,
}

impl<T> Size<T> {
    fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl From<Size> for Size<f32> {
    fn from(from: Size) -> Self {
        Self { width: from.width as f32, height: from.height as f32 }
    }
}

impl Mul<f64> for Size {
    type Output = Self;

    fn mul(mut self, factor: f64) -> Self {
        self.width = (self.width as f64 * factor) as i32;
        self.height = (self.height as f64 * factor) as i32;
        self
    }
}

impl Div<f64> for Size {
    type Output = Self;

    fn div(mut self, factor: f64) -> Self {
        self.width = (self.width as f64 / factor).round() as i32;
        self.height = (self.height as f64 / factor).round() as i32;
        self
    }
}
