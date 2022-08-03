//! Panel window state.

use std::error::Error;
use std::result::Result as StdResult;

use crossfont::Metrics;
use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::egl::{EGLContext, EGLSurface};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::shell::layer::{
    Anchor, Layer, LayerState, LayerSurface, LayerSurfaceConfigure,
};
use wayland_egl::WlEglSurface;

use crate::module::{Alignment, Module};
use crate::renderer::Renderer;
use crate::text::{GlRasterizer, Svg};
use crate::vertex::{GlVertex, VertexBatcher};
use crate::{gl, NativeDisplay, Size, State, GL_ATTRIBUTES};

/// Panel height in pixels with a scale factor of 1.
pub const PANEL_HEIGHT: i32 = 20;

/// Panel SVG width.
const MODULE_WIDTH: u32 = 20;

/// Panel padding to the screen edges.
const EDGE_PADDING: i16 = 5;

/// Padding between panel modules.
const MODULE_PADDING: i16 = 5;

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
    pub fn draw(&mut self, modules: &[Box<dyn Module>]) -> Result<()> {
        self.frame_pending = false;

        self.renderer.draw(|renderer| unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            Self::draw_modules(renderer, modules, renderer.size)
        })
    }

    /// Render just the panel modules.
    pub fn draw_modules(
        renderer: &mut Renderer,
        modules: &[Box<dyn Module>],
        size: Size<f32>,
    ) -> Result<()> {
        for alignment in [Alignment::Center, Alignment::Right] {
            let mut run = ModuleRun::new(renderer, size, alignment)?;

            for module in modules.iter().filter(|module| module.alignment() == Some(alignment)) {
                module.panel_insert(&mut run);
            }

            run.draw();
        }
        Ok(())
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
    }
}

/// Run of multiple panel modules.
pub struct ModuleRun<'a> {
    batcher: &'a mut VertexBatcher<GlVertex>,
    rasterizer: &'a mut GlRasterizer,
    alignment: Alignment,
    scale_factor: i16,
    metrics: Metrics,
    size: Size<f32>,
    width: i16,
}

impl<'a> ModuleRun<'a> {
    pub fn new(renderer: &'a mut Renderer, size: Size<f32>, alignment: Alignment) -> Result<Self> {
        Ok(Self {
            alignment,
            size,
            scale_factor: renderer.scale_factor as i16,
            metrics: renderer.rasterizer.metrics()?,
            rasterizer: &mut renderer.rasterizer,
            batcher: &mut renderer.batcher,
            width: 0,
        })
    }

    /// Draw all modules in this run.
    pub fn draw(mut self) {
        // Trim last module padding.
        self.width = self.width.saturating_sub(self.module_padding());

        // Determine vertex offset from left screen edge.
        let x_offset = match self.alignment {
            Alignment::Center => (self.size.width as i16 - self.width) / 2,
            Alignment::Right => self.size.width as i16 - self.width - self.edge_padding(),
        };

        // Update vertex position based on text alignment.
        for vertex in self.batcher.pending() {
            vertex.x += x_offset;
        }

        // Draw all batched vertices.
        let mut batches = self.batcher.batches();
        while let Some(batch) = batches.next() {
            batch.draw();
        }
    }

    /// Add text module to this run.
    pub fn batch_string(&mut self, text: &str) {
        // Calculate Y to center text.
        let y = ((self.size.height as f64 - self.metrics.line_height) / 2.
            + (self.metrics.line_height + self.metrics.descent as f64)) as i16;

        // Batch vertices for all glyphs.
        for glyph in self.rasterizer.rasterize_string(text) {
            for vertex in glyph.vertices(self.width, y).into_iter().flatten() {
                self.batcher.push(glyph.texture_id, vertex);
            }

            self.width += glyph.advance.0 as i16;
        }

        self.width += self.module_padding();
    }

    /// Add SVG module to this run.
    pub fn batch_svg(&mut self, svg: Svg) {
        let svg = match self.rasterizer.rasterize_svg(svg, MODULE_WIDTH) {
            Ok(svg) => svg,
            Err(err) => {
                eprintln!("SVG rasterization error: {:?}", err);
                return;
            },
        };

        // Calculate Y to center SVG.
        let y = (self.size.height as i16 - svg.height as i16) / 2;

        for vertex in svg.vertices(self.width, y).into_iter().flatten() {
            self.batcher.push(svg.texture_id, vertex);
        }
        self.width += svg.advance.0 as i16;

        self.width += self.module_padding();
    }

    /// Module padding with scale factor applied.
    fn module_padding(&self) -> i16 {
        MODULE_PADDING * self.scale_factor
    }

    /// Edge padding with scale factor applied.
    fn edge_padding(&self) -> i16 {
        EDGE_PADDING * self.scale_factor
    }
}
