//! OpenGL rendering.

use std::num::NonZeroU32;
use std::ops::Deref;
use std::{mem, ptr};

use crossfont::Size as FontSize;
use glutin::api::egl::context::{NotCurrentContext, PossiblyCurrentContext};
use glutin::api::egl::surface::Surface;
use glutin::prelude::*;
use glutin::surface::WindowSurface;

use crate::config::colors::BG;
use crate::config::font::{FONT, FONT_SIZE};
use crate::gl::types::{GLenum, GLfloat, GLshort, GLuint};
use crate::text::GlRasterizer;
use crate::vertex::{GlyphVertex, RectVertex, VertexBatcher};
use crate::{Result, Size, gl};

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
    pub text_batcher: VertexBatcher<TextRenderer>,
    pub rect_batcher: VertexBatcher<RectRenderer>,
    pub rasterizer: GlRasterizer,
    pub scale_factor: f64,
    pub size: Size<f32>,

    egl_surface: Surface<WindowSurface>,
    egl_context: PossiblyCurrentContext,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(
        egl_context: NotCurrentContext,
        egl_surface: Surface<WindowSurface>,
        scale_factor: f64,
    ) -> Result<Self> {
        unsafe {
            // Enable the OpenGL context.
            let egl_context = egl_context.make_current_surfaceless()?;

            // Set background color and blending.
            let [r, g, b] = BG.as_f32();
            gl::ClearColor(r, g, b, 1.);
            gl::Enable(gl::BLEND);

            let font_size = FontSize::new(FONT_SIZE);

            Ok(Renderer {
                scale_factor,
                egl_surface,
                egl_context,
                rasterizer: GlRasterizer::new(FONT, font_size, scale_factor)?,
                text_batcher: Default::default(),
                rect_batcher: Default::default(),
                size: Default::default(),
            })
        }
    }

    /// Update viewport size.
    pub fn resize(&mut self, size: Size, scale_factor: f64) -> Result<()> {
        // XXX: Resize here **must** be performed before making the EGL context current,
        // to avoid locking the back buffer and delaying the resize by one
        // frame.
        self.egl_surface.resize(
            &self.egl_context,
            NonZeroU32::new(size.width as u32).unwrap(),
            NonZeroU32::new(size.height as u32).unwrap(),
        );

        self.bind()?;

        unsafe { gl::Viewport(0, 0, size.width, size.height) };
        self.size = size.into();

        // Calculate OpenGL projection.
        let scale_x = 2. / size.width as f32;
        let scale_y = -2. / size.height as f32;
        let offset_x = -1.;
        let offset_y = 1.;

        // Update the text renderer's uniform.
        self.text_batcher.renderer().bind();
        unsafe {
            gl::Uniform4f(0, offset_x, offset_y, scale_x, scale_y);
        }

        // Update rasterizer's scale factor.
        self.rasterizer.set_scale_factor(scale_factor);
        self.scale_factor = scale_factor;

        Ok(())
    }

    /// Perform drawing with this renderer.
    pub fn draw<F: FnMut(&mut Renderer) -> Result<()>>(&mut self, mut fun: F) -> Result<()> {
        self.bind()?;

        fun(self)?;

        unsafe { gl::Flush() };

        self.egl_surface.swap_buffers(&self.egl_context)?;

        Ok(())
    }

    /// Bind this renderer's program and buffers.
    fn bind(&self) -> Result<()> {
        self.egl_context.make_current(&self.egl_surface)?;
        Ok(())
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
            // Create vertex shader.
            let vertex_shader = Shader::new(gl::VERTEX_SHADER, TEXT_VERTEX_SHADER);

            // Create fragment shader.
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
