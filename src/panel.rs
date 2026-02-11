//! Panel window state.

use std::mem;
use std::time::Duration;

use calloop::timer::{TimeoutAction, Timer};
use calloop::{LoopHandle, RegistrationToken};
use crossfont::Metrics;
use glutin::display::Display;
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, Layer, LayerSurface};
use tracing::error;

use crate::config::{Color, Config};
use crate::geometry::Size;
use crate::module::{Alignment, PanelModuleContent};
use crate::renderer::{Renderer, SizedRenderer, TextRenderer};
use crate::text::{GlRasterizer, Svg};
use crate::vertex::VertexBatcher;
use crate::{Modules, ProtocolStates, Result, State, gl};

/// Panel SVG width.
const MODULE_WIDTH: u32 = 20;

/// Padding between panel modules.
const MODULE_PADDING: f64 = 5.;

/// Duration after which background activity will be hidden agani.
const BACKGROUND_ACTIVITY_TIMEOUT: Duration = Duration::from_millis(1000);

pub struct Panel {
    event_loop: LoopHandle<'static, State>,
    queue: QueueHandle<State>,
    connection: Connection,
    viewport: WpViewport,
    window: LayerSurface,

    renderer: Renderer,

    background_activity_timeout: Option<RegistrationToken>,
    background_activity: Option<(Color, f64)>,
    last_background_activity: Vec<f64>,

    size: Size<u32>,
    scale: f64,

    stalled: bool,
    dirty: bool,
}

impl Panel {
    pub fn new(
        config: &Config,
        queue: QueueHandle<State>,
        connection: Connection,
        event_loop: LoopHandle<'static, State>,
        protocol_states: &ProtocolStates,
        display: Display,
    ) -> Self {
        // Create the Wayland surface.
        let surface = protocol_states.compositor.create_surface(&queue);

        // Initialize fractional scaling protocol.
        protocol_states.fractional_scale.fractional_scaling(&queue, &surface);

        // Initialize viewporter protocol.
        let viewport = protocol_states.viewporter.viewport(&queue, &surface);

        // Create the window.
        let window = protocol_states.layer.create_layer_surface(
            &queue,
            surface.clone(),
            Layer::Bottom,
            Some("panel"),
            None,
        );
        window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT);
        window.set_size(0, config.geometry.height);
        window.set_exclusive_zone(config.geometry.height as i32);
        window.commit();

        // Initialize the renderer.
        let renderer = Renderer::new(config, display, surface);

        Self {
            connection,
            event_loop,
            viewport,
            renderer,
            window,
            queue,
            stalled: true,
            dirty: true,
            scale: 1.,
            background_activity_timeout: Default::default(),
            last_background_activity: Default::default(),
            background_activity: Default::default(),
            size: Default::default(),
        }
    }

    /// Render the panel.
    pub fn draw(&mut self, config: &Config, modules: &Modules) {
        // Skip drawing initial configure is ready.
        if !self.dirty || self.size == Size::default() {
            self.stalled = true;
            return;
        }
        self.dirty = false;

        // Update viewporter logical render size.
        //
        // NOTE: This must be done every time we draw with Sway; it is not
        // persisted when drawing with the same surface multiple times.
        self.viewport.set_destination(self.size.width as i32, self.size.height as i32);

        // Mark entire surface as damaged.
        let wl_surface = self.window.wl_surface();
        wl_surface.damage(0, 0, self.size.width as i32, self.size.height as i32);

        self.update_background_activity(config, modules);

        let physical_size = self.size * self.scale;
        self.renderer.draw(physical_size, |renderer| {
            // Ensure rasterizer's text scale is up to date.
            renderer.rasterizer.set_scale_factor(self.scale);

            // Always draw default background.
            let [r, g, b] = config.colors.background.as_f32();
            unsafe {
                gl::ClearColor(r, g, b, 1.);
                gl::Clear(gl::COLOR_BUFFER_BIT);
            }

            // Partially change background color based on the activity module.
            if let Some((color, value)) = self.background_activity {
                unsafe {
                    let width = (physical_size.width as f64 * value).round() as i32;
                    let [r, g, b] = color.as_f32();

                    gl::Enable(gl::SCISSOR_TEST);
                    gl::Scissor(0, 0, width, physical_size.height as i32);

                    gl::ClearColor(r, g, b, 1.);
                    gl::Clear(gl::COLOR_BUFFER_BIT);

                    gl::Disable(gl::SCISSOR_TEST);
                }
            }

            if let Err(err) =
                Self::draw_modules(config, renderer, modules, physical_size.into(), self.scale)
            {
                error!("Failed drawer module rendering: {err}");
            }
        });

        // Request a new frame.
        let surface = self.window.wl_surface();
        surface.frame(&self.queue, surface.clone());

        // Apply surface changes.
        surface.commit();
    }

    /// Render just the panel modules.
    fn draw_modules(
        config: &Config,
        renderer: &mut SizedRenderer,
        modules: &Modules,
        size: Size<f32>,
        scale: f64,
    ) -> Result<()> {
        for alignment in [Alignment::Left, Alignment::Center, Alignment::Right] {
            let mut run = PanelRun::new(config, renderer, size, scale, alignment)?;
            for module in modules
                .as_vec()
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
    fn update_background_activity(&mut self, config: &Config, modules: &Modules) {
        // Ensure activite cache has the correct size.
        self.last_background_activity.resize(modules.as_vec().len(), 0.);

        // Find the first module that changed since the last frame.
        for (i, module) in modules.as_vec().iter().enumerate().rev() {
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

    /// Unstall the renderer.
    ///
    /// This will render a new frame if there currently is no frame request
    /// pending.
    pub fn unstall(&mut self, config: &Config, modules: &Modules) {
        // Ensure we actually draw even if renderer isn't stalled.
        self.dirty = true;

        // Ignore if unstalled.
        if !mem::take(&mut self.stalled) {
            return;
        }

        // Redraw immediately to unstall rendering.
        self.draw(config, modules);
        let _ = self.connection.flush();
    }

    /// Check if the panel owns this surface.
    pub fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.window.wl_surface() == surface
    }

    /// Update the window's logical size.
    pub fn set_size(&mut self, compositor: &CompositorState, size: Size<u32>) {
        if self.size == size {
            return;
        }

        self.size = size;

        // Update the window's opaque region.
        //
        // This is done here since it can only change on resize, but the commit happens
        // atomically on redraw.
        if let Ok(region) = Region::new(compositor) {
            region.add(0, 0, size.width as i32, size.height as i32);
            self.window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale: f64) {
        self.scale = scale;
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
    scale: f64,
    metrics: Metrics,
    size: Size<f32>,
    width: i16,
    padding: i16,
}

impl<'a> PanelRun<'a> {
    fn new(
        config: &Config,
        renderer: &'a mut SizedRenderer,
        size: Size<f32>,
        scale: f64,
        alignment: Alignment,
    ) -> Result<Self> {
        Ok(Self {
            alignment,
            scale,
            size,
            padding: (config.geometry.padding as f64 * scale).round() as i16,
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
            Alignment::Left => self.padding,
            Alignment::Center => (self.size.width as i16 - self.width) / 2,
            Alignment::Right => self.size.width as i16 - self.width - self.padding,
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
        (MODULE_PADDING * self.scale).round() as i16
    }
}
