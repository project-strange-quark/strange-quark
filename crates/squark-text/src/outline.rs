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

use crate::{FontInstance, Synthesis};

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
    /// The light hinting instance for the font instance at the key's
    /// raster scale, kept while consecutive glyphs share them.
    fn hint<'a>(
        slot: &'a mut Option<((FontInstanceId, u32), HintingInstance)>,
        key: GlyphKey,
        instance: &FontInstance,
        outlines: &OutlineGlyphCollection<'_>,
    ) -> Option<&'a HintingInstance> {
        let raster = (key.font, key.scale);
        if !slot.as_ref().is_some_and(|(prior, _)| *prior == raster) {
            let coords: Vec<_> = instance
                .coords
                .iter()
                .copied()
                .map(NormalizedCoord::from_bits)
                .collect();
            let hint = light_hint(
                outlines,
                Size::new(instance.size * key.raster_scale()),
                &coords,
            )?;
            *slot = Some((raster, hint));
        }
        slot.as_ref().map(|(_, hint)| hint)
    }

    pub(crate) fn render(
        &mut self,
        key: GlyphKey,
        instance: &FontInstance,
        offset: Vector,
    ) -> Option<Image> {
        let font = FontRef::from_index(instance.font.data.as_ref(), instance.font.index).ok()?;
        let outlines = font.outline_glyphs();
        let glyph = outlines.get(GlyphId::new(key.glyph_id))?;
        let glyf = outlines.format() == Some(OutlineGlyphFormat::Glyf);
        let synthesis = instance.synthesis;
        let synthesized = synthesis != Synthesis::default();
        self.path.commands.clear();
        // Emboldening offsets a polygon and moves points off the coverage
        // grid, so a synthesized outline snaps to it afterwards.
        self.path.flatten = glyf || synthesis.bold;
        self.path.snap_coverage = glyf && !synthesized;
        if synthesis.italic {
            // Chromium draws slanted glyphs unhinted.
            let coords: Vec<_> = instance
                .coords
                .iter()
                .copied()
                .map(NormalizedCoord::from_bits)
                .collect();
            let size = Size::new(instance.size * key.raster_scale());
            glyph
                .draw(
                    DrawSettings::unhinted(size, coords.as_slice()),
                    &mut self.path,
                )
                .ok()?;
            skew(&mut self.path.commands);
        } else {
            let hint = Self::hint(&mut self.hint, key, instance, &outlines)?;
            glyph
                .draw(DrawSettings::hinted(hint, false), &mut self.path)
                .ok()?;
        }
        // Chromium strokes the slanted outline: a bold italic stem widens by
        // the same amount horizontally as an upright one.
        if synthesis.bold {
            embolden(
                &mut self.path.commands,
                fake_bold_width(instance.size) * key.raster_scale(),
            );
        }
        if glyf && synthesized {
            for command in &mut self.path.commands {
                if let Command::MoveTo(point) | Command::LineTo(point) = command {
                    *point = snap(*point);
                }
            }
        }

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

/// The width of the stroke Skia's fake bold outlines glyphs with, in CSS
/// pixels: 1/24 of the font size at 9px and below, 1/32 at 36px and above,
/// interpolated between (`kStdFakeBoldInterpKeys`). Half of it widens each
/// side of a stem; the advances stay as they are.
pub(crate) fn fake_bold_width(size: f32) -> f32 {
    const KEYS: [f32; 2] = [9.0, 36.0];
    const SCALES: [f32; 2] = [1.0 / 24.0, 1.0 / 32.0];
    let t = ((size - KEYS[0]) / (KEYS[1] - KEYS[0])).clamp(0.0, 1.0);
    size * (SCALES[0] + (SCALES[1] - SCALES[0]) * t)
}

/// Skia's fake italic: a horizontal skew of a quarter of the height.
pub(crate) const FAKE_ITALIC_SKEW: f32 = 0.25;

/// Match the quarter-pixel vertical grid used by Skia's analytic
/// rasterizer. Glyph outline coordinates increase upwards.
fn snap(mut point: Vector) -> Vector {
    point.y = (point.y * 4.0 - 0.5).ceil() * 0.25;
    point
}

/// Slants the outline about the baseline, its top to the right.
fn skew(commands: &mut [Command]) {
    let slant = |point: &mut Vector| point.x += FAKE_ITALIC_SKEW * point.y;
    for command in commands {
        match command {
            Command::MoveTo(point) | Command::LineTo(point) => slant(point),
            Command::QuadTo(control, point) => {
                slant(control);
                slant(point);
            }
            Command::CurveTo(first, second, point) => {
                slant(first);
                slant(second);
                slant(point);
            }
            Command::Close => {}
        }
    }
}

/// Outsets a flattened outline by half of `width` on every side, as Skia's
/// stroke-and-fill of the glyph path does: each vertex moves to where the
/// offsets of its edges meet, beveled past a miter limit of 4. Holes, wound
/// the other way, shrink.
fn embolden(commands: &mut Vec<Command>, width: f32) {
    const EPSILON: f32 = 1.0 / 4096.0;
    let mut contours: Vec<Vec<Vector>> = Vec::new();
    for command in commands.iter() {
        match *command {
            Command::MoveTo(point) => contours.push(vec![point]),
            Command::LineTo(point) => match contours.last_mut() {
                Some(contour) => contour.push(point),
                None => contours.push(vec![point]),
            },
            Command::QuadTo(..) | Command::CurveTo(..) => return,
            Command::Close => {}
        }
    }
    for contour in &mut contours {
        contour.dedup_by(|point, previous| (*point - *previous).length() < EPSILON);
        while contour.len() > 1 && (contour[contour.len() - 1] - contour[0]).length() < EPSILON {
            contour.pop();
        }
    }
    contours.retain(|contour| contour.len() >= 3);
    let area: f32 = contours
        .iter()
        .flat_map(|contour| {
            let next = contour.iter().cycle().skip(1);
            contour.iter().zip(next).map(|(a, b)| a.x * b.y - b.x * a.y)
        })
        .sum();
    if area == 0.0 {
        return;
    }
    // The outward normal of an edge: to the right of an outer contour wound
    // counterclockwise in y-up coordinates, to the left of a clockwise one.
    let outward = |direction: Vector| {
        if area > 0.0 {
            Vector::new(direction.y, -direction.x)
        } else {
            Vector::new(-direction.y, direction.x)
        }
    };
    let half = width * 0.5;
    commands.clear();
    for contour in &contours {
        let count = contour.len();
        let start = commands.len();
        for index in 0..count {
            let previous = contour[(index + count - 1) % count];
            let point = contour[index];
            let next = contour[(index + 1) % count];
            let incoming = (point - previous).normalize();
            let outgoing = (next - point).normalize();
            let [a, b] = [outward(incoming), outward(outgoing)];
            let cosine = incoming.dot(outgoing);
            if 1.0 + cosine < 2.0 / 16.0 {
                commands.push(Command::LineTo(point + a * half));
                commands.push(Command::LineTo(point + b * half));
            } else {
                commands.push(Command::LineTo(point + (a + b) * (half / (1.0 + cosine))));
            }
        }
        if let Some(Command::LineTo(first)) = commands.get(start) {
            commands[start] = Command::MoveTo(*first);
        }
        commands.push(Command::Close);
    }
}

#[derive(Default)]
struct Path {
    commands: Vec<Command>,
    position: Vector,
    /// Draw quadratic and cubic curves as lines.
    flatten: bool,
    snap_coverage: bool,
}

impl Path {
    fn endpoint(&self, point: Vector) -> Vector {
        if self.snap_coverage {
            snap(point)
        } else {
            point
        }
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
        if self.flatten {
            self.quadratic(self.position, control, end);
        } else {
            self.commands.push(Command::QuadTo(control, end));
        }
        self.position = end;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, to_x: f32, to_y: f32) {
        let start = self.position;
        let [first, second] = [Vector::new(x1, y1), Vector::new(x2, y2)];
        let end = Vector::new(to_x, to_y);
        self.position = end;
        if !self.flatten {
            self.commands
                .push(Command::CurveTo(first, second, self.endpoint(end)));
            return;
        }
        // A cubic strays from its chord by at most 3/4 of the largest
        // second difference of its control points. Split it to an eighth
        // of a pixel.
        let bend = |a: Vector, b: Vector, c: Vector| {
            let d = a - b * 2.0 + c;
            d.x.abs().max(d.y.abs())
        };
        let deviation = 0.75 * bend(start, first, second).max(bend(first, second, end));
        let segments = ((deviation * 8.0).sqrt().ceil() as u32).clamp(1, 64);
        for segment in 1..=segments {
            let t = segment as f32 / segments as f32;
            let u = 1.0 - t;
            let point = start * (u * u * u)
                + first * (3.0 * u * u * t)
                + second * (3.0 * u * t * t)
                + end * (t * t * t);
            self.commands.push(Command::LineTo(self.endpoint(point)));
        }
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
            flatten: true,
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
            synthesis: Synthesis::default(),
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

    /// The coverage of each row of a mask, in pixels.
    fn rows(image: &Image) -> Vec<f32> {
        image
            .data
            .chunks(image.placement.width as usize)
            .map(|row| row.iter().map(|&value| f32::from(value) / 255.0).sum())
            .collect()
    }

    /// The midpoint of a stem's edges in a row, from the coverage of the
    /// pixels at either end.
    fn centre(image: &Image, row: usize) -> f32 {
        let width = image.placement.width as usize;
        let row = &image.data[row * width..(row + 1) * width];
        let first = row.iter().position(|&value| value > 0).unwrap();
        let last = row.iter().rposition(|&value| value > 0).unwrap();
        let coverage = |x: usize| f32::from(row[x]) / 255.0;
        let left = first as f32 + 1.0 - coverage(first);
        let right = last as f32 + coverage(last);
        image.placement.left as f32 + (left + right) / 2.0
    }

    #[test]
    fn synthesizes_bold_and_italic_as_skia_does() {
        let bytes = wuff::decompress_woff2(include_bytes!(
            "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
        ))
        .unwrap();
        let glyph_id = FontRef::new(&bytes)
            .unwrap()
            .charmap()
            .map('l')
            .unwrap()
            .to_u32();
        let data = Blob::new(Arc::new(bytes));
        let mut outlines = GrayOutlines::default();
        let mut render = |size: f32, synthesis| {
            let instance = FontInstance {
                font: FontData {
                    data: data.clone(),
                    index: 0,
                },
                size,
                coords: Vec::new(),
                synthesis,
            };
            let key = GlyphKey {
                font: FontInstanceId(size as u64),
                glyph_id,
                phase: [0, 0],
                scale: 1024,
            };
            outlines.render(key, &instance, Vector::ZERO).unwrap()
        };
        let bold = Synthesis {
            bold: true,
            italic: false,
        };
        let italic = Synthesis {
            bold: false,
            italic: true,
        };
        // Chromium 159 widens the stem of an "l" by 3.12px at 100px, 0.73px
        // at 20px and 0.50px at 12px, half on each side, and raises the top
        // and lowers the foot by half as much.
        for (size, wider) in [(100.0, 3.125), (20.0, 0.749), (12.0, 0.486)] {
            assert!((fake_bold_width(size) - wider).abs() < 0.001);
            let regular = render(size, Synthesis::default());
            let emboldened = render(size, bold);
            let middle = |image: &Image| rows(image)[image.placement.height as usize / 2];
            let growth = middle(&emboldened) - middle(&regular);
            assert!((growth - wider).abs() < 0.05, "{size}px: {growth}");
            let centres = [&regular, &emboldened]
                .map(|image| centre(image, image.placement.height as usize / 2));
            assert!((centres[0] - centres[1]).abs() < 0.05, "{centres:?}");
        }
        let regular = render(100.0, Synthesis::default());
        let emboldened = render(100.0, bold);
        let extent = |image: &Image| {
            let rows = rows(image);
            rows.iter().sum::<f32>() / rows[rows.len() / 2]
        };
        assert!((extent(&emboldened) - extent(&regular) - 3.125).abs() < 0.2);

        // The slant moves the top a quarter of the height to the right.
        let slanted = render(100.0, italic);
        assert_eq!(slanted.placement.height, regular.placement.height);
        let [top, bottom] = [5, slanted.placement.height as usize - 5];
        let shift = centre(&slanted, top) - centre(&slanted, bottom);
        assert!(
            (shift - 0.25 * (bottom - top) as f32).abs() < 0.1,
            "{shift}"
        );
        let upright = centre(&regular, top) - centre(&regular, bottom);
        assert!(upright.abs() < 0.05);
    }
}
