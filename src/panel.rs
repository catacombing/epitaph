//! Panel window state.

use std::num::NonZeroU32;
use std::ptr::NonNull;

use crossfont::Metrics;
use glutin::api::egl::config::Config;
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{RawWindowHandle, WaylandWindowHandle};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Proxy, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, Layer, LayerShell, LayerSurface, LayerSurfaceConfigure,
};

use crate::module::{Alignment, Module, PanelModuleContent};
use crate::protocols::fractional_scale::FractionalScaleManager;
use crate::protocols::viewporter::Viewporter;
use crate::renderer::{Renderer, TextRenderer};
use crate::text::{GlRasterizer, Svg};
use crate::vertex::VertexBatcher;
use crate::{Result, Size, State, gl};

/// Panel height in pixels with a scale factor of 1.
pub const PANEL_HEIGHT: i32 = 20;

/// Panel SVG width.
const MODULE_WIDTH: u32 = 20;

/// Padding between panel modules.
const MODULE_PADDING: f64 = 5.;

/// Panel padding to the screen edges.
const EDGE_PADDING: f64 = 5.;

pub struct Panel {
    queue: QueueHandle<State>,
    viewport: WpViewport,
    window: LayerSurface,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: f64,
    size: Size,
}

impl Panel {
    pub fn new(
        fractional_scale: &FractionalScaleManager,
        compositor: &CompositorState,
        viewporter: &Viewporter,
        queue: QueueHandle<State>,
        layer: &LayerShell,
        egl_config: &Config,
    ) -> Result<Self> {
        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        // Initialize EGL context.
        let context_attribules = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);

        let egl_display = egl_config.display();
        let egl_context = unsafe { egl_display.create_context(egl_config, &context_attribules)? };

        // Create the Wayland surface.
        let surface = compositor.create_surface(&queue);

        let window = NonNull::new(surface.id().as_ptr().cast()).unwrap();
        let wayland_window_handle = WaylandWindowHandle::new(window);
        let raw_window_handle = RawWindowHandle::Wayland(wayland_window_handle);

        // Create the EGL surface.
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            NonZeroU32::new(size.width as u32).unwrap(),
            NonZeroU32::new(size.height as u32).unwrap(),
        );

        // Create the EGL surface.
        let egl_surface =
            unsafe { egl_config.display().create_window_surface(egl_config, &surface_attributes)? };

        // Create the window.
        let window =
            layer.create_layer_surface(&queue, surface, Layer::Bottom, Some("panel"), None);
        window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT);
        window.set_size(0, PANEL_HEIGHT as u32);
        window.set_exclusive_zone(PANEL_HEIGHT);

        // Initialize the renderer.
        let mut renderer = Renderer::new(egl_context, 1.)?;
        renderer.set_surface(Some(egl_surface));

        // Initialize fractional scaling protocol.
        fractional_scale.fractional_scaling(&queue, window.wl_surface());

        // Initialize viewporter protocol.
        let viewport = viewporter.viewport(&queue, window.wl_surface());

        Ok(Self { viewport, renderer, window, queue, size, frame_pending: false, scale_factor: 1. })
    }

    /// Render the panel.
    pub fn draw(&mut self, modules: &[&dyn Module]) -> Result<()> {
        self.frame_pending = false;

        self.renderer.draw(|renderer| unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            Self::draw_modules(renderer, modules, renderer.size)
        })
    }

    /// Render just the panel modules.
    pub fn draw_modules(
        renderer: &mut Renderer,
        modules: &[&dyn Module],
        size: Size<f32>,
    ) -> Result<()> {
        for alignment in [Alignment::Center, Alignment::Right] {
            let mut run = PanelRun::new(renderer, size, alignment)?;
            for module in modules
                .iter()
                .filter_map(|module| module.panel_module())
                .filter(|module| module.alignment() == alignment)
            {
                run.batch(module.content());
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
    pub fn set_scale_factor(&mut self, compositor: &CompositorState, scale_factor: f64) {
        let factor_change = scale_factor / self.scale_factor;
        self.scale_factor = scale_factor;

        self.resize(compositor, self.size * factor_change);
    }

    /// Reconfigure the window.
    pub fn reconfigure(&mut self, compositor: &CompositorState, configure: LayerSurfaceConfigure) {
        // Update size.
        let new_width = configure.new_size.0 as i32;
        let size = Size::new(new_width, PANEL_HEIGHT) * self.scale_factor;
        self.resize(compositor, size);
    }

    /// Request a new frame.
    pub fn request_frame(&mut self) {
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;

        let surface = self.window.wl_surface();
        surface.frame(&self.queue, surface.clone());
        surface.commit();
    }

    /// Resize the window.
    fn resize(&mut self, compositor: &CompositorState, size: Size) {
        self.size = size;

        // Update viewporter buffer target size.
        let logical_size = size / self.scale_factor;
        self.viewport.set_destination(logical_size.width, logical_size.height);

        // Update opaque region.
        if let Ok(region) = Region::new(compositor) {
            region.add(0, 0, logical_size.width, logical_size.height);
            self.window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        let scale_factor = self.scale_factor;
        let _ = self.renderer.resize(size, scale_factor);
    }
}

/// Run of multiple panel modules.
struct PanelRun<'a> {
    batcher: &'a mut VertexBatcher<TextRenderer>,
    rasterizer: &'a mut GlRasterizer,
    alignment: Alignment,
    scale_factor: f64,
    metrics: Metrics,
    size: Size<f32>,
    width: i16,
}

impl<'a> PanelRun<'a> {
    fn new(renderer: &'a mut Renderer, size: Size<f32>, alignment: Alignment) -> Result<Self> {
        Ok(Self {
            alignment,
            size,
            scale_factor: renderer.scale_factor,
            metrics: renderer.rasterizer.metrics()?,
            rasterizer: &mut renderer.rasterizer,
            batcher: &mut renderer.text_batcher,
            width: 0,
        })
    }

    /// Draw all modules in this run.
    fn draw(mut self) {
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

    /// Add a panel module to the run.
    fn batch(&mut self, module: PanelModuleContent) {
        match module {
            PanelModuleContent::Text(text) => self.batch_string(&text),
            PanelModuleContent::Svg(svg) => {
                let _ = self.batch_svg(svg);
            },
        }
    }

    /// Add text module to this run.
    fn batch_string(&mut self, text: &str) {
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
    fn batch_svg(&mut self, svg: Svg) -> Result<()> {
        let svg = self.rasterizer.rasterize_svg(svg, MODULE_WIDTH, None)?;

        // Calculate Y to center SVG.
        let y = (self.size.height as i16 - svg.height) / 2;

        for vertex in svg.vertices(self.width, y).into_iter().flatten() {
            self.batcher.push(svg.texture_id, vertex);
        }
        self.width += svg.advance.0 as i16;

        self.width += self.module_padding();

        Ok(())
    }

    /// Module padding with scale factor applied.
    fn module_padding(&self) -> i16 {
        (MODULE_PADDING * self.scale_factor).round() as i16
    }

    /// Edge padding with scale factor applied.
    fn edge_padding(&self) -> i16 {
        (EDGE_PADDING * self.scale_factor).round() as i16
    }
}
