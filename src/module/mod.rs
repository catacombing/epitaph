//! Panel modules.

use std::error::Error;

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
    size: &'a mut Size<f32>,
    alignment: Alignment,
    scale_factor: i16,
    metrics: Metrics,
    width: i16,
}

impl<'a> ModuleRun<'a> {
    pub fn new(renderer: &'a mut Renderer, alignment: Alignment) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            scale_factor: renderer.scale_factor as i16,
            metrics: renderer.rasterizer.metrics()?,
            rasterizer: &mut renderer.rasterizer,
            batcher: &mut renderer.batcher,
            size: &mut renderer.size,
            alignment,
            width: 0,
        })
    }

    /// Insert module into run.
    pub fn insert<M: Module>(&mut self, module: M) {
        module.insert(self);
    }

    /// Draw all modules in this run.
    pub fn draw(mut self) {
        // Trim last module padding.
        self.width = self.width.saturating_sub(self.module_padding());

        // Determine vertex offset from left screen edge.
        let x_offset = match self.alignment {
            Alignment::Center => (self.size.width as i16 - self.width) / 2,
            Alignment::Right => self.size.width as i16 - self.width - self.edge_padding(),
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

        self.width += self.module_padding();
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

        self.width += self.module_padding();
    }

    /// Module padding with scale factor applied.
    fn module_padding(&self) -> i16 {
        MODULE_PADDING * self.scale_factor
    }

    /// Edge padding with scale factor applied.
    fn edge_padding(&self) -> i16 {
        EDGE_PADDING * self.scale_factor
    }
}

/// Module run alignment.
#[derive(Copy, Clone, PartialEq, Eq)]
pub enum Alignment {
    Center,
    Right,
}

/// Panel module.
pub trait Module {
    /// Insert this module into a [`ModuleRun`].
    fn insert(&self, run: &mut ModuleRun);
}
