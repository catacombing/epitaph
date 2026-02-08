//! Drawer window state.

use std::mem;
use std::time::Instant;

use glutin::display::Display;
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, Layer, LayerSurface};

use crate::config::Config;
use crate::geometry::{Position, Size};
use crate::module::{DrawerModule, Module, Slider, Toggle};
use crate::panel::PANEL_HEIGHT;
use crate::renderer::{RectRenderer, Renderer, SizedRenderer, TextRenderer};
use crate::text::{GlRasterizer, GlSubTexture, Svg};
use crate::vertex::{RectVertex, VertexBatcher};
use crate::{ProtocolStates, Result, State, gl};

/// Height of the handle for single-tap closing the drawer.
pub const HANDLE_HEIGHT: u32 = 32;

/// Slider module height.
///
/// This should be less than `MODULE_SIZE`.
const SLIDER_HEIGHT: f64 = (MODULE_SIZE - 16) as f64;

/// Padding between drawer modules.
const MODULE_PADDING: f64 = 16.;

/// Drawer padding to the screen edges.
const EDGE_PADDING: f64 = 24.;

/// Drawer module width and height.
const MODULE_SIZE: u32 = 64;

/// Drawer module icon height.
const ICON_HEIGHT: u32 = 32;

/// Height percentage when drawer animation starts opening instead
/// of closing.
const ANIMATION_THRESHOLD: f64 = 0.25;

/// Animation speed multiplier.
const ANIMATION_SPEED: f64 = 3.;

pub struct Drawer {
    /// Current drawer Y-offset.
    pub offset: f64,
    /// Drawer currently in the process of being opened/closed.
    pub offsetting: bool,

    queue: QueueHandle<State>,

    connection: Connection,
    viewport: WpViewport,
    window: LayerSurface,

    last_animation_frame: Option<Instant>,
    opening_icon: Option<GlSubTexture>,
    closing_icon: Option<GlSubTexture>,
    renderer: Renderer,

    touch_module: Option<usize>,
    touch_position: Position<f64>,
    touch_id: Option<i32>,

    size: Size<u32>,
    scale: f64,

    stalled: bool,
    visible: bool,
    dirty: bool,
}

impl Drawer {
    pub fn new(
        config: &Config,
        queue: QueueHandle<State>,
        connection: Connection,
        protocol_states: &ProtocolStates,
        display: Display,
    ) -> Self {
        // Create the Wayland surface.
        let surface = protocol_states.compositor.create_surface(&queue);

        // Initialize fractional scaling protocol.
        protocol_states.fractional_scale.fractional_scaling(&queue, &surface);

        // Initialize viewporter protocol.
        let viewport = protocol_states.viewporter.viewport(&queue, &surface);

        // Setup layer shell surface.
        let window = protocol_states.layer.create_layer_surface(
            &queue,
            surface.clone(),
            Layer::Overlay,
            Some("panel"),
            None,
        );
        window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM);
        window.set_exclusive_zone(-1);
        window.commit();

        // Initialize the renderer.
        let renderer = Renderer::new(config, display, surface);

        Self {
            connection,
            renderer,
            viewport,
            window,
            queue,
            stalled: true,
            dirty: true,
            scale: 1.,
            last_animation_frame: Default::default(),
            touch_position: Default::default(),
            touch_module: Default::default(),
            opening_icon: Default::default(),
            closing_icon: Default::default(),
            offsetting: Default::default(),
            touch_id: Default::default(),
            visible: Default::default(),
            offset: Default::default(),
            size: Default::default(),
        }
    }

    /// Show the drawer window.
    pub fn show(&mut self) {
        self.visible = true;

        // Reconfigure window, since it is currently unmapped.
        self.window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM);
        self.window.set_exclusive_zone(-1);
        self.stalled = true;
        self.window.wl_surface().commit();
    }

    /// Hide the drawer window.
    pub fn hide(&mut self) {
        println!("HIDE");
        self.visible = false;

        // Immediately detach the buffer, unmapping and hiding the window.
        let surface = self.window.wl_surface();
        surface.attach(None, 0, 0);
        surface.commit();
    }

    /// Render the panel.
    pub fn draw(
        &mut self,
        config: &Config,
        compositor: &CompositorState,
        modules: &mut [&mut dyn Module],
        opening: bool,
    ) {
        // Never attach new buffers while hidden or unconfigured.
        if !self.dirty || !self.visible || self.size == Size::default() {
            self.stalled = true;
            return;
        }
        self.dirty = false;

        // Update drawer open/close animation.
        self.animate_drawer(opening);
        self.dirty |= self.last_animation_frame.is_some();

        // Clamp offset, to ensure minimize works immediately.
        let max_offset = self.max_offset();
        self.offset = self.offset.min(max_offset).max(0.);

        // Calculate drawer offset.
        let physical_size = self.size * self.scale;
        let offset = (self.offset * self.scale).min(physical_size.height as f64);
        let y_offset = physical_size.height as i32 - offset.round() as i32;

        // Skip rendering if there's nothing to draw.
        if y_offset >= physical_size.height as i32 {
            self.stalled = true;
            return;
        }

        // Update viewporter logical render size.
        //
        // NOTE: This must be done every time we draw with Sway; it is not
        // persisted when drawing with the same surface multiple times.
        self.viewport.set_destination(self.size.width as i32, self.size.height as i32);

        // Mark entire surface as damaged.
        let wl_surface = self.window.wl_surface();
        wl_surface.damage(0, 0, self.size.width as i32, self.size.height as i32);

        // Update the window's opaque region.
        if let Ok(region) = Region::new(compositor) {
            // Calculate vertical opaque region start.
            let drawer_height = self.size.height as i32 - PANEL_HEIGHT;
            let y = (self.offset - drawer_height as f64).max(0.).round() as i32;

            region.add(0, y, self.size.width as i32, self.offset.round() as i32);
            self.window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        self.renderer.draw(physical_size, |renderer| unsafe {
            // Ensure rasterizer's text scale is up to date.
            renderer.rasterizer.set_scale_factor(self.scale);

            // Dynamically initialize icons on first draw.
            if self.opening_icon.is_none() {
                let texture =
                    renderer.rasterizer.rasterize_svg(Svg::ArrowDown, None, HANDLE_HEIGHT);
                self.opening_icon = texture.ok();
            }
            if self.closing_icon.is_none() {
                let texture = renderer.rasterizer.rasterize_svg(Svg::ArrowUp, None, HANDLE_HEIGHT);
                self.closing_icon = texture.ok();
            }

            // Transparently clear entire screen.
            let width = physical_size.width as i32;
            let height = physical_size.height as i32;
            gl::Disable(gl::SCISSOR_TEST);
            gl::Viewport(0, 0, width, height);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Setup drawer to render at correct offset.
            let panel_height = (PANEL_HEIGHT as f64 * self.scale).round() as i32;
            gl::Enable(gl::SCISSOR_TEST);
            gl::Scissor(0, y_offset, width, height - panel_height);
            gl::Viewport(0, y_offset, width, height);

            // Draw background for the offset viewport.
            let [r, g, b] = config.colors.bg.as_f32();
            gl::ClearColor(r, g, b, 1.);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Add modules to rendering batch.
            let mut run = DrawerRun::new(renderer, physical_size.into(), self.scale);
            for module in modules.iter_mut().filter_map(|module| module.drawer_module()) {
                run.batch(config, module);
            }

            // Add drawer handle to rendering batch.
            let opening = opening && self.offset != max_offset;
            let handle_icon = if opening { &self.opening_icon } else { &self.closing_icon };
            if let Some(handle_icon) = handle_icon {
                let handle_height = (HANDLE_HEIGHT as f64 * self.scale).round() as i16;
                let handle_x = (physical_size.width as i16 - handle_height) / 2;
                let handle_y = physical_size.height as i16 - handle_height;
                for vertex in handle_icon.vertices(handle_x, handle_y).into_iter().flatten() {
                    run.text_batcher.push(handle_icon.texture_id, vertex);
                }
            }

            // Draw batched textures.
            run.draw();
        });

        // Request a new frame.
        let surface = self.window.wl_surface();
        surface.frame(&self.queue, surface.clone());

        // Apply surface changes.
        surface.commit();
    }

    /// Unstall the renderer.
    ///
    /// This will render a new frame if there currently is no frame request
    /// pending.
    pub fn unstall(
        &mut self,
        config: &Config,
        compositor: &CompositorState,
        modules: &mut [&mut dyn Module],
        opening: bool,
    ) {
        // Ensure we actually draw even if renderer isn't stalled.
        self.dirty = true;

        // Ignore if unstalled.
        if !mem::take(&mut self.stalled) {
            return;
        }

        // Redraw immediately to unstall rendering.
        self.draw(config, compositor, modules, opening);
        let _ = self.connection.flush();
    }

    /// Check if the panel owns this surface.
    pub fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.window.wl_surface() == surface
    }

    /// Update the window's logical size.
    pub fn set_size(&mut self, size: Size<u32>) {
        println!("SIZE CHANGE: {:?}", size);
        if self.size == size {
            return;
        }

        self.size = size;
        self.dirty = true;

        // Ensure drawer stays fully open after resize.
        if !self.offsetting && self.offset > 0. {
            self.offset = self.max_offset();
        }
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale: f64) {
        if self.scale == scale {
            return;
        }

        self.scale = scale;
        self.dirty = true;

        // Force icon redraw on scale change.
        self.closing_icon = None;
        self.opening_icon = None;
    }

    /// Handle touch press events.
    pub fn touch_down(
        &mut self,
        id: i32,
        position: Position<f64>,
        modules: &mut [&mut dyn Module],
    ) -> TouchStart {
        self.touch_position = position * self.scale;
        self.touch_id = Some(id);

        // Find touched module.
        let physical_size = self.size * self.scale;
        let positioner = ModulePositioner::new(physical_size.into(), self.scale);
        let (index, x) = match positioner.module_position(modules, self.touch_position) {
            Some((index, x, _)) => (index, x),
            None => return TouchStart { requires_redraw: false, module_touched: false },
        };
        self.touch_module = Some(index);

        // Update sliders.
        let requires_redraw = match modules[index].drawer_module() {
            Some(DrawerModule::Slider(slider)) => {
                let _ = slider.set_value(x.clamp(0., 1.));
                true
            },
            _ => false,
        };

        TouchStart { requires_redraw, module_touched: true }
    }

    /// Handle touch motion events.
    pub fn touch_motion(
        &mut self,
        id: i32,
        position: Position<f64>,
        modules: &mut [&mut dyn Module],
    ) -> bool {
        if Some(id) != self.touch_id {
            return false;
        }
        self.touch_position = position * self.scale;

        // Update slider position.
        let physical_size = self.size * self.scale;
        let positioner = ModulePositioner::new(physical_size.into(), self.scale);
        match self.touch_module.and_then(|module| modules[module].drawer_module()) {
            Some(DrawerModule::Slider(slider)) => {
                let relative_x = self.touch_position.x - positioner.edge_padding as f64;
                let fractional_x = relative_x / positioner.slider_size.width as f64;

                let _ = slider.set_value(fractional_x.clamp(0., 1.));

                true
            },
            _ => false,
        }
    }

    /// Handle touch release events.
    pub fn touch_up(&mut self, id: i32, modules: &mut [&mut dyn Module]) -> bool {
        if Some(id) != self.touch_id {
            return false;
        }

        // Handle button toggles on touch up.
        let mut dirty = false;
        match self.touch_module.and_then(|module| modules[module].drawer_module()) {
            Some(DrawerModule::Toggle(toggle)) => {
                let _ = toggle.toggle();
                dirty = true;
            },
            Some(DrawerModule::Slider(slider)) => {
                let _ = slider.on_touch_up();
                dirty = true;
            },
            _ => (),
        }

        // Reset touch state.
        self.touch_module = None;
        self.touch_id = None;

        dirty
    }

    /// Drawer offset when fully visible.
    pub fn max_offset(&self) -> f64 {
        self.size.height as f64
    }

    /// Start the drawer animation.
    pub fn start_animation(&mut self) {
        self.last_animation_frame = Some(Instant::now());
        self.offsetting = false;
    }

    /// Update drawer animation.
    fn animate_drawer(&mut self, opening: bool) {
        // Ensure animation is active.
        let last_animation_frame = match self.last_animation_frame {
            Some(last_animation_frame) => last_animation_frame,
            None => return,
        };

        let max_offset = self.max_offset();

        // Compute threshold beyond which motion will automatically be completed.
        let threshold = if opening {
            max_offset * ANIMATION_THRESHOLD
        } else {
            max_offset - max_offset * ANIMATION_THRESHOLD
        };

        // Update drawer position.
        let animation_step = last_animation_frame.elapsed().as_millis() as f64 * ANIMATION_SPEED;
        if self.offset >= threshold {
            self.offset += animation_step;
        } else {
            self.offset -= animation_step;
        }

        if self.offset <= 0. {
            self.last_animation_frame = None;
            self.hide();
        } else if self.offset >= max_offset {
            self.last_animation_frame = None;
        } else {
            self.last_animation_frame = Some(Instant::now());
        }
    }
}

/// Drawer touch start status.
#[derive(Copy, Clone)]
pub struct TouchStart {
    pub requires_redraw: bool,
    pub module_touched: bool,
}

/// Batched drawer module rendering.
struct DrawerRun<'a> {
    text_batcher: &'a mut VertexBatcher<TextRenderer>,
    rect_batcher: &'a mut VertexBatcher<RectRenderer>,
    rasterizer: &'a mut GlRasterizer,
    positioner: ModulePositioner,
    column: i16,
    row: i16,
}

impl<'a> DrawerRun<'a> {
    fn new(renderer: &'a mut SizedRenderer, size: Size<f32>, scale: f64) -> Self {
        Self {
            positioner: ModulePositioner::new(size, scale),
            rasterizer: &mut renderer.rasterizer,
            text_batcher: &mut renderer.text_batcher,
            rect_batcher: &mut renderer.rect_batcher,
            column: 0,
            row: 0,
        }
    }

    /// Add a drawer module to the run.
    fn batch(&mut self, config: &Config, module: DrawerModule) {
        let _ = match module {
            DrawerModule::Toggle(toggle) => self.batch_toggle(config, toggle),
            DrawerModule::Slider(slider) => self.batch_slider(config, slider),
        };
    }

    /// Add a slider to the drawer.
    fn batch_slider(&mut self, config: &Config, slider: &dyn Slider) -> Result<()> {
        let window_width = self.positioner.size.width;
        let window_height = self.positioner.size.height;

        let width = self.positioner.slider_size.width;
        let height = self.positioner.slider_size.height;

        // Rasterize slider icon.
        let icon = self.rasterizer.rasterize_svg(slider.svg(), ICON_HEIGHT, None)?;

        // Ensure we're in an empty row.
        if self.column != 0 {
            self.column = 0;
            self.row += 1;
        }

        // Calculate origin point.
        let (x, mut y) = self.positioner.position(self.column, self.row);
        y += (self.positioner.module_size - self.positioner.slider_size.height) / 2;

        // Update active row.
        self.row += 1;

        // Stage tray vertices.
        let module_inactive = config.colors.module_inactive;
        let tray =
            RectVertex::new(window_width, window_height, x, y, width, height, module_inactive);
        for vertex in tray {
            self.rect_batcher.push(0, vertex);
        }

        // Stage slider vertices.
        let module_active = config.colors.module_active;
        let slider_width = (width as f64 * slider.value()) as i16;
        let slider =
            RectVertex::new(window_width, window_height, x, y, slider_width, height, module_active);
        for vertex in slider {
            self.rect_batcher.push(0, vertex);
        }

        // Calculate icon origin.
        let icon_x = x + (self.positioner.slider_size.width - icon.width) / 2;
        let icon_y = y + (self.positioner.slider_size.height - icon.height) / 2;

        for vertex in icon.vertices(icon_x, icon_y).into_iter().flatten() {
            self.text_batcher.push(icon.texture_id, vertex);
        }

        Ok(())
    }

    /// Add a toggle button to the drawer.
    fn batch_toggle(&mut self, config: &Config, toggle: &dyn Toggle) -> Result<()> {
        let window_width = self.positioner.size.width;
        let window_height = self.positioner.size.height;

        let size = self.positioner.module_size;

        let svg = self.rasterizer.rasterize_svg(toggle.svg(), None, ICON_HEIGHT)?;

        // Calculate module origin point.
        let (x, y) = self.positioner.position(self.column, self.row);

        // Calculate icon origin point.
        let icon_x = x + (size - svg.width) / 2;
        let icon_y = y + (size - svg.height) / 2;

        // Update active column/row.
        self.column += 1;
        if self.column >= self.positioner.columns {
            self.column = 0;
            self.row += 1;
        }

        // Batch icon backdrop.
        let color = if toggle.enabled() {
            config.colors.module_active
        } else {
            config.colors.module_inactive
        };
        let backdrop = RectVertex::new(window_width, window_height, x, y, size, size, color);
        for vertex in backdrop {
            self.rect_batcher.push(0, vertex);
        }

        // Batch icon.
        for vertex in svg.vertices(icon_x, icon_y).into_iter().flatten() {
            self.text_batcher.push(svg.texture_id, vertex);
        }

        Ok(())
    }

    /// Draw all modules in this run.
    fn draw(self) {
        let mut rect_batches = self.rect_batcher.batches();
        while let Some(rect_batch) = rect_batches.next() {
            rect_batch.draw();
        }

        let mut text_batches = self.text_batcher.batches();
        while let Some(text_batch) = text_batches.next() {
            text_batch.draw();
        }
    }
}

/// Module position calculator.
struct ModulePositioner {
    slider_size: Size<i16>,
    module_padding: i16,
    edge_padding: i16,
    panel_height: i16,
    module_size: i16,
    size: Size<i16>,
    columns: i16,
}

impl ModulePositioner {
    pub fn new(size: Size<f32>, scale_factor: f64) -> Self {
        let size = Size::new(size.width as i16, size.height as i16);

        // Scale constants by DPI scale factor.
        let panel_height = (PANEL_HEIGHT as f64 * scale_factor).round() as i16;
        let module_size = (MODULE_SIZE as f64 * scale_factor).round() as i16;
        let module_padding = (MODULE_PADDING * scale_factor).round() as i16;
        let slider_height = (SLIDER_HEIGHT * scale_factor).round() as i16;
        let edge_padding = (EDGE_PADDING * scale_factor).round() as i16;

        let content_width = size.width - edge_padding * 2;
        let padded_module_size = module_size + module_padding;
        let columns = (content_width + module_padding) / padded_module_size;
        let edge_padding = (size.width + module_padding - columns * padded_module_size) / 2;

        let slider_width = size.width - 2 * edge_padding;
        let slider_size = Size::new(slider_width, slider_height);

        Self { module_padding, edge_padding, panel_height, slider_size, module_size, columns, size }
    }

    /// Get cell origin point.
    fn position(&self, column: i16, row: i16) -> (i16, i16) {
        let padded_module_size = self.module_size + self.module_padding;
        let x = self.edge_padding + column * padded_module_size;
        let y = self.panel_height + self.edge_padding + row * padded_module_size;

        (x, y)
    }

    /// Get relative position inside a module.
    fn module_position(
        &self,
        modules: &mut [&mut dyn Module],
        position: Position<f64>,
    ) -> Option<(usize, f64, f64)> {
        let x = position.x as i16;
        let y = position.y as i16;
        let mut start_x = self.edge_padding;
        let mut start_y = self.panel_height + self.edge_padding;

        for (i, module) in modules.iter_mut().enumerate() {
            // Only check drawer modules.
            let module = match module.drawer_module() {
                Some(module) => module,
                None => continue,
            };

            // Calculate module end.
            let end_x = match module {
                DrawerModule::Toggle(_) => start_x + self.module_size,
                DrawerModule::Slider(_) => start_x + self.slider_size.width,
            };
            let end_y = start_y + self.module_size;

            // Check if position is within this module.
            if x >= start_x && y >= start_y && x < end_x && y < end_y {
                let fractional_x = (position.x - start_x as f64) / (end_x - start_x) as f64;
                let fractional_y = (position.y - start_y as f64) / (end_y - start_y) as f64;
                return Some((i, fractional_x, fractional_y));
            }

            // Calculate next module start.
            start_x = end_x + self.module_padding;
            if start_x >= self.size.width - self.edge_padding {
                start_x = self.edge_padding;
                start_y = end_y + self.module_padding;
            }
        }

        None
    }
}
