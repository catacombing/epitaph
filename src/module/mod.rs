//! Panel modules.

use crossfont::Metrics;

use crate::text::{GlRasterizer, Svg};
use crate::vertex::{GlVertex, VertexBatcher};
use crate::{Renderer, Size};

pub mod battery;
pub mod cellular;
pub mod clock;
pub mod wifi;

/// Padding to the screen edges.
const EDGE_PADDING: i16 = 5;

/// Padding between modules.
const MODULE_PADDING: i16 = 5;

/// Run of multiple panel modules.
pub struct ModuleRun<'a> {
    batcher: &'a mut VertexBatcher<GlVertex>,
    rasterizer: &'a mut GlRasterizer,
    metrics: &'a mut Metrics,
    size: &'a mut Size<f32>,
    alignment: Alignment,
    width: i16,
}

impl<'a> ModuleRun<'a> {
    pub fn new(renderer: &'a mut Renderer, alignment: Alignment) -> Self {
        Self {
            rasterizer: &mut renderer.rasterizer,
            batcher: &mut renderer.batcher,
            metrics: &mut renderer.metrics,
            size: &mut renderer.size,
            alignment,
            width: 0,
        }
    }

    /// Insert module into run.
    pub fn insert<M: Module>(&mut self, module: M) {
        module.insert(self);
    }

    /// Draw all modules in this run.
    pub fn draw(mut self) {
        // Trim last module padding.
        self.width = self.width.saturating_sub(MODULE_PADDING);

        // Determine vertex offset from left screen edge.
        let x_offset = match self.alignment {
            Alignment::Left => EDGE_PADDING,
            Alignment::Center => (self.size.width as i16 - self.width) / 2,
            Alignment::Right => self.size.width as i16 - self.width - EDGE_PADDING,
        };

        // Update vertex position based on text alignment.
        for vertex in self.batcher.pending() {
            vertex.x += x_offset;
        }

        // Draw all batched vertices.
        let mut batches = self.batcher.batches();
        while let Some(batch) = batches.next() {
            batch.draw();
        }
    }

    /// Add text module to this run.
    pub fn batch_string(&mut self, text: &str) {
        // Calculate Y to center text.
        let y = ((self.size.height as f64 - self.metrics.line_height) / 2.
            + (self.metrics.line_height + self.metrics.descent as f64)) as i16;

        // Batch vertices for all glyphs.
        for glyph in self.rasterizer.rasterize_string(text) {
            for vertex in glyph.vertices(self.width, y).into_iter().flatten() {
                self.batcher.push(glyph.texture_id, vertex);
            }

            self.width += glyph.advance.0 as i16;
        }

        self.width += MODULE_PADDING;
    }

    /// Add SVG module to this run.
    pub fn batch_svg(&mut self, svg: Svg) {
        let svg = match self.rasterizer.rasterize_svg(svg) {
            Ok(svg) => svg,
            Err(err) => {
                eprintln!("SVG rasterization error: {:?}", err);
                return;
            },
        };

        // Calculate Y to center SVG.
        let y = (self.size.height as i16 - svg.height as i16) / 2;

        for vertex in svg.vertices(self.width, y).into_iter().flatten() {
            self.batcher.push(svg.texture_id, vertex);
        }
        self.width += svg.advance.0 as i16;

        self.width += MODULE_PADDING;
    }
}

/// Module run alignment.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Alignment {
    Left,
    Center,
    Right,
}

/// Panel module.
pub trait Module {
    /// Insert this module into a [`ModuleRun`].
    fn insert(&self, run: &mut ModuleRun);
}
