//! OpenGL text rendering.

use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::{cmp, mem};

use crossfont::{
    BitmapBuffer, FontDesc, FontKey, GlyphKey, Metrics, Rasterize, RasterizedGlyph, Rasterizer,
    Size as FontSize, Slant, Style, Weight,
};
use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

use crate::Result;
use crate::gl::types::GLuint;
use crate::renderer::Texture;

/// Width and height of the glyph atlas texture.
///
/// 4096 is the maximum permitted texture size on the PinePhone.
const ATLAS_SIZE: i32 = 4096;

/// Cached OpenGL rasterization.
pub struct GlRasterizer {
    // OpenGL subtexture caching.
    cache: HashMap<CacheKey, GlSubTexture>,
    atlas: Atlas,

    // FreeType font rasterization.
    metrics: Option<Metrics>,
    rasterizer: Rasterizer,
    font_name: String,
    size: FontSize,
    font: FontKey,

    // DPI scale factor.
    scale_factor: f64,
}

impl GlRasterizer {
    pub fn new(
        font_name: impl Into<String>,
        size: impl Into<FontSize>,
        scale_factor: f64,
    ) -> Result<Self> {
        let font_name = font_name.into();
        let size = size.into();

        // Create FreeType rasterizer.
        let mut rasterizer = Rasterizer::new()?;

        // Load font at the requested size.
        let font = Self::load_font(&mut rasterizer, &font_name, size, scale_factor)?;

        Ok(Self {
            scale_factor,
            rasterizer,
            font_name,
            font,
            size,
            metrics: Default::default(),
            atlas: Default::default(),
            cache: Default::default(),
        })
    }

    /// Update the DPI scale factor.
    pub fn set_scale_factor(&mut self, scale_factor: f64) {
        // Avoid clearing all caches when factor didn't change.
        if self.scale_factor == scale_factor {
            return;
        }
        self.scale_factor = scale_factor;

        // Load font at new size.
        self.font = Self::load_font(&mut self.rasterizer, &self.font_name, self.size, scale_factor)
            .unwrap_or(self.font);

        // Clear glyph cache and drop all atlas textures.
        self.atlas = Atlas::default();
        self.cache = HashMap::new();

        // Clear font metrics.
        self.metrics = None;
    }

    /// Rasterize each glyph in a string.
    ///
    /// Returns an iterator over all glyphs. The advance stored on each glyph
    /// has the correct kerning applied already.
    ///
    /// If any of the glyphs cannot be rasterized, all glyphs up to that point
    /// will be returned.
    pub fn rasterize_string<'a>(
        &'a mut self,
        text: &'a str,
    ) -> impl Iterator<Item = GlSubTexture> + 'a {
        text.chars().scan(self.glyph_key(' '), |glyph_key, c| {
            let mut glyph = self.rasterize_char(c).ok()?;

            // Add kerning to glyph advance.
            let last_key = mem::replace(glyph_key, self.glyph_key(c));
            let kerning = self.rasterizer.kerning(last_key, *glyph_key);
            glyph.advance.0 += kerning.0 as i32;
            glyph.advance.1 += kerning.1 as i32;

            Some(glyph)
        })
    }

    /// Get rasterized OpenGL glyph.
    pub fn rasterize_char(&mut self, character: char) -> Result<GlSubTexture> {
        let glyph_key = self.glyph_key(character);

        // Try to load glyph from cache.
        let entry = match self.cache.entry(character.into()) {
            Entry::Occupied(entry) => return Ok(*entry.get()),
            Entry::Vacant(entry) => entry,
        };

        // Rasterize the glyph if it's missing.
        let rasterized_glyph = self.rasterizer.get_glyph(glyph_key)?;
        let glyph = self.atlas.insert(&rasterized_glyph)?;

        Ok(*entry.insert(glyph))
    }

    /// Rasterize an SVG from its text.
    pub fn rasterize_svg(
        &mut self,
        svg: Svg,
        target_width: impl Into<Option<u32>>,
        target_height: impl Into<Option<u32>>,
    ) -> Result<GlSubTexture> {
        // Calculate SVG X/Y scale factor.
        let (mut width, mut height) = svg.size();
        let x_scale = target_width.into().map(|tw| tw as f64 / width as f64);
        let y_scale = target_height.into().map(|th| th as f64 / height as f64);
        let (x_scale, y_scale) = match (x_scale, y_scale) {
            (Some(x_scale), Some(y_scale)) => (x_scale, y_scale),
            (Some(scale), None) | (None, Some(scale)) => (scale, scale),
            (None, None) => (1., 1.),
        };

        // Calculate target dimensions.
        width = (width as f64 * self.scale_factor * x_scale) as u32;
        height = (height as f64 * self.scale_factor * y_scale) as u32;

        // Try to load svg from cache.
        let entry = match self.cache.entry(CacheKey::Svg((svg, width, height))) {
            Entry::Occupied(entry) => return Ok(*entry.get()),
            Entry::Vacant(entry) => entry,
        };

        // Setup target buffer.
        let mut pixmap = Pixmap::new(width, height)
            .ok_or_else(|| format!("Invalid SVG buffer size: {width}x{height}"))?;

        // Compute transform for height.
        let tree = Tree::from_str(svg.content(), &Options::default())?;
        let tree_scale = width as f32 / tree.size().width();
        let transform = Transform::from_scale(tree_scale, (y_scale / x_scale) as f32 * tree_scale);

        // Render SVG into buffer.
        resvg::render(&tree, transform, &mut pixmap.as_mut());

        // Load SVG into atlas.
        let atlas_entry = AtlasEntry::new_svg(pixmap.take(), width, height);
        let svg = self.atlas.insert(atlas_entry)?;

        Ok(*entry.insert(svg))
    }

    /// Get font metrics.
    pub fn metrics(&mut self) -> Result<Metrics> {
        match &mut self.metrics {
            Some(metrics) => Ok(*metrics),
            None => {
                let _ = self.rasterize_char(' ');
                let new_metrics = self.rasterizer.metrics(self.font, self.font_size())?;
                Ok(*self.metrics.insert(new_metrics))
            },
        }
    }

    /// Get glyph key for a character.
    fn glyph_key(&self, character: char) -> GlyphKey {
        GlyphKey { font_key: self.font, size: self.font_size(), character }
    }

    /// Load a new font.
    fn load_font(
        rasterizer: &mut Rasterizer,
        font_name: &str,
        size: FontSize,
        scale_factor: f64,
    ) -> Result<FontKey> {
        let font_style = Style::Description { slant: Slant::Normal, weight: Weight::Normal };
        let font_desc = FontDesc::new(font_name, font_style);
        Ok(rasterizer.load_font(&font_desc, size.scale(scale_factor as f32))?)
    }

    /// Scaled font size.
    fn font_size(&self) -> FontSize {
        self.size.scale(self.scale_factor as f32)
    }
}

/// Atlas for combining multiple textures in OpenGL.
///
/// The strategy for filling an atlas looks roughly like this:
///
/// ```text
///                           (width, height)
///   ┌─────┬─────┬─────┬─────┬─────┐
///   │ 10  │     │     │     │     │ <- Atlas is full when next glyph's height doesn't fit.
///   │     │     │     │     │     │ <- Empty spaces for new elements.
///   ├─────┼─────┼─────┼─────┼─────┤
///   │ 5   │ 6   │ 7   │ 8   │ 9   │
///   │     │     │     │     │     │
///   ├─────┼─────┼─────┼─────┴─────┤ <- Row height is tallest subtexture in the row.
///   │ 1   │ 2   │ 3   │ 4         │    This is the baseline for the next row.
///   │     │     │     │           │ <- Row is full when next glyph's width doesn't fit.
///   └─────┴─────┴─────┴───────────┘
/// (0, 0)
/// ```
pub struct Atlas {
    /// OpenGL texture ID.
    textures: Vec<Texture>,
    /// Largest glyph's height in this row.
    row_height: i32,
    /// X position for writing new glyphs.
    cursor_x: i32,
    /// Y position for writing new glyphs.
    cursor_y: i32,
}

impl Default for Atlas {
    fn default() -> Self {
        Self {
            textures: vec![Texture::new(ATLAS_SIZE, ATLAS_SIZE)],
            row_height: Default::default(),
            cursor_x: Default::default(),
            cursor_y: Default::default(),
        }
    }
}

impl Atlas {
    /// Insert an entry into the atlas.
    fn insert<'a, E: Into<AtlasEntry<'a>>>(&mut self, entry: E) -> Result<GlSubTexture> {
        let entry = entry.into();

        // Error if entry cannot fit at all.
        if entry.width > ATLAS_SIZE || entry.height > ATLAS_SIZE {
            return Err("glyph too big for atlas".into());
        }

        // Create new row if entry doesn't fit into current one.
        if self.cursor_x + entry.width > ATLAS_SIZE {
            self.cursor_y += mem::take(&mut self.row_height);
            self.cursor_x = 0;
        }

        // Create a new texture if the row's available height is too little.
        if self.cursor_y + entry.height > ATLAS_SIZE {
            self.textures.push(Texture::new(ATLAS_SIZE, ATLAS_SIZE));
            self.row_height = 0;
            self.cursor_x = 0;
            self.cursor_y = 0;
        }

        // Upload entry's buffer to OpenGL.
        let active_texture = &self.textures[self.textures.len() - 1];
        active_texture.upload_buffer(
            self.cursor_x,
            self.cursor_y,
            entry.width,
            entry.height,
            &entry.buffer,
        );

        // Generate UV coordinates.
        let uv_bot = self.cursor_y as f32 / ATLAS_SIZE as f32;
        let uv_left = self.cursor_x as f32 / ATLAS_SIZE as f32;
        let uv_height = entry.height as f32 / ATLAS_SIZE as f32;
        let uv_width = entry.width as f32 / ATLAS_SIZE as f32;

        // Update atlas write position.
        self.row_height = cmp::max(self.row_height, entry.height);
        self.cursor_x += entry.width;

        Ok(GlSubTexture {
            uv_height,
            uv_width,
            uv_left,
            uv_bot,
            multicolor: entry.multicolor,
            texture_id: active_texture.id,
            advance: entry.advance,
            height: entry.height as i16,
            width: entry.width as i16,
            left: entry.left as i16,
            top: entry.top as i16,
        })
    }
}

/// Subtexture cached inside an [`Atlas`].
#[derive(Copy, Clone, Debug)]
pub struct GlSubTexture {
    pub texture_id: GLuint,
    pub multicolor: bool,
    pub top: i16,
    pub left: i16,
    pub width: i16,
    pub height: i16,
    pub uv_bot: f32,
    pub uv_left: f32,
    pub uv_width: f32,
    pub uv_height: f32,
    pub advance: (i32, i32),
}

fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let rgb_len = rgb.len();
    debug_assert_eq!(rgb_len % 3, 0);

    let pixel_count = rgb_len / 3;
    let mut rgba = vec![255; pixel_count * 4];

    for (rgb, rgba) in rgb.chunks_exact(3).zip(rgba.chunks_exact_mut(4)) {
        rgba[..3].copy_from_slice(rgb);
    }

    rgba
}

/// Element stored in the texture atlas.
struct AtlasEntry<'a> {
    buffer: Cow<'a, [u8]>,
    width: i32,
    height: i32,
    top: i32,
    left: i32,
    advance: (i32, i32),
    multicolor: bool,
}

impl AtlasEntry<'static> {
    /// Create a new SVG atlas entry.
    fn new_svg(buffer: Vec<u8>, width: u32, height: u32) -> Self {
        Self {
            buffer: Cow::Owned(buffer),
            width: width as i32,
            height: height as i32,
            top: 0,
            left: 0,
            advance: (width as i32, 0),
            multicolor: true,
        }
    }
}

impl<'a> From<&'a RasterizedGlyph> for AtlasEntry<'a> {
    fn from(glyph: &'a RasterizedGlyph) -> Self {
        let (buffer, multicolor) = match &glyph.buffer {
            BitmapBuffer::Rgb(buffer) => (Cow::Owned(rgb_to_rgba(buffer)), false),
            BitmapBuffer::Rgba(buffer) => (Cow::Borrowed(buffer.as_slice()), true),
        };

        Self {
            multicolor,
            buffer,
            width: glyph.width,
            height: glyph.height,
            top: glyph.top,
            left: glyph.left,
            advance: glyph.advance,
        }
    }
}

/// Key for caching atlas entries.
#[derive(Copy, Clone, Hash, PartialEq, Eq)]
enum CacheKey {
    Character(char),
    Svg((Svg, u32, u32)),
}

impl From<char> for CacheKey {
    fn from(c: char) -> Self {
        Self::Character(c)
    }
}

/// Built-in SVGs.
#[derive(Copy, Clone, Hash, PartialEq, Eq, Debug)]
pub enum Svg {
    BatteryCharging100,
    BatteryCharging80,
    BatteryCharging60,
    BatteryCharging40,
    BatteryCharging20,
    Battery100,
    Battery80,
    Battery60,
    Battery40,
    Battery20,
    WifiConnected100,
    WifiConnected75,
    WifiConnected50,
    WifiConnected25,
    WifiConnected0,
    WifiDisconnected100,
    WifiDisconnected75,
    WifiDisconnected50,
    WifiDisconnected25,
    WifiDisconnected0,
    WifiDisabled,
    Cellular100,
    Cellular80,
    Cellular60,
    Cellular40,
    Cellular20,
    Cellular0,
    CellularDisabled,
    Brightness,
    FlashlightOn,
    FlashlightOff,
    OrientationLocked,
    OrientationUnlocked,
    Scale,
    ArrowUp,
    ArrowDown,
}

impl Svg {
    /// Get SVG's dimensions.
    pub const fn size(&self) -> (u32, u32) {
        match self {
            Self::BatteryCharging100 => (20, 13),
            Self::BatteryCharging80 => (20, 13),
            Self::BatteryCharging60 => (20, 13),
            Self::BatteryCharging40 => (20, 13),
            Self::BatteryCharging20 => (20, 13),
            Self::Battery100 => (20, 7),
            Self::Battery80 => (20, 7),
            Self::Battery60 => (20, 7),
            Self::Battery40 => (20, 7),
            Self::Battery20 => (20, 7),
            Self::WifiConnected100 => (20, 14),
            Self::WifiConnected75 => (20, 14),
            Self::WifiConnected50 => (20, 14),
            Self::WifiConnected25 => (20, 14),
            Self::WifiConnected0 => (20, 14),
            Self::WifiDisconnected100 => (20, 14),
            Self::WifiDisconnected75 => (20, 14),
            Self::WifiDisconnected50 => (20, 14),
            Self::WifiDisconnected25 => (20, 14),
            Self::WifiDisconnected0 => (20, 14),
            Self::WifiDisabled => (20, 16),
            Self::Cellular100 => (20, 15),
            Self::Cellular80 => (20, 15),
            Self::Cellular60 => (20, 15),
            Self::Cellular40 => (20, 15),
            Self::Cellular20 => (20, 15),
            Self::Cellular0 => (20, 15),
            Self::CellularDisabled => (20, 18),
            Self::Brightness => (1, 1),
            Self::FlashlightOn => (45, 75),
            Self::FlashlightOff => (45, 75),
            Self::OrientationLocked => (73, 65),
            Self::OrientationUnlocked => (73, 65),
            Self::Scale => (11, 7),
            Self::ArrowUp => (64, 64),
            Self::ArrowDown => (64, 64),
        }
    }

    /// Get SVG's text content.
    const fn content(&self) -> &'static str {
        match self {
            Self::BatteryCharging100 => include_str!("../svgs/battery/battery_charging_100.svg"),
            Self::BatteryCharging80 => include_str!("../svgs/battery/battery_charging_80.svg"),
            Self::BatteryCharging60 => include_str!("../svgs/battery/battery_charging_60.svg"),
            Self::BatteryCharging40 => include_str!("../svgs/battery/battery_charging_40.svg"),
            Self::BatteryCharging20 => include_str!("../svgs/battery/battery_charging_20.svg"),
            Self::Battery100 => include_str!("../svgs/battery/battery_100.svg"),
            Self::Battery80 => include_str!("../svgs/battery/battery_80.svg"),
            Self::Battery60 => include_str!("../svgs/battery/battery_60.svg"),
            Self::Battery40 => include_str!("../svgs/battery/battery_40.svg"),
            Self::Battery20 => include_str!("../svgs/battery/battery_20.svg"),
            Self::WifiConnected100 => include_str!("../svgs/wifi/wifi_connected_100.svg"),
            Self::WifiConnected75 => include_str!("../svgs/wifi/wifi_connected_75.svg"),
            Self::WifiConnected50 => include_str!("../svgs/wifi/wifi_connected_50.svg"),
            Self::WifiConnected25 => include_str!("../svgs/wifi/wifi_connected_25.svg"),
            Self::WifiConnected0 => include_str!("../svgs/wifi/wifi_connected_0.svg"),
            Self::WifiDisconnected100 => include_str!("../svgs/wifi/wifi_disconnected_100.svg"),
            Self::WifiDisconnected75 => include_str!("../svgs/wifi/wifi_disconnected_75.svg"),
            Self::WifiDisconnected50 => include_str!("../svgs/wifi/wifi_disconnected_50.svg"),
            Self::WifiDisconnected25 => include_str!("../svgs/wifi/wifi_disconnected_25.svg"),
            Self::WifiDisconnected0 => include_str!("../svgs/wifi/wifi_disconnected_0.svg"),
            Self::WifiDisabled => include_str!("../svgs/wifi/wifi_disabled.svg"),
            Self::Cellular100 => include_str!("../svgs/cellular/cellular_100.svg"),
            Self::Cellular80 => include_str!("../svgs/cellular/cellular_80.svg"),
            Self::Cellular60 => include_str!("../svgs/cellular/cellular_60.svg"),
            Self::Cellular40 => include_str!("../svgs/cellular/cellular_40.svg"),
            Self::Cellular20 => include_str!("../svgs/cellular/cellular_20.svg"),
            Self::Cellular0 => include_str!("../svgs/cellular/cellular_0.svg"),
            Self::CellularDisabled => include_str!("../svgs/cellular/cellular_disabled.svg"),
            Self::Brightness => include_str!("../svgs/brightness/brightness.svg"),
            Self::FlashlightOn => include_str!("../svgs/flashlight/flashlight_on.svg"),
            Self::FlashlightOff => include_str!("../svgs/flashlight/flashlight_off.svg"),
            Self::OrientationLocked => include_str!("../svgs/orientation/orientation_locked.svg"),
            Self::OrientationUnlocked => {
                include_str!("../svgs/orientation/orientation_unlocked.svg")
            },
            Self::Scale => include_str!("../svgs/scale/scale.svg"),
            Self::ArrowUp => include_str!("../svgs/arrow_up.svg"),
            Self::ArrowDown => include_str!("../svgs/arrow_down.svg"),
        }
    }
}
