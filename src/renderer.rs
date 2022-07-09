//! OpenGL rendering.

use smithay::backend::egl::{self, EGLContext, EGLSurface};

use crate::{gl, Size};

/// OpenGL renderer.
#[derive(Debug)]
pub struct Renderer {
    size: Size<f32>,
}

impl Renderer {
    /// Initialize a new renderer.
    pub fn new(
        context: &EGLContext,
        surface: &EGLSurface,
    ) -> Self {
        unsafe {
            // Setup OpenGL symbol loader.
            gl::load_with(|symbol| egl::get_proc_address(symbol));

            // Enable the OpenGL context.
            context.make_current_with_surface(surface).expect("Unable to enable OpenGL context");

            // Generate VBO.
            let mut vbo = 0;
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

            // Set background color and blending.
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

            Renderer {
                size: Default::default(),
            }
        }
    }

    /// Render all passed icon textures.
    pub fn draw(&self) {
        unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);

            // TODO: Render Stuff

            gl::Flush();
        }
    }

    /// Update viewport size.
    pub fn resize(&mut self, size: Size) {
        unsafe { gl::Viewport(0, 0, size.width, size.height) };
        self.size = size.into();
    }
}

/// OpenGL texture.
#[derive(Debug, Copy, Clone)]
pub struct Texture {
    pub width: usize,
    pub height: usize,
    id: u32,
}

impl Default for Texture {
    fn default() -> Self {
        Texture::new(&[], 0, 0)
    }
}

impl Texture {
    /// Load a buffer as texture into OpenGL.
    pub fn new(buffer: &[u8], width: usize, height: usize) -> Self {
        assert!(buffer.len() == width * height * 4);

        unsafe {
            let mut id = 0;
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                width as i32,
                height as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE as u32,
                buffer.as_ptr() as *const _,
            );
            gl::GenerateMipmap(gl::TEXTURE_2D);
            gl::BindTexture(gl::TEXTURE_2D, 0);
            Self { id, width, height }
        }
    }
}
