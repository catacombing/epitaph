//! OpenGL vertex batching.

use std::{cmp, mem, ptr};

use crate::gl;
use crate::gl::types::GLuint;
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
pub struct VertexBatcher<V> {
    texture_ids: Vec<GLuint>,
    vertices: Vec<V>,
}

impl<V> Default for VertexBatcher<V> {
    fn default() -> Self {
        Self { texture_ids: Vec::new(), vertices: Vec::new() }
    }
}

impl<V> VertexBatcher<V> {
    /// Add a vertex to the batcher.
    pub fn push(&mut self, texture_id: GLuint, vertex: V) {
        self.texture_ids.push(texture_id);
        self.vertices.push(vertex);
    }

    /// Get all vertex batches.
    pub fn batches(&mut self) -> VertexBatches<'_, V> {
        sort_multiple(&mut self.texture_ids, &mut self.vertices);

        VertexBatches {
            texture_ids: &mut self.texture_ids,
            vertices: &mut self.vertices,
            offset: 0,
        }
    }

    /// Get pending vertices.
    pub fn pending(&mut self) -> &mut [V] {
        &mut self.vertices
    }
}

/// Iterator over batched vertex groups.
pub struct VertexBatches<'a, V> {
    texture_ids: &'a mut Vec<GLuint>,
    vertices: &'a mut Vec<V>,
    offset: usize,
}

impl<'a, V> Drop for VertexBatches<'a, V> {
    fn drop(&mut self) {
        self.texture_ids.clear();
        self.vertices.clear();
    }
}

impl<'a, V> VertexBatches<'a, V> {
    /// Get the next vertex batch.
    pub fn next(&mut self) -> Option<VertexBatch<'_, V>> {
        let vertex_count = self.vertices.len();
        if self.offset >= vertex_count {
            return None;
        }

        // Group all vertices up to `MAX_BATCH_SIZE` with identical texture ID.
        let texture_id = self.texture_ids[self.offset];
        let max_size = cmp::min(vertex_count, MAX_BATCH_SIZE);
        let batch_size = self.texture_ids[self.offset..max_size]
            .iter()
            .position(|id| id != &texture_id)
            .unwrap_or(max_size);
        let batch_end = self.offset + batch_size;

        let old_offset = mem::replace(&mut self.offset, batch_end);

        Some(VertexBatch { vertices: &self.vertices[old_offset..self.offset], texture_id })
    }
}

/// Batch of vertices with consistent texture ID.
pub struct VertexBatch<'a, V> {
    texture_id: GLuint,
    vertices: &'a [V],
}

impl<'a, V> VertexBatch<'a, V> {
    /// Render this batch.
    pub fn draw(&self) {
        let vertex_count = self.vertices.len();
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.texture_id);

            gl::BufferSubData(
                gl::ARRAY_BUFFER,
                0,
                (vertex_count * mem::size_of::<GlVertex>()) as isize,
                self.vertices.as_ptr() as *const _,
            );

            let num_indices = (vertex_count / 4 * 6) as i32;
            gl::DrawElements(gl::TRIANGLES, num_indices, gl::UNSIGNED_SHORT, ptr::null());
        }
    }
}

impl GlSubTexture {
    /// OpenGL vertices for this subtexture.
    pub fn vertices(&self, x: i16, y: i16) -> Option<[GlVertex; 4]> {
        if self.width == 0 || self.height == 0 {
            return None;
        }

        let x = x + self.left;
        let y = y - self.top;

        let flags = if self.multicolor { 1. } else { 0. };

        // Bottom-Left vertex.
        let bottom_left = GlVertex {
            x,
            y: y + self.height,
            u: self.uv_left,
            v: self.uv_bot + self.uv_height,
            flags,
        };

        // Top-Left vertex.
        let top_left = GlVertex { x, y, u: self.uv_left, v: self.uv_bot, flags };

        // Top-Right vertex.
        let top_right = GlVertex {
            x: x + self.width,
            y,
            u: self.uv_left + self.uv_width,
            v: self.uv_bot,
            flags,
        };

        // Bottom-Right vertex.
        let bottom_right = GlVertex {
            x: x + self.width,
            y: y + self.height,
            u: self.uv_left + self.uv_width,
            v: self.uv_bot + self.uv_height,
            flags,
        };

        Some([bottom_left, top_left, top_right, bottom_right])
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct GlVertex {
    // Vertex position.
    pub x: i16,
    pub y: i16,

    // Offsets into Atlas.
    pub u: f32,
    pub v: f32,

    // Vertex flags.
    pub flags: f32,
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
