//! Drawer window state.

use std::mem;
use std::num::NonZeroU32;
use std::ptr::NonNull;
use std::time::Instant;

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

use crate::config::Config;
use crate::module::{DrawerModule, Module, Slider, Toggle};
use crate::panel::PANEL_HEIGHT;
use crate::renderer::{RectRenderer, Renderer, TextRenderer};
use crate::text::{GlRasterizer, GlSubTexture, Svg};
use crate::vertex::{RectVertex, VertexBatcher};
use crate::{ProtocolStates, Result, Size, State, gl};

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

    last_animation_frame: Option<Instant>,
    opening_icon: Option<GlSubTexture>,
    closing_icon: Option<GlSubTexture>,
    viewport: WpViewport,
    window: LayerSurface,
    queue: QueueHandle<State>,
    touch_module: Option<usize>,
    touch_position: (f64, f64),
    touch_id: Option<i32>,
    pending_resize: bool,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: f64,
    visible: bool,
    size: Size,
}

impl Drawer {
    pub fn new(
        config: &Config,
        queue: QueueHandle<State>,
        protocol_states: &ProtocolStates,
        egl_config: &EglConfig,
    ) -> Result<Self> {
        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        let context_attribules = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);

        let egl_context =
            unsafe { egl_config.display().create_context(egl_config, &context_attribules)? };

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

        // Setup layer shell surface.
        let window = protocol_states.layer.create_layer_surface(
            &queue,
            surface,
            Layer::Overlay,
            Some("panel"),
            None,
        );
        window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM);
        window.set_exclusive_zone(-1);
        window.commit();

        // Initialize the renderer.
        let renderer = Renderer::new(config, egl_context, egl_surface, 1.)?;

        // Initialize fractional scaling protocol.
        protocol_states.fractional_scale.fractional_scaling(&queue, window.wl_surface());

        // Initialize viewporter protocol.
        let viewport = protocol_states.viewporter.viewport(&queue, window.wl_surface());

        Ok(Self {
            renderer,
            viewport,
            window,
            queue,
            size,
            scale_factor: 1.,
            last_animation_frame: Default::default(),
            pending_resize: Default::default(),
            touch_position: Default::default(),
            frame_pending: Default::default(),
            touch_module: Default::default(),
            opening_icon: Default::default(),
            closing_icon: Default::default(),
            offsetting: Default::default(),
            touch_id: Default::default(),
            visible: Default::default(),
            offset: Default::default(),
        })
    }

    /// Show the drawer window.
    pub fn show(
        &mut self,
        config: &Config,
        compositor: &CompositorState,
        modules: &mut [&mut dyn Module],
        opening: bool,
    ) -> Result<()> {
        self.visible = true;

        // Immediately render the first frame.
        self.draw(config, compositor, modules, opening)
    }

    /// Hide the drawer window.
    pub fn hide(&mut self) {
        self.visible = false;

        // Immediately detach the buffer, hiding the window.
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
    ) -> Result<()> {
        self.frame_pending = false;

        // Never attach new buffers while hidden.
        if !self.visible {
            return Ok(());
        }

        // Apply pending resize before rendering.
        //
        // XXX: This cannot be done in `Self::resize` since that would cause latching
        // with multiple resize events while hidden, running into the Mesa bug
        // that prevents us from resizing the surface until rendering.
        if mem::take(&mut self.pending_resize) {
            // Update viewporter buffer target size.
            let logical_size = self.size / self.scale_factor;
            self.viewport.set_destination(logical_size.width, logical_size.height);

            // Ensure drawer stays fully open after resize.
            if !self.offsetting && self.offset > 0. {
                self.offset = self.max_offset();
            }

            let _ = self.renderer.resize(self.size, self.scale_factor);
        }

        // Update drawer open/close animation.
        self.animate_drawer(opening);
        if self.last_animation_frame.is_some() {
            let surface = self.window.wl_surface();
            surface.frame(&self.queue, surface.clone());
        }

        // Clamp offset, to ensure minimize works immediately.
        let max_offset = self.max_offset();
        self.offset = self.offset.min(max_offset).max(0.);

        // Calculate drawer offset.
        let offset = (self.offset * self.scale_factor).min(self.size.height as f64);
        let y_offset = self.size.height - offset.round() as i32;

        // Skip rendering if there's nothing to draw.
        if y_offset >= self.size.height {
            return Ok(());
        }

        // Update opaque region.
        if let Ok(region) = Region::new(compositor) {
            // Calculate vertical opaque region start.
            let logical_size = self.size / self.scale_factor;
            let drawer_height = logical_size.height - PANEL_HEIGHT;
            let y = (self.offset - drawer_height as f64).max(0.).round() as i32;

            region.add(0, y, logical_size.width, self.offset.round() as i32);
            self.window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        self.renderer.draw(|renderer| unsafe {
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
            gl::Disable(gl::SCISSOR_TEST);
            gl::Viewport(0, 0, self.size.width, self.size.height);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Setup drawer to render at correct offset.
            let panel_height = (PANEL_HEIGHT as f64 * renderer.scale_factor).round() as i32;
            gl::Enable(gl::SCISSOR_TEST);
            gl::Scissor(0, y_offset, self.size.width, self.size.height - panel_height);
            gl::Viewport(0, y_offset, self.size.width, self.size.height);

            // Draw background for the offset viewport.
            let [r, g, b] = config.colors.bg.as_f32();
            gl::ClearColor(r, g, b, 1.);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Add modules to rendering batch.
            let mut run = DrawerRun::new(renderer);
            for module in modules.iter_mut().filter_map(|module| module.drawer_module()) {
                run.batch(config, module);
            }

            // Add drawer handle to rendering batch.
            let opening = opening && self.offset != max_offset;
            let handle_icon = if opening { &self.opening_icon } else { &self.closing_icon };
            if let Some(handle_icon) = handle_icon {
                let handle_height = (HANDLE_HEIGHT as f64 * self.scale_factor).round() as i16;
                let handle_x = (self.size.width as i16 - handle_height) / 2;
                let handle_y = self.size.height as i16 - handle_height;
                for vertex in handle_icon.vertices(handle_x, handle_y).into_iter().flatten() {
                    run.text_batcher.push(handle_icon.texture_id, vertex);
                }
            }

            // Draw batched textures.
            run.draw();

            Ok(())
        })
    }

    /// Check if the panel owns this surface.
    pub fn owns_surface(&self, surface: &WlSurface) -> bool {
        self.window.wl_surface() == surface
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        let factor_change = scale_factor / self.scale_factor;
        self.scale_factor = scale_factor;

        // Force icon redraw on scale change.
        self.closing_icon = None;
        self.opening_icon = None;

        self.resize(self.size * factor_change);
    }

    /// Reconfigure the window.
    pub fn reconfigure(&mut self, configure: LayerSurfaceConfigure) {
        let new_width = configure.new_size.0 as i32;
        let new_height = configure.new_size.1 as i32;
        let size = Size::new(new_width, new_height) * self.scale_factor;
        self.resize(size);
    }

    /// Request a new frame.
    pub fn request_frame(&mut self) {
        // Ensure window is mapped without pending frame.
        if self.frame_pending {
            return;
        }
        self.frame_pending = true;

        let surface = self.window.wl_surface();
        surface.frame(&self.queue, surface.clone());
        surface.commit();
    }

    /// Handle touch press events.
    pub fn touch_down(
        &mut self,
        id: i32,
        position: (f64, f64),
        modules: &mut [&mut dyn Module],
    ) -> TouchStart {
        self.touch_position = scale_touch(position, self.scale_factor);
        self.touch_id = Some(id);

        // Find touched module.
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor);
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
        position: (f64, f64),
        modules: &mut [&mut dyn Module],
    ) -> bool {
        if Some(id) != self.touch_id {
            return false;
        }
        self.touch_position = scale_touch(position, self.scale_factor);

        // Update slider position.
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor);
        match self.touch_module.and_then(|module| modules[module].drawer_module()) {
            Some(DrawerModule::Slider(slider)) => {
                let relative_x = self.touch_position.0 - positioner.edge_padding as f64;
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
        self.size.height as f64 / self.scale_factor
    }

    /// Start the drawer animation.
    pub fn start_animation(&mut self) {
        self.last_animation_frame = Some(Instant::now());
        self.offsetting = false;
        self.request_frame();
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

    /// Resize the window.
    fn resize(&mut self, size: Size) {
        self.pending_resize = true;
        self.size = size;
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
    fn new(renderer: &'a mut Renderer) -> Self {
        Self {
            positioner: ModulePositioner::new(renderer.size, renderer.scale_factor),
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
        position: (f64, f64),
    ) -> Option<(usize, f64, f64)> {
        let x = position.0 as i16;
        let y = position.1 as i16;
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
                let fractional_x = (position.0 - start_x as f64) / (end_x - start_x) as f64;
                let fractional_y = (position.1 - start_y as f64) / (end_y - start_y) as f64;
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

/// Scale touch position by scale factor.
fn scale_touch(position: (f64, f64), scale_factor: f64) -> (f64, f64) {
    (position.0 * scale_factor, position.1 * scale_factor)
}
