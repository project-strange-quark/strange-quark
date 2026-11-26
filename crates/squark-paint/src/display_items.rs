// SPDX-License-Identifier: MIT
//! A minimal CPU-side display-item frame, drawn by squark-hello and by squark-gpu's
//! primitive renderer.

pub use squark_types::Rect;

/// Straight alpha and sRGB-encoded channels.
pub type Color = [u8; 4];

#[derive(Debug)]
pub struct Frame {
    pub clear: Color,
    /// Back-to-front painting order.
    pub display_list: Vec<DisplayItem>,
    pub glyph_atlas: GlyphAtlas,
}

#[derive(Debug, Clone, Copy)]
pub struct RoundedRect {
    pub rect: Rect,
    pub radius: f32,
}

#[derive(Debug)]
pub enum DisplayItem {
    Quad {
        shape: RoundedRect,
        color: Color,
    },
    Border {
        shape: RoundedRect,
        width: f32,
        color: Color,
    },
    Shadow {
        shape: RoundedRect,
        sigma: f32,
        color: Color,
    },
    Gradient {
        shape: RoundedRect,
        left: Color,
        right: Color,
    },
    GlyphRun {
        color: Color,
        glyphs: Vec<Glyph>,
    },
}

#[derive(Debug)]
pub struct GlyphAtlas {
    pub size: [u32; 2],
    pub coverage: Vec<u8>,
}

#[derive(Debug)]
pub struct Glyph {
    pub dest: Rect,
    /// Integer texel coordinates in the coverage atlas.
    pub source: [u32; 4],
}
