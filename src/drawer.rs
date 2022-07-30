//! Drawer window state.

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

use crate::renderer::Renderer;
use crate::{gl, NativeDisplay, Size, State, GL_ATTRIBUTES};

/// Convenience result wrapper.
type Result<T> = StdResult<T, Box<dyn Error>>;

pub struct Drawer {
    window: Option<LayerSurface>,
    queue: QueueHandle<State>,
    display: EGLDisplay,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: i32,
    offset: f64,
    size: Size,
}

impl Drawer {
    pub fn new(connection: &mut Connection, queue: QueueHandle<State>) -> Result<Self> {
        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        // Initialize EGL context.
        let native_display = NativeDisplay::new(connection.display());
        let display = EGLDisplay::new(&native_display, None)?;
        let egl_context =
            EGLContext::new_with_config(&display, GL_ATTRIBUTES, Default::default(), None)?;

        // Initialize the renderer.
        let renderer = Renderer::new(egl_context, 1)?;

        Ok(Self {
            renderer,
            display,
            queue,
            size,
            scale_factor: 1,
            frame_pending: Default::default(),
            window: Default::default(),
            offset: Default::default(),
        })
    }

    /// Create the window.
    pub fn show(&mut self, compositor: &CompositorState, layer: &mut LayerState) -> Result<()> {
        // Ensure the window is not mapped yet.
        if self.window.is_some() {
            return Ok(());
        }

        // Create the Wayland surface.
        let surface = compositor.create_surface(&self.queue)?;

        // Create the EGL surface.
        let config = self.renderer.egl_context().config_id();
        let native_surface = WlEglSurface::new(surface.id(), self.size.width, self.size.height)?;
        let pixel_format = self
            .renderer
            .egl_context()
            .pixel_format()
            .ok_or_else(|| String::from("no pixel format"))?;
        let egl_surface =
            EGLSurface::new(&self.display, pixel_format, config, native_surface, None)?;

        // Create the window.
        self.window = Some(
            LayerSurface::builder()
                .anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM)
                .exclusive_zone(-1)
                .size((0, 0))
                .namespace("panel")
                .map(&self.queue, layer, surface, Layer::Overlay)?,
        );

        self.renderer.set_surface(Some(egl_surface));

        // Reset window offset.
        self.offset = 0.;

        Ok(())
    }

    /// Destroy the window.
    pub fn hide(&mut self) {
        self.renderer.set_surface(None);
        self.window = None;
    }

    /// Render the panel.
    pub fn draw(&mut self, offset: f64) -> Result<()> {
        self.offset = (offset * self.scale_factor as f64).min(self.size.height as f64);
        self.frame_pending = false;

        self.renderer.draw(|_| unsafe {
            // Transparently clear entire screen.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Viewport(0, 0, self.size.width, self.size.height);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Setup drawer to render at correct offset.
            let y_offset = (self.size.height as f64 - self.offset) as i32;
            gl::Enable(gl::SCISSOR_TEST);
            gl::Scissor(0, y_offset, self.size.width, self.size.height);
            gl::Viewport(0, y_offset, self.size.width, self.size.height);

            // Draw background for the offset viewport.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // TODO: Draw content.

            Ok(())
        })
    }

    /// Check if the panel owns this surface.
    pub fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.window.as_ref().map_or(false, |window| window.wl_surface() == surface)
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: i32) {
        // Ensure the window is currently mapped.
        let window = match &self.window {
            Some(window) => window,
            None => return,
        };

        window.wl_surface().set_buffer_scale(scale_factor);

        let factor_change = scale_factor as f64 / self.scale_factor as f64;
        self.scale_factor = scale_factor;

        self.resize(self.size * factor_change);
    }

    /// Reconfigure the window.
    pub fn reconfigure(&mut self, configure: LayerSurfaceConfigure) {
        let new_width = configure.new_size.0 as i32;
        let new_height = configure.new_size.1 as i32;
        let size = Size::new(new_width, new_height) * self.scale_factor as f64;
        self.resize(size);
    }

    /// Request a new frame.
    pub fn request_frame(&mut self) {
        // Ensure window is mapped without pending frame.
        let window = match &self.window {
            Some(window) if !self.frame_pending => window,
            _ => return,
        };
        self.frame_pending = true;

        let surface = window.wl_surface();
        surface.frame(&self.queue, surface.clone()).expect("scheduled frame request");
        surface.commit();
    }

    pub fn max_offset(&self) -> f64 {
        (self.size.height / self.scale_factor) as f64
    }

    /// Resize the window.
    fn resize(&mut self, size: Size) {
        self.size = size;

        let scale_factor = self.scale_factor;
        let _ = self.renderer.resize(size, scale_factor);
        let _ = self.draw(self.offset);
    }
}
