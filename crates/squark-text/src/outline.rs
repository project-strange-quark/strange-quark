// SPDX-License-Identifier: MIT

use std::collections::HashMap;

use skrifa::{
    FontRef, GlyphId, MetadataProvider,
    instance::{NormalizedCoord, Size},
    metrics::BoundingBox,
    outline::{
        DrawSettings, Engine, HintingInstance, HintingOptions, OutlineGlyphCollection,
        OutlineGlyphFormat, OutlinePen, SmoothMode, Target,
    },
};
use squark_types::render::{FontInstanceId, GlyphKey};
use swash::{
    scale::{
        Source,
        image::{Content, Image},
    },
    zeno::{Command, Mask, Origin, Scratch, Vector},
};

use crate::FontInstance;

fn light_hint(
    outlines: &OutlineGlyphCollection<'_>,
    size: Size,
    coords: &[NormalizedCoord],
) -> Option<HintingInstance> {
    // Light automatic hinting grid-fits TrueType outlines for grayscale
    // masks. Keep the native interpreter available for PostScript outlines.
    let engine = if outlines.format() == Some(OutlineGlyphFormat::Glyf) {
        Engine::Auto(None)
    } else {
        Engine::AutoFallback
    };
    HintingInstance::new(
        outlines,
        size,
        coords,
        HintingOptions {
            engine,
            target: Target::from(SmoothMode::Light),
        },
    )
    .ok()
}

#[derive(Default)]
pub(crate) struct GrayOutlines {
    hint: Option<((FontInstanceId, u32), HintingInstance)>,
    path: Path,
    scratch: Scratch,
}

impl GrayOutlines {
    pub(crate) fn render(
        &mut self,
        key: GlyphKey,
        instance: &FontInstance,
        offset: Vector,
    ) -> Option<Image> {
        let font = FontRef::from_index(instance.font.data.as_ref(), instance.font.index).ok()?;
        let outlines = font.outline_glyphs();
        let glyph = outlines.get(GlyphId::new(key.glyph_id))?;
        let raster = (key.font, key.scale);
        if !self
            .hint
            .as_ref()
            .is_some_and(|(prior, _)| *prior == raster)
        {
            let coords: Vec<_> = instance
                .coords
                .iter()
                .copied()
                .map(NormalizedCoord::from_bits)
                .collect();
            let hint = light_hint(
                &outlines,
                Size::new(instance.size * key.raster_scale()),
                &coords,
            )?;
            self.hint = Some((raster, hint));
        }
        let (_, hint) = self.hint.as_ref()?;
        self.path.commands.clear();
        self.path.snap_coverage = outlines.format() == Some(OutlineGlyphFormat::Glyf);
        glyph
            .draw(DrawSettings::hinted(hint, false), &mut self.path)
            .ok()?;

        let mut data = Vec::new();
        let placement = Mask::with_scratch(self.path.commands.as_slice(), &mut self.scratch)
            .origin(Origin::BottomLeft)
            .offset(offset)
            .render_offset(offset)
            .inspect(|format, width, height| data.resize(format.buffer_size(width, height), 0))
            .render_into(&mut data, None);
        Some(Image {
            source: Source::Outline,
            content: Content::Mask,
            placement,
            data,
        })
    }
}

#[derive(Default)]
pub(crate) struct CanvasInkBounds {
    font: Option<FontInstanceId>,
    hint: Option<HintingInstance>,
    bounds: HashMap<GlyphId, Option<BoundingBox>>,
    path: Path,
}

impl CanvasInkBounds {
    pub(crate) fn glyph(
        &mut self,
        id: FontInstanceId,
        glyph: GlyphId,
        font: &FontRef<'_>,
        size: Size,
        coords: &[NormalizedCoord],
    ) -> Option<BoundingBox> {
        if self.font != Some(id) {
            self.font = Some(id);
            self.bounds.clear();
            self.hint = light_hint(&font.outline_glyphs(), size, coords);
        }
        *self.bounds.entry(glyph).or_insert_with(|| {
            let hinted = self.hint.as_ref().and_then(|hint| {
                self.path.commands.clear();
                font.outline_glyphs()
                    .get(glyph)?
                    .draw(DrawSettings::hinted(hint, false), &mut self.path)
                    .ok()?;
                let ink = swash::zeno::bounds(
                    self.path.commands.as_slice(),
                    swash::zeno::Fill::NonZero,
                    None,
                );
                Some(BoundingBox {
                    x_min: ink.min.x,
                    y_min: ink.min.y,
                    x_max: ink.max.x,
                    y_max: ink.max.y,
                })
            });
            let ink = hinted.or_else(|| font.glyph_metrics(size, coords).bounds(glyph))?;
            // Round the hinted bounds at the glyph origin before adding the
            // shaped position. The latter retains its fractional advance.
            Some(BoundingBox {
                x_min: ink.x_min.floor(),
                y_min: ink.y_min.floor(),
                x_max: ink.x_max.ceil(),
                y_max: ink.y_max.ceil(),
            })
        })
    }
}

#[derive(Default)]
struct Path {
    commands: Vec<Command>,
    position: Vector,
    snap_coverage: bool,
}

impl Path {
    fn endpoint(&self, mut point: Vector) -> Vector {
        if self.snap_coverage {
            // Match the quarter-pixel vertical grid used by Skia's analytic
            // rasterizer. Glyph outline coordinates increase upwards.
            point.y = (point.y * 4.0 - 0.5).ceil() * 0.25;
        }
        point
    }

    fn quadratic(&mut self, start: Vector, control: Vector, end: Vector) {
        // The center's deviation from the chord decreases by a factor of four
        // per subdivision. Use the scan converter's one-eighth-pixel threshold
        // and keep at least two segments before snapping their endpoints.
        let delta = (control * 2.0 - start - end) * 0.25;
        let (x, y) = (delta.x.abs(), delta.y.abs());
        let deviation = x.max(y) + x.min(y) * 0.5;
        let eighths = (deviation * 8.0 + 0.5).floor() as u32;
        let subdivisions = ((u32::BITS - eighths.leading_zeros()) / 2).clamp(1, 6);
        let segments = 1 << subdivisions;
        for segment in 1..=segments {
            let t = segment as f32 / segments as f32;
            let u = 1.0 - t;
            let point = start * (u * u) + control * (2.0 * u * t) + end * (t * t);
            self.commands.push(Command::LineTo(self.endpoint(point)));
        }
    }
}

impl OutlinePen for Path {
    fn move_to(&mut self, x: f32, y: f32) {
        self.position = Vector::new(x, y);
        self.commands
            .push(Command::MoveTo(self.endpoint(self.position)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.position = Vector::new(x, y);
        self.commands
            .push(Command::LineTo(self.endpoint(self.position)));
    }

    fn quad_to(&mut self, x: f32, y: f32, to_x: f32, to_y: f32) {
        let control = Vector::new(x, y);
        let end = Vector::new(to_x, to_y);
        if self.snap_coverage {
            self.quadratic(self.position, control, end);
        } else {
            self.commands.push(Command::QuadTo(control, end));
        }
        self.position = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, to_x: f32, to_y: f32) {
        self.position = Vector::new(to_x, to_y);
        self.commands.push(Command::CurveTo(
            [x1, y1].into(),
            [x2, y2].into(),
            self.endpoint(self.position),
        ));
    }

    fn close(&mut self) {
        self.commands.push(Command::Close);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parley::{FontData, fontique::Blob};

    use super::*;

    #[test]
    fn snaps_coverage_edges_in_device_y_without_moving_subpixel_x() {
        let mut path = Path {
            snap_coverage: true,
            ..Path::default()
        };
        path.move_to(0.25, 0.125);
        path.line_to(1.25, 0.11);
        path.quad_to(2.25, 2.3, 3.25, 1.37);
        assert_eq!(
            path.commands,
            [
                Command::MoveTo(Vector::new(0.25, 0.0)),
                Command::LineTo(Vector::new(1.25, 0.0)),
                Command::LineTo(Vector::new(2.25, 1.5)),
                Command::LineTo(Vector::new(3.25, 1.25)),
            ]
        );
    }

    #[test]
    fn lightly_hinted_outline_retains_subpixel_coverage_and_scales() {
        let bytes = wuff::decompress_woff2(include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
        ))
        .unwrap();
        let font = FontRef::new(&bytes).unwrap();
        let glyph_id = font.charmap().map('A').unwrap().to_u32();
        let instance = FontInstance {
            font: FontData {
                data: Blob::new(Arc::new(bytes)),
                index: 0,
            },
            size: 16.0,
            coords: Vec::new(),
        };
        let mut outlines = GrayOutlines::default();
        let base = GlyphKey {
            font: FontInstanceId(1),
            glyph_id,
            phase: [0, 0],
            scale: 1024,
        };
        let mask = outlines
            .render(base, &instance, Vector::new(0.0, 0.0))
            .unwrap();
        let moved = outlines
            .render(
                GlyphKey {
                    phase: [1, 0],
                    ..base
                },
                &instance,
                Vector::new(0.25, 0.0),
            )
            .unwrap();
        assert_eq!(mask.content, Content::Mask);
        assert_eq!(mask.placement.top, moved.placement.top);
        assert!(mask.placement.top > 0);
        assert!(mask.data.iter().any(|&coverage| coverage > 0));
        assert_ne!(mask.data, moved.data);
        let doubled = outlines
            .render(
                GlyphKey {
                    scale: 2048,
                    ..base
                },
                &instance,
                Vector::new(0.0, 0.0),
            )
            .unwrap();
        assert!(doubled.placement.height > mask.placement.height);
        assert!(doubled.placement.width > mask.placement.width);
    }
}
