//! Panel window state.

use std::error::Error;
use std::result::Result as StdResult;

use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::egl::{EGLContext, EGLSurface};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::shell::layer::{
    Anchor, Layer, LayerState, LayerSurface, LayerSurfaceConfigure,
};
use wayland_egl::WlEglSurface;

use crate::module::battery::Battery;
use crate::module::cellular::Cellular;
use crate::module::clock::Clock;
use crate::module::wifi::Wifi;
use crate::module::{Alignment, ModuleRun};
use crate::renderer::Renderer;
use crate::{gl, NativeDisplay, Size, State, GL_ATTRIBUTES};

/// Panel height in pixels with a scale factor of 1.
const PANEL_HEIGHT: i32 = 20;

/// Convenience result wrapper.
type Result<T> = StdResult<T, Box<dyn Error>>;

pub struct Panel {
    queue: QueueHandle<State>,
    window: LayerSurface,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: i32,
    size: Size,
}

impl Panel {
    pub fn new(
        connection: &mut Connection,
        compositor: &CompositorState,
        queue: QueueHandle<State>,
        layer: &mut LayerState,
    ) -> Result<Self> {
        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        // Initialize EGL context.
        let native_display = NativeDisplay::new(connection.display());
        let display = EGLDisplay::new(&native_display, None)?;
        let egl_context =
            EGLContext::new_with_config(&display, GL_ATTRIBUTES, Default::default(), None)?;

        // Create the Wayland surface.
        let surface = compositor.create_surface(&queue)?;

        // Create the EGL surface.
        let config = egl_context.config_id();
        let native_surface = WlEglSurface::new(surface.id(), size.width, size.height)?;
        let pixel_format =
            egl_context.pixel_format().ok_or_else(|| String::from("no pixel format"))?;
        let egl_surface = EGLSurface::new(&display, pixel_format, config, native_surface, None)?;

        // Create the window.
        let window = LayerSurface::builder()
            .anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT)
            .exclusive_zone(PANEL_HEIGHT)
            .size((0, PANEL_HEIGHT as u32))
            .namespace("panel")
            .map(&queue, layer, surface, Layer::Top)?;

        // Initialize the renderer.
        let mut renderer = Renderer::new(egl_context, 1)?;
        renderer.set_surface(Some(egl_surface));

        Ok(Self { renderer, window, queue, size, frame_pending: false, scale_factor: 1 })
    }

    /// Render the panel.
    pub fn draw(&mut self) -> Result<()> {
        self.frame_pending = false;

        self.renderer.draw(|renderer| unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Center-aligned modules.
            let mut center = ModuleRun::new(renderer, Alignment::Center)?;
            center.insert(Clock);
            center.draw();

            // Right-aligned modules.
            let mut right = ModuleRun::new(renderer, Alignment::Right)?;
            right.insert(Cellular);
            right.insert(Wifi);
            right.insert(Battery);
            right.draw();

            Ok(())
        })
    }

    /// Check if the panel owns this surface.
    pub fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.window.wl_surface() == surface
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: i32) {
        self.window.wl_surface().set_buffer_scale(scale_factor);

        let factor_change = scale_factor as f64 / self.scale_factor as f64;
        self.scale_factor = scale_factor;

        self.resize(self.size * factor_change);
    }

    /// Reconfigure the window.
    pub fn reconfigure(&mut self, configure: LayerSurfaceConfigure) {
        let new_width = configure.new_size.0 as i32;
        let size = Size::new(new_width, PANEL_HEIGHT) * self.scale_factor as f64;
        self.resize(size);
    }

    /// Request a new frame.
    pub fn request_frame(&mut self) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;

        let surface = self.window.wl_surface();
        surface.frame(&self.queue, surface.clone()).expect("scheduled frame request");
        surface.commit();
    }

    /// Resize the window.
    fn resize(&mut self, size: Size) {
        self.size = size;

        let scale_factor = self.scale_factor;
        let _ = self.renderer.resize(size, scale_factor);
        let _ = self.draw();
    }
}
