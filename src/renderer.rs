//! OpenGL rendering.

use std::{mem, ptr};

use smithay::backend::egl::{EGLContext, EGLSurface};

use crate::gl::types::{GLfloat, GLshort, GLuint};
use crate::text::GlRasterizer;
use crate::vertex::{GlVertex, VertexBatcher};
use crate::{gl, Result, Size};

/// Default font.
const FONT: &str = "Sans";

/// Default font size.
const FONT_SIZE: f32 = 6.;

/// Maximum items to be drawn in a batch.
///
/// We use the closest number to `u16::MAX` dividable by 4 (amount of vertices
/// we push for a glyph), since it's the maximum possible index in
/// `glDrawElements` in GLES2.
const BATCH_MAX: usize = (u16::MAX - u16::MAX % 4) as usize;

const VERTEX_SHADER: &str = include_str!("../shaders/vertex.glsl");
const FRAGMENT_SHADER: &str = include_str!("../shaders/fragment.glsl");

/// OpenGL renderer.
pub struct Renderer {
    pub batcher: VertexBatcher<GlVertex>,
    pub rasterizer: GlRasterizer,
    pub scale_factor: i32,
    pub size: Size<f32>,

    egl_surface: Option<EGLSurface>,
    egl_context: EGLContext,
    program: GLuint,
    vao: GLuint,
    vbo: GLuint,
    ebo: GLuint,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(egl_context: EGLContext, scale_factor: i32) -> Result<Self> {
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
            // Enable the OpenGL context.
            egl_context.make_current()?;

            // Create vertex shader.
            let vertex_shader = gl::CreateShader(gl::VERTEX_SHADER);
            gl::ShaderSource(
                vertex_shader,
                1,
                [VERTEX_SHADER.as_ptr()].as_ptr() as *const _,
                &(VERTEX_SHADER.len() as i32) as *const _,
            );
            gl::CompileShader(vertex_shader);

            // Create fragment shader.
            let fragment_shader = gl::CreateShader(gl::FRAGMENT_SHADER);
            gl::ShaderSource(
                fragment_shader,
                1,
                [FRAGMENT_SHADER.as_ptr()].as_ptr() as *const _,
                &(FRAGMENT_SHADER.len() as i32) as *const _,
            );
            gl::CompileShader(fragment_shader);

            // Create shader program.
            let program = gl::CreateProgram();
            gl::AttachShader(program, vertex_shader);
            gl::AttachShader(program, fragment_shader);
            gl::LinkProgram(program);
            gl::UseProgram(program);

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
                (BATCH_MAX * mem::size_of::<GlVertex>()) as isize,
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
                mem::size_of::<GlVertex>() as i32,
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
                mem::size_of::<GlVertex>() as i32,
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
                mem::size_of::<GlVertex>() as i32,
                offset as *const _,
            );
            gl::EnableVertexAttribArray(2);

            // Set background color and blending.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC1_COLOR_EXT, gl::ONE_MINUS_SRC1_COLOR_EXT);

            Ok(Renderer {
                scale_factor,
                egl_context,
                program,
                vao,
                vbo,
                ebo,
                rasterizer: GlRasterizer::new(FONT, FONT_SIZE, scale_factor)?,
                egl_surface: Default::default(),
                batcher: Default::default(),
                size: Default::default(),
            })
        }
    }

    /// Update viewport size.
    pub fn resize(&mut self, size: Size, scale_factor: i32) -> Result<()> {
        // XXX: Resize here **must** be performed before making the EGL context current,
        // to avoid locking the back buffer and delaying the resize by one
        // frame.
        if let Some(egl_surface) = &self.egl_surface {
            egl_surface.resize(size.width, size.height, 0, 0);
        }

        self.bind()?;

        unsafe { gl::Viewport(0, 0, size.width, size.height) };
        self.size = size.into();

        // Calculate OpenGL projection.
        let scale_x = 2. / size.width as f32;
        let scale_y = -2. / size.height as f32;
        let offset_x = -1.;
        let offset_y = 1.;

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

        if let Some(egl_surface) = &self.egl_surface {
            egl_surface.swap_buffers(None)?;
        }

        Ok(())
    }

    /// Get the renderer's EGL context.
    pub fn egl_context(&self) -> &EGLContext {
        &self.egl_context
    }

    /// Update the renderer's active EGL surface.
    pub fn set_surface(&mut self, egl_surface: Option<EGLSurface>) {
        self.egl_surface = egl_surface;
    }

    /// Bind this renderer's program and buffers.
    fn bind(&self) -> Result<&EGLSurface> {
        let egl_surface = match &self.egl_surface {
            Some(egl_surface) => egl_surface,
            None => return Err("Attempted to bind EGL context without surface".into()),
        };

        unsafe {
            self.egl_context.make_current_with_surface(egl_surface)?;
            gl::UseProgram(self.program);
            gl::BindVertexArrayOES(self.vao);
            gl::BindBuffer(gl::ELEMENT_ARRAY_BUFFER, self.ebo);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
        }

        Ok(egl_surface)
    }
}
