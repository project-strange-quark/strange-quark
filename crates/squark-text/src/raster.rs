// SPDX-License-Identifier: MIT

use squark_types::render::{Bitmap, BitmapFormat, GlyphKey, ResourceUpdate};
use swash::{
    FontRef,
    scale::{Render, ScaleContext, Source, StrikeWith, image::Content},
    zeno::{Format, Transform, Vector},
};

use crate::{FontInstance, Synthesis, outline::GrayOutlines};

#[derive(Default)]
pub struct GlyphRasterizer {
    context: ScaleContext,
    outlines: GrayOutlines,
}

impl std::fmt::Debug for GlyphRasterizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlyphRasterizer").finish_non_exhaustive()
    }
}

impl GlyphRasterizer {
    pub fn rasterize(&mut self, key: GlyphKey, instance: &FontInstance) -> ResourceUpdate {
        let image = u16::try_from(key.glyph_id)
            .ok()
            .and_then(|glyph| {
                let font =
                    FontRef::from_index(instance.font.data.as_ref(), instance.font.index as usize)?;
                let size = instance.size * key.raster_scale();
                let mut scaler = self
                    .context
                    .builder(font)
                    .size(size)
                    .hint(true)
                    .normalized_coords(instance.coords.iter().copied())
                    .build();
                // Swash's outline y axis points up. The device y axis points down.
                let offset = Vector::new(
                    f32::from(key.phase[0]) * 0.25,
                    f32::from(key.phase[1]) * -0.25,
                );
                Render::new(&[
                    Source::ColorOutline(0),
                    Source::ColorBitmap(StrikeWith::BestFit),
                ])
                .offset(offset)
                .render(&mut scaler, glyph)
                .or_else(|| self.outlines.render(key, instance, offset))
                .or_else(|| {
                    // Swash emboldens as FreeType does, growing the outline
                    // up and to the right by twice the strength; shift it
                    // back to grow it evenly, as Skia's fake bold does. It
                    // slants after emboldening, a little unlike Chromium.
                    let synthesis = instance.synthesis;
                    let half = if synthesis.bold {
                        crate::outline::fake_bold_width(instance.size) * key.raster_scale() / 2.0
                    } else {
                        0.0
                    };
                    let skew = if synthesis.italic {
                        crate::outline::FAKE_ITALIC_SKEW
                    } else {
                        0.0
                    };
                    Render::new(&[Source::Outline, Source::Bitmap(StrikeWith::BestFit)])
                        .format(Format::Alpha)
                        .embolden(half)
                        .transform((synthesis != Synthesis::default()).then(|| {
                            Transform::new(1.0, 0.0, skew, 1.0, -half - skew * half, -half)
                        }))
                        .offset(offset)
                        .render(&mut scaler, glyph)
                })
            })
            .unwrap_or_default();
        let placement = image.placement;
        let (format, channels, mut data) = match image.content {
            Content::Color => (BitmapFormat::Rgba8SrgbPremultiplied, 4, image.data),
            Content::Mask => (BitmapFormat::R8Coverage, 1, image.data),
            Content::SubpixelMask => (
                BitmapFormat::R8Coverage,
                1,
                image
                    .data
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect(),
            ),
        };
        // Color bitmaps can have RGB in transparent pixels. Normalize their RGBA
        // to the premultiplied representation expected by the resource stream.
        if matches!(image.source, Source::ColorBitmap(_)) {
            squark_assets::premultiply_rgba(&mut data);
        }
        ResourceUpdate::PutGlyph {
            key,
            bitmap: Bitmap {
                size: [placement.width, placement.height],
                stride: placement.width * channels,
                format,
                pixels: data.into(),
            },
            bearing: [placement.left, placement.top],
        }
    }
}
