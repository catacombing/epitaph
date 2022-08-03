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

use crate::module::Module;
use crate::panel::PANEL_HEIGHT;
use crate::renderer::Renderer;
use crate::text::{GlRasterizer, Svg};
use crate::vertex::{GlVertex, VertexBatcher};
use crate::{gl, NativeDisplay, Size, State, GL_ATTRIBUTES};

/// Padding between drawer modules.
const MODULE_PADDING: i16 = 16;

/// Drawer padding to the screen edges.
const EDGE_PADDING: i16 = 24;

/// Drawer module width and height.
const MODULE_SIZE: i16 = 64;

/// Drawer module icon width.
const ICON_WIDTH: u32 = 32;

/// Convenience result wrapper.
type Result<T> = StdResult<T, Box<dyn Error>>;

pub struct Drawer {
    window: Option<LayerSurface>,
    queue: QueueHandle<State>,
    touch_position: (i16, i16),
    touch_start: (i16, i16),
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
            touch_start: Default::default(),
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
    pub fn draw(&mut self, modules: &[Box<dyn Module>], mut offset: f64) -> Result<()> {
        offset = (offset * self.scale_factor as f64).min(self.size.height as f64);
        self.frame_pending = false;

        self.renderer.draw(|renderer| unsafe {
            // Transparently clear entire screen.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Viewport(0, 0, self.size.width, self.size.height);
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // Setup drawer to render at correct offset.
            let y_offset = (self.size.height as f64 - offset) as i32;
            gl::Enable(gl::SCISSOR_TEST);
            gl::Scissor(0, y_offset, self.size.width, self.size.height);
            gl::Viewport(0, y_offset, self.size.width, self.size.height);

            // Draw background for the offset viewport.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // TODO: Enable when modules are updated without blocking.
            //
            // Draw panel at top of drawer.
            // let mut size = renderer.size;
            // size.height = (PANEL_HEIGHT * renderer.scale_factor) as f32;
            // Panel::draw_modules(renderer, size)?;

            // Draw module grid.
            let mut run = DrawerRun::new(renderer);
            for (svg, active) in modules.iter().filter_map(|module| module.drawer_button()) {
                run.batch_toggle(svg, active);
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
    pub fn touch_down(&mut self, id: i32, position: (f64, f64)) {
        self.touch_position = scale_touch(position, self.scale_factor);
        self.touch_start = scale_touch(position, self.scale_factor);
        self.touch_id = Some(id);
    }

    /// Handle touch motion events.
    pub fn touch_motion(&mut self, id: i32, position: (f64, f64)) {
        if Some(id) == self.touch_id {
            self.touch_position = scale_touch(position, self.scale_factor);
        }
    }

    /// Handle touch release events.
    pub fn touch_up(&mut self, id: i32, modules: &mut [Box<dyn Module>]) {
        if Some(id) != self.touch_id {
            return;
        }
        self.touch_id = None;

        // Calculate touched module.
        let positioner = ModulePositioner::new(self.size.into(), self.scale_factor as i16);
        let start_cell = positioner.cell(self.touch_start.0, self.touch_start.1);
        let end_cell = positioner.cell(self.touch_position.0, self.touch_position.1);

        // Find clicked module index.
        let index = match start_cell.zip(end_cell) {
            Some((start, end)) if start == end => start.0 + start.1 * positioner.columns as usize,
            _ => return,
        };

        // Toggle buttons at click location.
        if let Some(module) =
            modules.iter_mut().filter(|module| module.drawer_button().is_some()).nth(index)
        {
            module.toggle();

            // TODO: Unnecessary if actual update comes through epoll or some shit?
            // TODO: Should buttons update immediately or only through backend?
            let _ = self.draw(modules, self.size.height as f64);
        }
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
pub struct DrawerRun<'a> {
    batcher: &'a mut VertexBatcher<GlVertex>,
    rasterizer: &'a mut GlRasterizer,
    positioner: ModulePositioner,
    column: i16,
    row: i16,
}

impl<'a> DrawerRun<'a> {
    pub fn new(renderer: &'a mut Renderer) -> Self {
        Self {
            positioner: ModulePositioner::new(renderer.size, renderer.scale_factor as i16),
            rasterizer: &mut renderer.rasterizer,
            batcher: &mut renderer.batcher,
            column: 0,
            row: 0,
        }
    }

    /// Add a toggle button to the drawer.
    pub fn batch_toggle(&mut self, svg: Svg, active: bool) {
        let svg = match self.rasterizer.rasterize_svg(svg, ICON_WIDTH) {
            Ok(svg) => svg,
            Err(err) => {
                eprintln!("SVG rasterization error: {:?}", err);
                return;
            },
        };

        let button_svg = if active { Svg::ButtonOn } else { Svg::ButtonOff };

        let backdrop = match self.rasterizer.rasterize_svg(button_svg, MODULE_SIZE as u32) {
            Ok(svg) => svg,
            Err(err) => {
                eprintln!("SVG rasterization error: {:?}", err);
                return;
            },
        };

        // Calculate module origin point.
        let (x, y) = self.positioner.position(self.column, self.row);

        // Calculate icon origin point.
        let icon_x = x + (backdrop.width - svg.width) / 2;
        let icon_y = y + (backdrop.height - svg.height) / 2;

        // Update active column/line.
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
    }

    /// Draw all modules in this run.
    pub fn draw(self) {
        let mut batches = self.batcher.batches();
        while let Some(batch) = batches.next() {
            batch.draw();
        }
    }
}

/// Module position calculator.
struct ModulePositioner {
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
        let module_padding = MODULE_PADDING * scale_factor;
        let edge_padding = EDGE_PADDING * scale_factor;
        let panel_height = PANEL_HEIGHT as i16 * scale_factor;
        let module_size = MODULE_SIZE * scale_factor;

        let content_width = size.width - edge_padding * 2;
        let padded_module_size = module_size + module_padding;
        let columns = (content_width + module_padding) / padded_module_size;
        let edge_padding = (size.width + module_padding - columns * padded_module_size) / 2;

        Self { module_padding, edge_padding, panel_height, module_size, columns, size }
    }

    /// Get cell origin point.
    fn position(&self, column: i16, row: i16) -> (i16, i16) {
        let padded_module_size = self.module_size + self.module_padding;
        let x = self.edge_padding + column * padded_module_size;
        let y = self.panel_height + self.edge_padding + row * padded_module_size;

        (x, y)
    }

    /// Get row/column in module grid from position.
    fn cell(&self, x: i16, y: i16) -> Option<(usize, usize)> {
        let padded_module_size = self.module_size + self.module_padding;

        // Get X/Y relative to module cell.
        let relative_x = (x - self.edge_padding) % padded_module_size;
        let relative_y = (y - self.panel_height - self.edge_padding) % padded_module_size;

        // Filter clicks inside padding.
        if x < self.edge_padding
            || x >= (self.size.width - self.edge_padding)
            || y < (self.panel_height + self.edge_padding)
            || y >= (self.size.height - self.edge_padding)
            || relative_x < self.module_padding
            || relative_x >= padded_module_size
            || relative_y < self.module_padding
            || relative_y >= padded_module_size
        {
            return None;
        }

        // Calculate click column/row.
        let column = (x - self.edge_padding) / padded_module_size;
        let row = (y - self.panel_height - self.edge_padding) / padded_module_size;

        Some((column as usize, row as usize))
    }
}

/// Scale touch position by scale factor.
fn scale_touch(position: (f64, f64), scale_factor: i32) -> (i16, i16) {
    (position.0 as i16 * scale_factor as i16, position.1 as i16 * scale_factor as i16)
}
