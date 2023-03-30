//! Drawer window state.
use std::num::NonZeroU32;

use glutin::api::egl::config::Config;
use glutin::config::GetGlConfig;
use glutin::context::{ContextApi, ContextAttributesBuilder, Version};
use glutin::display::GetGlDisplay;
use glutin::prelude::*;
use glutin::surface::{SurfaceAttributesBuilder, WindowSurface};
use raw_window_handle::{RawWindowHandle, WaylandWindowHandle};
use smithay_client_toolkit::compositor::{CompositorState, Region};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Proxy, QueueHandle};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, Layer, LayerShell, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;

use crate::module::{DrawerModule, Module, Slider, Toggle};
use crate::panel::PANEL_HEIGHT;
use crate::renderer::{RectRenderer, Renderer, TextRenderer};
use crate::text::GlRasterizer;
use crate::vertex::{RectVertex, VertexBatcher};
use crate::{gl, Result, Size, State};

/// Slider module height.
///
/// This should be less than `MODULE_SIZE`.
const SLIDER_HEIGHT: i16 = MODULE_SIZE as i16 - 16;

/// Color of slider handle and active buttons,
const MODULE_COLOR_FG: [u8; 4] = [85, 85, 85, 255];

/// Color of the slider tray and inactive buttons.
const MODULE_COLOR_BG: [u8; 4] = [51, 51, 51, 255];

/// Padding between drawer modules.
const MODULE_PADDING: i16 = 16;

/// Drawer padding to the screen edges.
const EDGE_PADDING: i16 = 24;

/// Drawer module width and height.
const MODULE_SIZE: u32 = 64;

/// Drawer module icon height.
const ICON_HEIGHT: u32 = 32;

pub struct Drawer {
    /// Current drawer Y-offset.
    pub offset: f64,
    /// Drawer currently in the process of being opened/closed.
    pub offsetting: bool,

    window: Option<LayerSurface>,
    queue: QueueHandle<State>,
    touch_module: Option<usize>,
    touch_position: (f64, f64),
    touch_id: Option<i32>,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: i32,
    size: Size,
}

impl Drawer {
    pub fn new(queue: QueueHandle<State>, egl_config: &Config) -> Result<Self> {
        // Default to 1x1 initial size since 0x0 EGL surfaces are illegal.
        let size = Size { width: 1, height: 1 };

        let context_attribules = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);

        let egl_context =
            unsafe { egl_config.display().create_context(egl_config, &context_attribules)? };

        // Initialize the renderer.
        let renderer = Renderer::new(egl_context, 1)?;

        Ok(Self {
            renderer,
            queue,
            size,
            scale_factor: 1,
            frame_pending: Default::default(),
            touch_position: Default::default(),
            touch_module: Default::default(),
            offsetting: Default::default(),
            touch_id: Default::default(),
            offset: Default::default(),
            window: Default::default(),
        })
    }

    /// Create the window.
    pub fn show(&mut self, compositor: &CompositorState, layer: &mut LayerShell) -> Result<()> {
        // Ensure the window is not mapped yet.
        if self.window.is_some() {
            return Ok(());
        }

        // Create the Wayland surface.
        let surface = compositor.create_surface(&self.queue);

        let mut wayland_window_handle = WaylandWindowHandle::empty();
        wayland_window_handle.surface = surface.id().as_ptr() as *mut _;
        let raw_window_handle = RawWindowHandle::Wayland(wayland_window_handle);

        // Create the EGL surface.
        let config = self.renderer.egl_context().config();
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            NonZeroU32::new(self.size.width as u32).unwrap(),
            NonZeroU32::new(self.size.height as u32).unwrap(),
        );

        let egl_surface =
            unsafe { config.display().create_window_surface(&config, &surface_attributes)? };

        // Setup layer shell surface.
        let window =
            layer.create_layer_surface(&self.queue, surface, Layer::Overlay, Some("panel"), None);
        window.set_anchor(Anchor::LEFT | Anchor::TOP | Anchor::RIGHT | Anchor::BOTTOM);
        window.set_exclusive_zone(-1);
        self.window = Some(window);

        self.renderer.set_surface(Some(egl_surface));

        Ok(())
    }

    /// Destroy the window.
    pub fn hide(&mut self) {
        self.renderer.set_surface(None);
        self.window = None;
    }

    /// Render the panel.
    pub fn draw(
        &mut self,
        compositor: &CompositorState,
        modules: &mut [&mut dyn Module],
    ) -> Result<()> {
        // Clamp offset, to ensure minimize works immediately.
        self.offset = self.offset.min(self.max_offset());

        let offset = (self.offset * self.scale_factor as f64).min(self.size.height as f64);
        let y_offset = (offset - self.size.height as f64) as i32;
        self.frame_pending = false;

        // Update opaque region.
        let region = Region::new(compositor).ok();
        if let Some((window, region)) = self.window.as_ref().zip(region) {
            let y = (y_offset / self.scale_factor + PANEL_HEIGHT).max(0);
            let width = self.size.width / self.scale_factor;
            let height = offset as i32 / self.scale_factor - y;
            region.add(0, y, width, height);
            window.wl_surface().set_opaque_region(Some(region.wl_region()));
        }

        self.renderer.draw(|renderer| unsafe {
            // Transparently clear entire screen.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Viewport(0, 0, self.size.width, self.size.height);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Setup drawer to render at correct offset.
            let drawer_height = self.size.height - PANEL_HEIGHT * renderer.scale_factor;
            gl::Enable(gl::SCISSOR_TEST);
            gl::Scissor(0, -y_offset, self.size.width, drawer_height);
            gl::Viewport(0, -y_offset, self.size.width, self.size.height);

            // Draw background for the offset viewport.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Draw module grid.
            let mut run = DrawerRun::new(renderer);
            for module in modules.iter_mut().filter_map(|module| module.drawer_module()) {
                run.batch(module);
            }
            run.draw();

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
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor as i16);
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
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor as i16);
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
        (self.size.height / self.scale_factor) as f64
    }

    /// Resize the window.
    fn resize(&mut self, size: Size) {
        self.size = size;

        let scale_factor = self.scale_factor;
        let _ = self.renderer.resize(size, scale_factor);

        // Ensure drawer stays fully open after resize.
        if !self.offsetting && self.offset > 0. {
            self.offset = self.max_offset();
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
    fn new(renderer: &'a mut Renderer) -> Self {
        Self {
            positioner: ModulePositioner::new(renderer.size, renderer.scale_factor as i16),
            rasterizer: &mut renderer.rasterizer,
            text_batcher: &mut renderer.text_batcher,
            rect_batcher: &mut renderer.rect_batcher,
            column: 0,
            row: 0,
        }
    }

    /// Add a drawer module to the run.
    fn batch(&mut self, module: DrawerModule) {
        let _ = match module {
            DrawerModule::Toggle(toggle) => self.batch_toggle(toggle),
            DrawerModule::Slider(slider) => self.batch_slider(slider),
        };
    }

    /// Add a slider to the drawer.
    fn batch_slider(&mut self, slider: &dyn Slider) -> Result<()> {
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
        let tray =
            RectVertex::new(window_width, window_height, x, y, width, height, &MODULE_COLOR_BG);
        for vertex in tray {
            self.rect_batcher.push(0, vertex);
        }

        // Stage slider vertices.
        let slider_width = (width as f64 * slider.get_value()) as i16;
        let slider = RectVertex::new(
            window_width,
            window_height,
            x,
            y,
            slider_width,
            height,
            &MODULE_COLOR_FG,
        );
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
    fn batch_toggle(&mut self, toggle: &dyn Toggle) -> Result<()> {
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
        let color = if toggle.enabled() { MODULE_COLOR_FG } else { MODULE_COLOR_BG };
        let backdrop = RectVertex::new(window_width, window_height, x, y, size, size, &color);
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
    pub fn new(size: Size<f32>, scale_factor: i16) -> Self {
        let size = Size::new(size.width as i16, size.height as i16);

        // Scale constants by DPI scale factor.
        let panel_height = PANEL_HEIGHT as i16 * scale_factor;
        let module_size = MODULE_SIZE as i16 * scale_factor;
        let module_padding = MODULE_PADDING * scale_factor;
        let slider_height = SLIDER_HEIGHT * scale_factor;
        let edge_padding = EDGE_PADDING * scale_factor;

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
fn scale_touch(position: (f64, f64), scale_factor: i32) -> (f64, f64) {
    (position.0 * scale_factor as f64, position.1 * scale_factor as f64)
}
