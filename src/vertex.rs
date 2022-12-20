//! OpenGL vertex batching.

use std::{cmp, mem, ptr};

use crate::gl;
use crate::gl::types::GLuint;
use crate::renderer::RenderProgram;
use crate::text::GlSubTexture;

/// Maximum items to be drawn in a batch.
///
/// We use the closest number to `u16::MAX` dividable by 4 (amount of vertices
/// we push for a subtexture), since it's the maximum possible index in
/// `glDrawElements` in GLES2.
const MAX_BATCH_SIZE: usize = (u16::MAX - u16::MAX % 4) as usize;

/// Batch vertices by texture ID.
///
/// Groups together multiple vertices with the same texture ID into a rendering
/// batch and limits the maximum size of each batch.
pub struct VertexBatcher<R: RenderProgram> {
    texture_ids: Vec<GLuint>,
    vertices: Vec<R::Vertex>,
    renderer: R,
}

impl<R: RenderProgram> Default for VertexBatcher<R> {
    fn default() -> Self {
        Self {
            texture_ids: Default::default(),
            vertices: Default::default(),
            renderer: Default::default(),
        }
    }
}

impl<R: RenderProgram> VertexBatcher<R> {
    /// Add a vertex to the batcher.
    pub fn push(&mut self, texture_id: GLuint, vertex: R::Vertex) {
        self.texture_ids.push(texture_id);
        self.vertices.push(vertex);
    }

    /// Get all vertex batches.
    pub fn batches(&mut self) -> VertexBatches<'_, R> {
        sort_multiple(&mut self.texture_ids, &mut self.vertices);

        VertexBatches {
            texture_ids: &mut self.texture_ids,
            vertices: &mut self.vertices,
            renderer: &self.renderer,
            offset: 0,
        }
    }

    /// Get pending vertices.
    pub fn pending(&mut self) -> &mut [R::Vertex] {
        &mut self.vertices
    }
}

/// Iterator over batched vertex groups.
pub struct VertexBatches<'a, R: RenderProgram> {
    texture_ids: &'a mut Vec<GLuint>,
    vertices: &'a mut Vec<R::Vertex>,
    offset: usize,
    renderer: &'a R,
}

impl<'a, R: RenderProgram> Drop for VertexBatches<'a, R> {
    fn drop(&mut self) {
        self.texture_ids.clear();
        self.vertices.clear();
    }
}

impl<'a, R: RenderProgram> VertexBatches<'a, R> {
    /// Get the next vertex batch.
    pub fn next(&mut self) -> Option<VertexBatch<'_, R>> {
        let vertex_count = self.vertices.len();
        if self.offset >= vertex_count {
            return None;
        }

        // Group all vertices up to `MAX_BATCH_SIZE` with identical texture ID.
        let texture_id = self.texture_ids[self.offset];
        let max_size = cmp::min(vertex_count - self.offset, MAX_BATCH_SIZE);
        let batch_size = self.texture_ids[self.offset..self.offset + max_size]
            .iter()
            .position(|id| id != &texture_id)
            .unwrap_or(max_size);
        let batch_end = self.offset + batch_size;

        let old_offset = mem::replace(&mut self.offset, batch_end);

        Some(VertexBatch {
            texture_id,
            vertices: &self.vertices[old_offset..self.offset],
            renderer: self.renderer,
        })
    }
}

/// Batch of vertices with consistent resource ID.
pub struct VertexBatch<'a, R: RenderProgram> {
    texture_id: GLuint,
    vertices: &'a [R::Vertex],
    renderer: &'a R,
}

impl<'a, R: RenderProgram> VertexBatch<'a, R> {
    /// Render this batch.
    pub fn draw(&self) {
        self.renderer.bind();

        let vertex_count = self.vertices.len();
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.texture_id);

            gl::BufferSubData(
                gl::ARRAY_BUFFER,
                0,
                (vertex_count * mem::size_of::<R::Vertex>()) as isize,
                self.vertices.as_ptr() as *const _,
            );

            let num_indices = (vertex_count / 4 * 6) as i32;
            gl::DrawElements(gl::TRIANGLES, num_indices, gl::UNSIGNED_SHORT, ptr::null());
        }
    }
}

impl GlSubTexture {
    /// OpenGL vertices for this subtexture.
    pub fn vertices(&self, x: i16, y: i16) -> Option<[GlyphVertex; 4]> {
        if self.width == 0 || self.height == 0 {
            return None;
        }

        let x = x + self.left;
        let y = y - self.top;

        let flags = if self.multicolor { 1. } else { 0. };

        // Bottom-Left vertex.
        let bottom_left = GlyphVertex {
            x,
            y: y + self.height,
            u: self.uv_left,
            v: self.uv_bot + self.uv_height,
            flags,
        };

        // Top-Left vertex.
        let top_left = GlyphVertex { x, y, u: self.uv_left, v: self.uv_bot, flags };

        // Top-Right vertex.
        let top_right = GlyphVertex {
            x: x + self.width,
            y,
            u: self.uv_left + self.uv_width,
            v: self.uv_bot,
            flags,
        };

        // Bottom-Right vertex.
        let bottom_right = GlyphVertex {
            x: x + self.width,
            y: y + self.height,
            u: self.uv_left + self.uv_width,
            v: self.uv_bot + self.uv_height,
            flags,
        };

        Some([bottom_left, top_left, top_right, bottom_right])
    }
}

/// Vertex for the text shader.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct GlyphVertex {
    // Vertex position.
    pub x: i16,
    pub y: i16,

    // Offsets into Atlas.
    pub u: f32,
    pub v: f32,

    // Vertex flags.
    pub flags: f32,
}

/// Vertex for the rectangle shader.
#[repr(C)]
pub struct RectVertex {
    // Vertex position.
    pub x: f32,
    pub y: f32,

    // Vertex color.
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl RectVertex {
    pub fn new(
        window_width: i16,
        window_height: i16,
        x: i16,
        y: i16,
        width: i16,
        height: i16,
        color: &[u8; 4],
    ) -> [Self; 4] {
        // Calculate rectangle vertex positions in normalized device coordinates.
        // NDC range from -1 to +1, with Y pointing up.
        let half_width = window_width as f32 / 2.;
        let half_height = window_height as f32 / 2.;
        let x = x as f32 / half_width - 1.;
        let y = -y as f32 / half_height + 1.;
        let width = width as f32 / half_width;
        let height = height as f32 / half_height;

        let [r, g, b, a] = *color;
        [
            RectVertex { x, y, r, g, b, a },
            RectVertex { x, y: y - height, r, g, b, a },
            RectVertex { x: x + width, y: y - height, r, g, b, a },
            RectVertex { x: x + width, y, r, g, b, a },
        ]
    }
}

/// Insertion sort for multiple arrays.
///
/// This will use `v1` as a discriminant for sorting and perform the same
/// permutations on `v2`.
pub fn sort_multiple<T, U>(v1: &mut [T], v2: &mut [U])
where
    T: Ord,
{
    let len = v1.len();
    for i in (0..len - 1).rev() {
        if v1[i] <= v1[i + 1] {
            continue;
        }

        let mut j = i;
        loop {
            v1.swap(j, j + 1);
            v2.swap(j, j + 1);

            j += 1;

            if j + 1 >= len || v1[j] <= v1[j + 1] {
                break;
            }
        }
    }
}
