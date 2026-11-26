// SPDX-License-Identifier: MIT

use squark_paint::display_items::{
    Color, DisplayItem, Frame, Glyph, GlyphAtlas, Rect, RoundedRect,
};

pub(crate) const SIZE: [u32; 2] = [401, 240];

pub(crate) fn frame() -> Frame {
    let card = shape(24.0, 22.0, 353.0, 196.0, 22.0);
    let banner = shape(48.0, 47.0, 305.0, 66.0, 14.0);
    let (glyph_run, glyph_atlas) = make_glyph_run();
    let display_list = vec![
        DisplayItem::Shadow {
            shape: shape(24.0, 28.0, 353.0, 196.0, 22.0),
            sigma: 6.0,
            color: [36, 60, 103, 77],
        },
        quad(card, [251, 253, 255, 255]),
        border(card, [189, 203, 227, 255]),
        DisplayItem::Gradient {
            shape: shape(50.0, 49.0, 301.0, 62.0, 12.0),
            left: [87, 181, 246, 255],
            right: [94, 87, 226, 255],
        },
        border(banner, [40, 92, 149, 255]),
        glyph_run,
        quad(shape(48.0, 169.0, 144.0, 27.0, 12.0), [224, 242, 233, 255]),
        quad(shape(203.0, 169.0, 150.0, 27.0, 12.0), [252, 235, 220, 255]),
        border(shape(48.0, 169.0, 144.0, 27.0, 12.0), [156, 204, 185, 255]),
        border(shape(203.0, 169.0, 150.0, 27.0, 12.0), [237, 181, 138, 255]),
    ];
    Frame {
        clear: [237, 242, 250, 255],
        display_list,
        glyph_atlas,
    }
}

fn shape(x: f32, y: f32, width: f32, height: f32, radius: f32) -> RoundedRect {
    RoundedRect {
        rect: Rect::new(x, y, width, height),
        radius,
    }
}

fn quad(shape: RoundedRect, color: Color) -> DisplayItem {
    DisplayItem::Quad { shape, color }
}

fn border(shape: RoundedRect, color: Color) -> DisplayItem {
    DisplayItem::Border {
        shape,
        width: 2.0,
        color,
    }
}

// Original 5×7 glyphs for “SQK GPU”. The gap takes one character advance.
const GLYPHS: [[u8; 7]; 6] = [
    [
        0b01110, 0b10001, 0b10000, 0b01110, 0b00001, 0b10001, 0b01110,
    ], // S
    [
        0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
    ], // Q
    [
        0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
    ], // K
    [
        0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110,
    ], // G
    [
        0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
    ], // P
    [
        0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
    ], // U
];

fn make_glyph_run() -> (DisplayItem, GlyphAtlas) {
    const WIDTH: u32 = 5;
    const HEIGHT: u32 = 7;
    let atlas_width = WIDTH * GLYPHS.len() as u32;
    let mut coverage = Vec::with_capacity((atlas_width * HEIGHT) as usize);
    for row in 0..HEIGHT as usize {
        for mask in GLYPHS {
            for column in (0..WIDTH).rev() {
                coverage.push(if mask[row] & (1 << column) != 0 {
                    255
                } else {
                    0
                });
            }
        }
    }

    let glyphs = [0, 1, 2, 4, 5, 6]
        .into_iter()
        .enumerate()
        .map(|(index, column)| Glyph {
            dest: Rect::new(49.0 + column as f32 * 18.0, 130.0, 15.0, 21.0),
            source: [index as u32 * WIDTH, 0, WIDTH, HEIGHT],
        })
        .collect();
    (
        DisplayItem::GlyphRun {
            color: [34, 49, 76, 255],
            glyphs,
        },
        GlyphAtlas {
            size: [atlas_width, HEIGHT],
            coverage,
        },
    )
}
