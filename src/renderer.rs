//! OpenGL rendering.

use std::ffi::CString;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::Once;
use std::{mem, ptr};

use crossfont::Size as FontSize;
use glutin::config::{Api, ConfigTemplateBuilder};
use glutin::context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext, Version};
use glutin::display::Display;
use glutin::prelude::*;
use glutin::surface::{Surface, SurfaceAttributesBuilder, SwapInterval, WindowSurface};
use raw_window_handle::{RawWindowHandle, WaylandWindowHandle};
use smithay_client_toolkit::reexports::client::Proxy;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;

use crate::config::Config;
use crate::geometry::Size;
use crate::gl;
use crate::gl::types::{GLenum, GLfloat, GLshort, GLuint};
use crate::text::GlRasterizer;
use crate::vertex::{GlyphVertex, RectVertex, VertexBatcher};

/// Maximum items to be drawn in a batch.
///
/// We use the closest number to `u16::MAX` dividable by 4 (amount of vertices
/// we push for a glyph), since it's the maximum possible index in
/// `glDrawElements` in GLES2.
const BATCH_MAX: usize = (u16::MAX - u16::MAX % 4) as usize;

const TEXT_VERTEX_SHADER: &str = include_str!("../shaders/text.v.glsl");
const TEXT_FRAGMENT_SHADER: &str = include_str!("../shaders/text.f.glsl");
const RECT_VERTEX_SHADER: &str = include_str!("../shaders/rect.v.glsl");
const RECT_FRAGMENT_SHADER: &str = include_str!("../shaders/rect.f.glsl");

/// OpenGL renderer.
pub struct Renderer {
    rasterizer: Option<GlRasterizer>,
    sized: Option<SizedRenderer>,
    surface: WlSurface,
    display: Display,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(config: &Config, display: Display, surface: WlSurface) -> Self {
        static GL_INIT: Once = Once::new();
        GL_INIT.call_once(|| {
            gl::load_with(|symbol| {
                let symbol = CString::new(symbol).unwrap();
                display.get_proc_address(symbol.as_c_str()).cast()
            });
        });

        let font_size = FontSize::new(config.font.size);
        let rasterizer =
            GlRasterizer::new(&config.font.family, font_size, 1.).expect("rasterizer creation");

        Renderer { display, surface, rasterizer: Some(rasterizer), sized: Default::default() }
    }

    /// Perform drawing with this renderer.
    pub fn draw<F: FnOnce(&mut SizedRenderer)>(&mut self, size: Size<u32>, fun: F) {
        let sized = self.sized(size);
        sized.make_current();

        // Calculate OpenGL projection.
        let scale_x = 2. / size.width as f32;
        let scale_y = -2. / size.height as f32;
        let offset_x = -1.;
        let offset_y = 1.;

        // Update the text renderer's uniform.
        sized.text_batcher.renderer().bind();
        unsafe { gl::Uniform4f(0, offset_x, offset_y, scale_x, scale_y) };

        // Resize OpenGL viewport.
        unsafe { gl::Viewport(0, 0, size.width as i32, size.height as i32) };

        fun(sized);

        unsafe { gl::Flush() };

        sized.swap_buffers();
    }

    /// Get render state requiring a size.
    fn sized(&mut self, size: Size<u32>) -> &mut SizedRenderer {
        // Initialize or resize sized state.
        match &mut self.sized {
            // Resize renderer.
            Some(sized) => sized.resize(size),
            // Create sized state.
            None => {
                let rasterizer = self.rasterizer.take().unwrap();
                self.sized =
                    Some(SizedRenderer::new(&self.display, &self.surface, size, rasterizer));
            },
        }

        self.sized.as_mut().unwrap()
    }
}

/// Render state requiring known size.
///
/// This state is initialized on-demand, to avoid Mesa's issue with resizing
/// before the first draw.
pub struct SizedRenderer {
    pub text_batcher: VertexBatcher<TextRenderer>,
    pub rect_batcher: VertexBatcher<RectRenderer>,
    pub rasterizer: GlRasterizer,

    egl_surface: Surface<WindowSurface>,
    egl_context: PossiblyCurrentContext,

    size: Size<u32>,
}

impl SizedRenderer {
    /// Create sized renderer state.
    fn new(
        display: &Display,
        surface: &WlSurface,
        size: Size<u32>,
        rasterizer: GlRasterizer,
    ) -> Self {
        // Create EGL surface and context and make it current.
        let (egl_surface, egl_context) = Self::create_surface(display, surface, size);

        // Enable blending for text rendering.
        unsafe { gl::Enable(gl::BLEND) };

        Self {
            egl_surface,
            egl_context,
            rasterizer,
            size,
            text_batcher: Default::default(),
            rect_batcher: Default::default(),
        }
    }

    /// Resize the renderer.
    fn resize(&mut self, size: Size<u32>) {
        if self.size == size {
            return;
        }

        // Resize EGL texture.
        self.egl_surface.resize(
            &self.egl_context,
            NonZeroU32::new(size.width).unwrap(),
            NonZeroU32::new(size.height).unwrap(),
        );

        self.size = size;
    }

    /// Make EGL surface current.
    fn make_current(&self) {
        self.egl_context.make_current(&self.egl_surface).unwrap();
    }

    /// Perform OpenGL buffer swap.
    fn swap_buffers(&self) {
        self.egl_surface.swap_buffers(&self.egl_context).unwrap();
    }

    /// Create a new EGL surface.
    fn create_surface(
        display: &Display,
        surface: &WlSurface,
        size: Size<u32>,
    ) -> (Surface<WindowSurface>, PossiblyCurrentContext) {
        assert!(size.width > 0 && size.height > 0);

        // Create EGL config.
        let config_template = ConfigTemplateBuilder::new().with_api(Api::GLES2).build();
        let egl_config = unsafe {
            display
                .find_configs(config_template)
                .ok()
                .and_then(|mut configs| configs.next())
                .unwrap()
        };

        // Create EGL context.
        let context_attributes = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(2, 0))))
            .build(None);
        let egl_context =
            unsafe { display.create_context(&egl_config, &context_attributes).unwrap() };
        let egl_context = egl_context.treat_as_possibly_current();

        let surface = NonNull::new(surface.id().as_ptr().cast()).unwrap();
        let raw_window_handle = WaylandWindowHandle::new(surface);
        let raw_window_handle = RawWindowHandle::Wayland(raw_window_handle);
        let surface_attributes = SurfaceAttributesBuilder::<WindowSurface>::new().build(
            raw_window_handle,
            NonZeroU32::new(size.width).unwrap(),
            NonZeroU32::new(size.height).unwrap(),
        );

        let egl_surface =
            unsafe { display.create_window_surface(&egl_config, &surface_attributes).unwrap() };

        // Ensure rendering never blocks.
        egl_context.make_current(&egl_surface).unwrap();
        egl_surface.set_swap_interval(&egl_context, SwapInterval::DontWait).unwrap();

        (egl_surface, egl_context)
    }
}

/// Abstraction over shader programs.
pub trait RenderProgram: Default {
    /// Type of the vertex used for this program.
    type Vertex;

    /// Make this renderer active for drawing.
    fn bind(&self);
}

/// Renderer for glyphs and SVGs.
pub struct TextRenderer {
    id: GLuint,
    vao: GLuint,
    vbo: GLuint,
    ebo: GLuint,
}

impl Default for TextRenderer {
    fn default() -> Self {
        // Create buffer with all possible vertex indices.
        let mut vertex_indices = Vec::with_capacity(BATCH_MAX / 4 * 6);
        for index in 0..(BATCH_MAX / 4) as u16 {
            let index = index * 4;
            vertex_indices.push(index);
            vertex_indices.push(index + 1);
            vertex_indices.push(index + 3);

            vertex_indices.push(index + 1);
            vertex_indices.push(index + 2);
            vertex_indices.push(index + 3);
        }

        unsafe {
            // Create shaders.
            let vertex_shader = Shader::new(gl::VERTEX_SHADER, TEXT_VERTEX_SHADER);
            let fragment_shader = Shader::new(gl::FRAGMENT_SHADER, TEXT_FRAGMENT_SHADER);

            // Create shader program.
            let id = gl::CreateProgram();
            gl::AttachShader(id, *vertex_shader);
            gl::AttachShader(id, *fragment_shader);
            gl::LinkProgram(id);
            gl::UseProgram(id);

            // Generate VAO.
            let mut vao = 0;
            gl::GenVertexArraysOES(1, &mut vao);
            gl::BindVertexArrayOES(vao);

            // Generate EBO.
            let mut ebo = 0;
            gl::GenBuffers(1, &mut ebo);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, ebo);
            gl::BufferData(
                gl::ELEMENT_ARRAY_BUFFER,
                (vertex_indices.capacity() * mem::size_of::<u16>()) as isize,
                vertex_indices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // Generate VBO.
            let mut vbo = 0;
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (BATCH_MAX * mem::size_of::<GlyphVertex>()) as isize,
                ptr::null(),
                gl::STREAM_DRAW,
            );

            // Glyph position.
            let mut offset = 0;
            gl::VertexAttribPointer(
                0,
                2,
                gl::SHORT,
                gl::FALSE,
                mem::size_of::<GlyphVertex>() as i32,
                offset as *const _,
            );
            gl::EnableVertexAttribArray(0);
            offset += 2 * mem::size_of::<GLshort>();

            // UV position.
            gl::VertexAttribPointer(
                1,
                2,
                gl::FLOAT,
                gl::FALSE,
                mem::size_of::<GlyphVertex>() as i32,
                offset as *const _,
            );
            gl::EnableVertexAttribArray(1);
            offset += 2 * mem::size_of::<GLfloat>();

            // Glyph flags.
            gl::VertexAttribPointer(
                2,
                1,
                gl::FLOAT,
                gl::FALSE,
                mem::size_of::<GlyphVertex>() as i32,
                offset as *const _,
            );
            gl::EnableVertexAttribArray(2);

            Self { id, vao, vbo, ebo }
        }
    }
}

impl RenderProgram for TextRenderer {
    type Vertex = GlyphVertex;

    fn bind(&self) {
        unsafe {
            gl::UseProgram(self.id);
            gl::BindVertexArrayOES(self.vao);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, self.ebo);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BlendFunc(gl::SRC1_COLOR_EXT, gl::ONE_MINUS_SRC1_COLOR_EXT);
        }
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteBuffers(1, &self.ebo);
            gl::DeleteVertexArraysOES(1, &self.vao);
        }
    }
}

/// Renderer for single-color rectangles.
pub struct RectRenderer {
    id: GLuint,
    vao: GLuint,
    vbo: GLuint,
    ebo: GLuint,
}

impl Default for RectRenderer {
    fn default() -> Self {
        // Create buffer with all possible vertex indices.
        let mut vertex_indices = Vec::with_capacity(BATCH_MAX / 4 * 6);
        for index in 0..(BATCH_MAX / 4) as u16 {
            let index = index * 4;
            vertex_indices.push(index);
            vertex_indices.push(index + 1);
            vertex_indices.push(index + 3);

            vertex_indices.push(index + 1);
            vertex_indices.push(index + 2);
            vertex_indices.push(index + 3);
        }

        unsafe {
            // Create shaders.
            let vertex_shader = Shader::new(gl::VERTEX_SHADER, RECT_VERTEX_SHADER);
            let fragment_shader = Shader::new(gl::FRAGMENT_SHADER, RECT_FRAGMENT_SHADER);

            // Create shader program.
            let id = gl::CreateProgram();
            gl::AttachShader(id, *vertex_shader);
            gl::AttachShader(id, *fragment_shader);
            gl::LinkProgram(id);
            gl::UseProgram(id);

            // Generate VAO.
            let mut vao = 0;
            gl::GenVertexArraysOES(1, &mut vao);
            gl::BindVertexArrayOES(vao);

            // Generate EBO.
            let mut ebo = 0;
            gl::GenBuffers(1, &mut ebo);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, ebo);
            gl::BufferData(
                gl::ELEMENT_ARRAY_BUFFER,
                (vertex_indices.capacity() * mem::size_of::<u16>()) as isize,
                vertex_indices.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );

            // Generate VBO.
            let mut vbo = 0;
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (BATCH_MAX * mem::size_of::<GlyphVertex>()) as isize,
                ptr::null(),
                gl::STREAM_DRAW,
            );

            // Rectangle position.
            let mut offset = 0;
            gl::VertexAttribPointer(
                0,
                2,
                gl::FLOAT,
                gl::FALSE,
                mem::size_of::<RectVertex>() as i32,
                offset as *const _,
            );
            gl::EnableVertexAttribArray(0);
            offset += mem::size_of::<GLfloat>() * 2;

            // Rectangle color.
            gl::VertexAttribPointer(
                1,
                4,
                gl::UNSIGNED_BYTE,
                gl::TRUE,
                mem::size_of::<RectVertex>() as i32,
                offset as *const _,
            );
            gl::EnableVertexAttribArray(1);

            Self { id, vao, vbo, ebo }
        }
    }
}

impl RenderProgram for RectRenderer {
    type Vertex = RectVertex;

    fn bind(&self) {
        unsafe {
            gl::UseProgram(self.id);
            gl::BindVertexArrayOES(self.vao);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, self.ebo);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        }
    }
}

impl Drop for RectRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteBuffers(1, &self.ebo);
            gl::DeleteVertexArraysOES(1, &self.vao);
        }
    }
}

struct Shader {
    id: GLuint,
}

impl Deref for Shader {
    type Target = GLuint;

    fn deref(&self) -> &Self::Target {
        &self.id
    }
}

impl Shader {
    fn new(shader_type: GLenum, source: &str) -> Self {
        unsafe {
            let id = gl::CreateShader(shader_type);
            gl::ShaderSource(
                id,
                1,
                [source.as_ptr()].as_ptr() as *const _,
                &(source.len() as i32) as *const _,
            );
            gl::CompileShader(id);

            Self { id }
        }
    }
}

/// OpenGL texture.
pub struct Texture {
    pub id: GLuint,
    pub _width: i32,
    pub _height: i32,
}

impl Texture {
    /// Create a new texture.
    pub fn new(width: i32, height: i32) -> Self {
        let mut id = 0;
        unsafe {
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width,
                height,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                ptr::null(),
            );
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::BindTexture(gl::TEXTURE_2D, 0);
        }

        Self { id, _width: width, _height: height }
    }

    /// Upload buffer to texture.
    pub fn upload_buffer(&self, x: i32, y: i32, width: i32, height: i32, buffer: &[u8]) {
        assert_eq!(width * height * 4, buffer.len() as i32);

        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.id);

            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                x,
                y,
                width,
                height,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                buffer.as_ptr() as *const _,
            );

            gl::BindTexture(gl::TEXTURE_2D, 0);
        }
    }
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteTextures(1, &self.id);
        }
    }
}
