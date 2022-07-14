//! OpenGL rendering.

use std::error::Error;
use std::{mem, ptr};

use crossfont::Metrics;
use smithay::backend::egl::{self, EGLContext, EGLSurface};

use crate::gl::types::{GLfloat, GLshort};
use crate::text::GlRasterizer;
use crate::vertex::{GlyphVertex, VertexBatcher};
use crate::{gl, Size};

/// Default font.
const FONT: &str = "Sans";

/// Default font size.
const FONT_SIZE: f32 = 12.;

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
    batcher: VertexBatcher<GlyphVertex>,
    rasterizer: GlRasterizer,
    metrics: Metrics,
    size: Size<f32>,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(context: &EGLContext, surface: &EGLSurface) -> Result<Self, Box<dyn Error>> {
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
            // Setup OpenGL symbol loader.
            gl::load_with(|symbol| egl::get_proc_address(symbol));

            // Enable the OpenGL context.
            context.make_current_with_surface(surface)?;

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

            let mut success = 0;
            gl::GetProgramiv(program, gl::LINK_STATUS, &mut success);

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

            // Set background color and blending.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC1_COLOR_EXT, gl::ONE_MINUS_SRC1_COLOR_EXT);
        }

        let mut rasterizer = GlRasterizer::new(FONT, FONT_SIZE)?;

        // Rasterize any glyph to initialize metrics.
        let _ = rasterizer.rasterize_char(' ');
        let metrics = rasterizer.metrics()?;

        Ok(Renderer {
            rasterizer,
            metrics,
            batcher: VertexBatcher::new(),
            size: Default::default(),
        })
    }

    /// Update viewport size.
    pub fn resize(&mut self, size: Size) {
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
    }

    /// Render all passed icon textures.
    pub fn draw(&mut self) {
        unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // TODO: Just for demonstration purposes.
            let time = chrono::offset::Local::now();
            self.draw_string(&time.format("%H:%M").to_string(), Alignment::Center);

            gl::Flush();
        }
    }

    /// Render text.
    fn draw_string(&mut self, text: &str, alignment: Alignment) {
        let mut x = 0;
        let y = (self.metrics.line_height + self.metrics.descent as f64) as i16;

        // Batch vertices for all glyphs.
        for glyph in self.rasterizer.rasterize_string(text) {
            for vertex in glyph.vertices(x, y).into_iter().flatten() {
                self.batcher.push(glyph.texture_id, vertex);
            }

            x += glyph.advance.0 as i16;
        }

        // Determine text offset from left screen edge.
        let x_offset = match alignment {
            Alignment::Left => 0,
            Alignment::Center => (self.size.width as i16 - x) / 2,
            Alignment::Right => self.size.width as i16 - x,
        };

        // Update vertex position based on text alignment.
        if x_offset != 0 {
            for vertex in self.batcher.pending() {
                vertex.x += x_offset;
            }
        }

        self.draw_batches();
    }

    /// Render all staged vertices.
    fn draw_batches(&mut self) {
        let mut batches = self.batcher.batches();
        while let Some(batch) = batches.next() {
            batch.draw();
        }
    }
}

/// Text alignment.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Alignment {
    Left,
    Center,
    Right,
}
