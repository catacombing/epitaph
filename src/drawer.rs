//! Drawer window state.

use smithay::backend::egl::display::EGLDisplay;
use smithay::backend::egl::{EGLContext, EGLSurface};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::shell::layer::{
    Anchor, Layer, LayerState, LayerSurface, LayerSurfaceConfigure,
};
use wayland_egl::WlEglSurface;

use crate::module::{DrawerModule, Module, Slider, Toggle};
use crate::panel::PANEL_HEIGHT;
use crate::renderer::Renderer;
use crate::text::{GlRasterizer, Svg};
use crate::vertex::{GlVertex, VertexBatcher};
use crate::{gl, NativeDisplay, Result, Size, State, GL_ATTRIBUTES, BG};

/// Slider module height.
///
/// This should be less than `MODULE_SIZE`.
const SLIDER_HEIGHT: i16 = MODULE_SIZE as i16 - 16;

/// Padding between drawer modules.
const MODULE_PADDING: i16 = 16;

/// Drawer padding to the screen edges.
const EDGE_PADDING: i16 = 24;

/// Drawer module width and height.
const MODULE_SIZE: u32 = 64;

/// Drawer module icon height.
const ICON_HEIGHT: u32 = 32;

pub struct Drawer {
    window: Option<LayerSurface>,
    queue: QueueHandle<State>,
    touch_module: Option<usize>,
    touch_position: (f64, f64),
    touch_id: Option<i32>,
    display: EGLDisplay,
    frame_pending: bool,
    renderer: Renderer,
    scale_factor: i32,
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
            touch_position: Default::default(),
            touch_module: Default::default(),
            touch_id: Default::default(),
            window: Default::default(),
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

        Ok(())
    }

    /// Destroy the window.
    pub fn hide(&mut self) {
        self.renderer.set_surface(None);
        self.window = None;
    }

    /// Render the panel.
    pub fn draw(&mut self, modules: &mut [&mut dyn Module], mut offset: f64) -> Result<()> {
        offset = (offset * self.scale_factor as f64).min(self.size.height as f64);
        self.frame_pending = false;

        self.renderer.draw(|renderer| unsafe {
            // Transparently clear entire screen.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Viewport(0, 0, self.size.width, self.size.height);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Setup drawer to render at correct offset.
            let drawer_height = self.size.height - PANEL_HEIGHT * renderer.scale_factor;
            let y_offset = (self.size.height as f64 - offset) as i32;
            gl::Enable(gl::SCISSOR_TEST);
            gl::Scissor(0, y_offset, self.size.width, drawer_height);
            gl::Viewport(0, y_offset, self.size.width, self.size.height);

            // Draw background for the offset viewport.
            gl::ClearColor(BG[0], BG[1], BG[2], BG[3]);
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
        surface.frame(&self.queue, surface.clone()).expect("scheduled frame request");
        surface.commit();
    }

    /// Handle touch press events.
    pub fn touch_down(
        &mut self,
        id: i32,
        position: (f64, f64),
        modules: &mut [&mut dyn Module],
    ) -> bool {
        self.touch_position = scale_touch(position, self.scale_factor);
        self.touch_id = Some(id);

        // Find touched module.
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor as i16);
        let (index, x) = match positioner.module_position(modules, self.touch_position) {
            Some((index, x, _)) => (index, x),
            None => return false,
        };
        self.touch_module = Some(index);

        // Update sliders.
        match modules[index].drawer_module() {
            Some(DrawerModule::Slider(slider)) => {
                let _ = slider.set_value(x);
                true
            },
            _ => false,
        }
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

                let _ = slider.set_value(fractional_x);

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
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor as i16);
        if let Some(DrawerModule::Toggle(toggle)) = positioner
            .module_position(modules, self.touch_position)
            .filter(|(index, ..)| Some(*index) == self.touch_module)
            .and_then(|(index, ..)| modules[index].drawer_module())
        {
            let _ = toggle.toggle();
            dirty = true;
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
    }
}

/// Batched drawer module rendering.
struct DrawerRun<'a> {
    batcher: &'a mut VertexBatcher<GlVertex>,
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
            batcher: &mut renderer.batcher,
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
        let width = (self.positioner.slider_size.width / self.positioner.scale_factor) as u32;
        let height = (self.positioner.slider_size.height / self.positioner.scale_factor) as u32;

        // Rasterize slider icon.
        let icon = self.rasterizer.rasterize_svg(slider.svg(), ICON_HEIGHT, None)?;

        // Rasterize slider background.
        let tray = self.rasterizer.rasterize_svg(Svg::ButtonOff, width, height)?;

        // Rasterize slider foreground, if it is non-zero.
        let slider_width = (width as f64 * slider.get_value()) as u32;
        let slider = if slider_width > 0 {
            self.rasterizer.rasterize_svg(Svg::ButtonOn, slider_width, height).ok()
        } else {
            None
        };

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

        for vertex in tray.vertices(x, y).into_iter().flatten() {
            self.batcher.push(tray.texture_id, vertex);
        }

        if let Some(slider) = slider {
            for vertex in slider.vertices(x, y).into_iter().flatten() {
                self.batcher.push(slider.texture_id, vertex);
            }
        }

        // Calculate icon origin.
        let icon_x = x + (self.positioner.slider_size.width - icon.width) / 2;
        let icon_y = y + (self.positioner.slider_size.height - icon.height) / 2;

        for vertex in icon.vertices(icon_x, icon_y).into_iter().flatten() {
            self.batcher.push(icon.texture_id, vertex);
        }

        Ok(())
    }

    /// Add a toggle button to the drawer.
    fn batch_toggle(&mut self, toggle: &dyn Toggle) -> Result<()> {
        let svg = self.rasterizer.rasterize_svg(toggle.svg(), None, ICON_HEIGHT)?;

        let button_svg = if toggle.enabled() { Svg::ButtonOn } else { Svg::ButtonOff };
        let backdrop = self.rasterizer.rasterize_svg(button_svg, MODULE_SIZE, MODULE_SIZE)?;

        // Calculate module origin point.
        let (x, y) = self.positioner.position(self.column, self.row);

        // Calculate icon origin point.
        let icon_x = x + (backdrop.width - svg.width) / 2;
        let icon_y = y + (backdrop.height - svg.height) / 2;

        // Update active column/row.
        self.column += 1;
        if self.column >= self.positioner.columns {
            self.column = 0;
            self.row += 1;
        }

        // Batch icon backdrop.
        for vertex in backdrop.vertices(x, y).into_iter().flatten() {
            self.batcher.push(backdrop.texture_id, vertex);
        }

        // Batch icon.
        for vertex in svg.vertices(icon_x, icon_y).into_iter().flatten() {
            self.batcher.push(svg.texture_id, vertex);
        }

        Ok(())
    }

    /// Draw all modules in this run.
    fn draw(self) {
        let mut batches = self.batcher.batches();
        while let Some(batch) = batches.next() {
            batch.draw();
        }
    }
}

/// Module position calculator.
struct ModulePositioner {
    slider_size: Size<i16>,
    module_padding: i16,
    scale_factor: i16,
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

        Self {
            module_padding,
            edge_padding,
            panel_height,
            scale_factor,
            slider_size,
            module_size,
            columns,
            size,
        }
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
                start_y = end_y;
            }
        }

        None
    }
}

/// Scale touch position by scale factor.
fn scale_touch(position: (f64, f64), scale_factor: i32) -> (f64, f64) {
    (position.0 * scale_factor as f64, position.1 * scale_factor as f64)
}
