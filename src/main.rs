use std::error::Error;
use std::ops::Mul;
use std::process;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use smithay::backend::egl::context::GlAttributes;
use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::egl::native::{EGLNativeDisplay, EGLPlatform};
use smithay::backend::egl::{ffi, EGLContext, EGLSurface};
use smithay::egl_platform;
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::event_loop::WaylandSource;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::protocol::wl_display::WlDisplay;
use smithay_client_toolkit::reexports::client::protocol::wl_output::WlOutput;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, EventQueue, Proxy, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::touch::TouchHandler;
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::layer::{
    Anchor, Layer, LayerHandler, LayerState, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_seat,
    delegate_touch, registry_handlers,
};
use wayland_egl::WlEglSurface;

use crate::renderer::Renderer;

mod renderer;
mod text;
mod vertex;

mod gl {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/gl_bindings.rs"));
}

/// Attributes for OpenGL context creation.
const GL_ATTRIBUTES: GlAttributes =
    GlAttributes { version: (2, 0), profile: None, debug: false, vsync: false };

/// Maximum time between redraws.
const FRAME_INTERVAL: Duration = Duration::from_secs(60);

/// Panel height in pixels with a scale factor of 1.
const PANEL_HEIGHT: i32 = 20;

fn main() {
    // Initialize Wayland connection.
    let mut connection = match Connection::connect_to_env() {
        Ok(connection) => connection,
        Err(err) => {
            eprintln!("Error: {}", err);
            process::exit(1);
        },
    };
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();

    let mut state = State::new(&mut connection, &mut queue).expect("state setup");

    // Setup calloop event loop.
    let mut event_loop = EventLoop::try_new().expect("event loop creation");
    let wayland_source = WaylandSource::new(queue).expect("wayland source creation");
    wayland_source.insert(event_loop.handle()).expect("wayland source registration");

    // Start event loop.
    let mut next_frame = Instant::now() + FRAME_INTERVAL;
    while !state.terminated {
        // Calculate upper bound for event queue dispatch timeout.
        let timeout = next_frame.saturating_duration_since(Instant::now());

        // Dispatch Wayland & Calloop event queue.
        event_loop.dispatch(Some(timeout), &mut state).expect("event dispatch");

        // Request redraw when `FRAME_INTERVAL` was reached.
        let now = Instant::now();
        if now >= next_frame {
            next_frame = now + FRAME_INTERVAL;

            let surface = state.window().wl_surface();
            surface.frame(&handle, surface.clone()).expect("scheduled frame request");
            surface.commit();
        }
    }
}

/// Wayland protocol handler state.
struct State {
    protocol_states: ProtocolStates,
    terminated: bool,
    factor: i32,
    size: Size,

    egl_context: Option<EGLContext>,
    egl_surface: Option<EGLSurface>,
    window: Option<LayerSurface>,
    renderer: Option<Renderer>,
    touch: Option<WlTouch>,
}

impl State {
    fn new(
        connection: &mut Connection,
        queue: &mut EventQueue<Self>,
    ) -> Result<Self, Box<dyn Error>> {
        // Setup globals.
        let queue_handle = queue.handle();
        let protocol_states = ProtocolStates::new(connection, &queue_handle);

        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        let mut state = Self {
            factor: 1,
            protocol_states,
            size,
            egl_context: Default::default(),
            egl_surface: Default::default(),
            terminated: Default::default(),
            renderer: Default::default(),
            window: Default::default(),
            touch: Default::default(),
        };

        // Roundtrip to initialize globals.
        queue.blocking_dispatch(&mut state)?;
        queue.blocking_dispatch(&mut state)?;

        state.init_window(connection, &queue_handle)?;

        Ok(state)
    }

    /// Initialize the window and its EGL surface.
    fn init_window(
        &mut self,
        connection: &mut Connection,
        queue: &QueueHandle<Self>,
    ) -> Result<(), Box<dyn Error>> {
        // Initialize EGL context.
        let native_display = NativeDisplay::new(connection.display());
        let display = EGLDisplay::new(&native_display, None)?;
        let context =
            EGLContext::new_with_config(&display, GL_ATTRIBUTES, Default::default(), None)?;

        // Create the Wayland surface.
        let surface = self.protocol_states.compositor.create_surface(queue)?;

        // Create the EGL surface.
        let config = context.config_id();
        let native_surface = WlEglSurface::new(surface.id(), self.size.width, self.size.height)?;
        let pixel_format = context.pixel_format().ok_or_else(|| String::from("no pixel format"))?;
        let egl_surface = EGLSurface::new(&display, pixel_format, config, native_surface, None)?;

        // Create the window.
        let window = LayerSurface::builder()
            .anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT)
            .exclusive_zone(PANEL_HEIGHT)
            .size((0, PANEL_HEIGHT as u32))
            .namespace("panel")
            .map(queue, &mut self.protocol_states.layer, surface, Layer::Top)?;

        // Initialize the renderer.
        let renderer = Renderer::new(&context, &egl_surface)?;

        self.egl_surface = Some(egl_surface);
        self.egl_context = Some(context);
        self.renderer = Some(renderer);
        self.window = Some(window);

        Ok(())
    }

    /// Render the application state.
    fn draw(&mut self) {
        self.renderer().draw();

        if let Err(error) = self.egl_surface().swap_buffers(None) {
            eprintln!("Buffer swap failed: {:?}", error);
        }
    }

    fn resize(&mut self, size: Size) {
        self.size = size;

        self.egl_surface().resize(size.width, size.height, 0, 0);
        self.renderer().resize(size);
        self.draw();
    }

    fn egl_surface(&self) -> &EGLSurface {
        self.egl_surface.as_ref().expect("EGL surface access before initialization")
    }

    fn renderer(&mut self) -> &mut Renderer {
        self.renderer.as_mut().expect("Renderer access before initialization")
    }

    fn window(&self) -> &LayerSurface {
        self.window.as_ref().expect("Window access before initialization")
    }
}

impl ProvidesRegistryState for State {
    registry_handlers![CompositorState, OutputState, LayerState, SeatState];

    fn registry(&mut self) -> &mut RegistryState {
        &mut self.protocol_states.registry
    }
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.protocol_states.compositor
    }

    fn scale_factor_changed(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        factor: i32,
    ) {
        self.window().wl_surface().set_buffer_scale(factor);

        let factor_change = factor as f64 / self.factor as f64;
        self.factor = factor;

        self.resize(self.size * factor_change);
    }

    fn frame(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _surface: &WlSurface,
        _time: u32,
    ) {
        self.draw();
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

impl LayerHandler for State {
    fn layer_state(&mut self) -> &mut LayerState {
        &mut self.protocol_states.layer
    }

    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.terminated = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let new_width = configure.new_size.0 as i32;
        let size = Size::new(new_width, PANEL_HEIGHT) * self.factor as f64;
        self.resize(size);

        self.draw();
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
        _surface: WlSurface,
        _id: i32,
        _position: (f64, f64),
    ) {
    }

    fn up(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _serial: u32,
        _time: u32,
        _id: i32,
    ) {
    }

    fn motion(
        &mut self,
        _connection: &Connection,
        _queue: &QueueHandle<Self>,
        _touch: &WlTouch,
        _time: u32,
        _id: i32,
        _position: (f64, f64),
    ) {
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
    compositor: CompositorState,
    registry: RegistryState,
    output: OutputState,
    layer: LayerState,
    seat: SeatState,
}

impl ProtocolStates {
    fn new(connection: &Connection, queue: &QueueHandle<State>) -> Self {
        Self {
            registry: RegistryState::new(connection, queue),
            compositor: CompositorState::new(),
            output: OutputState::new(),
            layer: LayerState::new(),
            seat: SeatState::new(),
        }
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

struct NativeDisplay {
    display: WlDisplay,
}

impl NativeDisplay {
    fn new(display: WlDisplay) -> Self {
        Self { display }
    }
}

impl EGLNativeDisplay for NativeDisplay {
    fn supported_platforms(&self) -> Vec<EGLPlatform<'_>> {
        let display = self.display.id().as_ptr();
        vec![
            egl_platform!(PLATFORM_WAYLAND_KHR, display, &["EGL_KHR_platform_wayland"]),
            egl_platform!(PLATFORM_WAYLAND_EXT, display, &["EGL_EXT_platform_wayland"]),
        ]
    }
}
