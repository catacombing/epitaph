//! Panel window state.

use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};
use crossfont::Metrics;
use glutin::api::egl::config::Config as EglConfig;
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
    Anchor, Layer, LayerSurface, LayerSurfaceConfigure,
};

use crate::config::{Color, Config};
use crate::module::{Alignment, Module, PanelModuleContent};
use crate::renderer::{Renderer, TextRenderer};
use crate::text::{GlRasterizer, Svg};
use crate::vertex::VertexBatcher;
use crate::{ProtocolStates, Result, Size, State, gl};

/// Panel height in pixels with a scale factor of 1.
pub const PANEL_HEIGHT: i32 = 20;

/// Panel SVG width.
const MODULE_WIDTH: u32 = 20;

/// Padding between panel modules.
const MODULE_PADDING: f64 = 5.;

/// Panel padding to the screen edges.
const EDGE_PADDING: f64 = 5.;

/// Duration after which background activity will be hidden agani.
const BACKGROUND_ACTIVITY_TIMEOUT: Duration = Duration::from_millis(1000);

pub struct Panel {
    event_loop: LoopHandle<'static, State>,
    queue: QueueHandle<State>,
    viewport: WpViewport,
    window: LayerSurface,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: f64,
    size: Size,

    background_activity_timeout: Option<RegistrationToken>,
    background_activity: Option<(Color, f64)>,
    last_background_activity: Vec<f64>,
}

impl Panel {
    pub fn new(
        config: &Config,
        queue: QueueHandle<State>,
        event_loop: LoopHandle<'static, State>,
        protocol_states: &ProtocolStates,
        egl_config: &EglConfig,
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
        let surface = protocol_states.compositor.create_surface(&queue);

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
        let window = protocol_states.layer.create_layer_surface(
            &queue,
            surface,
            Layer::Bottom,
            Some("panel"),
            None,
        );
        window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT);
        window.set_size(0, PANEL_HEIGHT as u32);
        window.set_exclusive_zone(PANEL_HEIGHT);

        // Initialize the renderer.
        let renderer = Renderer::new(config, egl_context, egl_surface, 1.)?;

        // Initialize fractional scaling protocol.
        protocol_states.fractional_scale.fractional_scaling(&queue, window.wl_surface());

        // Initialize viewporter protocol.
        let viewport = protocol_states.viewporter.viewport(&queue, window.wl_surface());

        Ok(Self {
            event_loop,
            viewport,
            renderer,
            window,
            queue,
            size,
            frame_pending: false,
            scale_factor: 1.,
            background_activity_timeout: Default::default(),
            last_background_activity: Default::default(),
            background_activity: Default::default(),
        })
    }

    /// Render the panel.
    pub fn draw(&mut self, config: &Config, modules: &[&dyn Module]) -> Result<()> {
        self.frame_pending = false;

        self.update_background_activity(config, modules);

        self.renderer.draw(|renderer| {
            // Always draw default background.
            let [r, g, b] = config.colors.bg.as_f32();
            unsafe {
                gl::ClearColor(r, g, b, 1.);
                gl::Clear(gl::COLOR_BUFFER_BIT);
            }

            // Partially change background color based on the activity module.
            if let Some((color, value)) = self.background_activity {
                unsafe {
                    let width = (self.size.width as f64 * value).round() as i32;
                    let [r, g, b] = color.as_f32();

                    gl::Enable(gl::SCISSOR_TEST);
                    gl::Scissor(0, 0, width, self.size.height);

                    gl::ClearColor(r, g, b, 1.);
                    gl::Clear(gl::COLOR_BUFFER_BIT);

                    gl::Disable(gl::SCISSOR_TEST);
                }
            }

            Self::draw_modules(renderer, modules, renderer.size)
        })
    }

    /// Render just the panel modules.
    fn draw_modules(
        renderer: &mut Renderer,
        modules: &[&dyn Module],
        size: Size<f32>,
    ) -> Result<()> {
        for alignment in [Alignment::Left, Alignment::Center, Alignment::Right] {
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

    /// Update current status of the background activity bar.
    fn update_background_activity(&mut self, config: &Config, modules: &[&dyn Module]) {
        // Ensure activite cache has the correct size.
        self.last_background_activity.resize(modules.len(), 0.);

        // Find the first module that changed since the last frame.
        for (i, module) in modules.iter().enumerate().rev() {
            let module = match module.panel_background_module() {
                Some(module) => module,
                None => continue,
            };

            let value = module.value();
            if self.last_background_activity[i] != value {
                self.background_activity = Some((module.color(config), value));
                self.last_background_activity[i] = value;
            }
        }

        if self.background_activity.is_some() {
            self.restart_background_timeout();
        }
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

        let _ = self.renderer.resize(size, self.scale_factor);
    }

    /// Reset the background activity bar.
    pub fn clear_background_activity(&mut self) {
        self.background_activity = None;
    }

    /// (Re)start timer for the background activity bar.
    fn restart_background_timeout(&mut self) {
        // Cancel existing timer.
        if let Some(timeout) = self.background_activity_timeout.take() {
            self.event_loop.remove(timeout);
        }

        // Stage new timeout.
        let timer = Timer::from_duration(BACKGROUND_ACTIVITY_TIMEOUT);
        let timeout = self.event_loop.insert_source(timer, move |_, _, state| {
            state.clear_background_activity();
            TimeoutAction::Drop
        });
        self.background_activity_timeout = timeout.ok();
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
            Alignment::Left => self.edge_padding(),
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
