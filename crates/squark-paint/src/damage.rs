// SPDX-License-Identifier: MIT

use std::collections::HashMap;

use squark_types::{Rect, render::Viewport};

use crate::{CommittedFrame, Damage, DisplayItem, Frame, Matrix, Primitive, hit::world_transforms};

pub fn frame_damage(previous: &CommittedFrame, viewport: Viewport, next: &Frame) -> Damage {
    let old = &previous.frame;
    if previous.viewport != viewport
        || old.property_trees != next.property_trees
        || old.resource_updates != next.resource_updates
    {
        return Damage::Full;
    }
    if old.display_list == next.display_list {
        return Damage::Rects {
            base: previous.id,
            rects: Box::new([]),
        };
    }
    // A changed item can affect other pixels through blur, blending or a
    // backdrop filter. Invalidate the whole frame until those effects have
    // layer bounds and dependency tracking.
    if next.property_trees.effects.len() > 1 {
        return Damage::Full;
    }
    let mut before: HashMap<_, _> = old
        .display_list
        .iter()
        .enumerate()
        .map(|(index, item)| (item.id, (index, item)))
        .collect();
    let mut last = None;
    let mut rects = Vec::new();
    let world = world_transforms(&next.property_trees);
    let mut add = |item: &DisplayItem| {
        // Raster bearings and subpixel phases can carry ink outside the
        // layout rect. SVG stroke bounds can also exceed the layout rect.
        if matches!(item.primitive, Primitive::GlyphRun(_) | Primitive::Path(_)) {
            return false;
        }
        let matrix = world[item.properties.transform.0 as usize];
        let Some(bounds) = project_bounds(item.bounds, matrix) else {
            return false;
        };
        let fringe = (1.0 / viewport.scale) as f32;
        let bounds = Rect::new(
            bounds.origin.x - fringe,
            bounds.origin.y - fringe,
            bounds.size.width + 2.0 * fringe,
            bounds.size.height + 2.0 * fringe,
        );
        if let Some(visible) = bounds.intersection(&Rect::new(
            0.0,
            0.0,
            viewport.css.width,
            viewport.css.height,
        )) {
            rects.push(visible);
        }
        true
    };
    for next in next.display_list.iter() {
        if let Some((index, old)) = before.remove(&next.id) {
            if last.is_some_and(|last| index < last) {
                return Damage::Full;
            }
            last = Some(index);
            if old != next && !(add(old) && add(next)) {
                return Damage::Full;
            }
        } else if !add(next) {
            return Damage::Full;
        }
    }
    // Visit removals in their original paint order for deterministic damage.
    for old in old.display_list.iter() {
        if before.contains_key(&old.id) && !add(old) {
            return Damage::Full;
        }
    }
    Damage::Rects {
        base: previous.id,
        rects: rects.into_boxed_slice(),
    }
}

pub fn project_bounds(rect: Rect, matrix: Matrix) -> Option<Rect> {
    let mut min = [f32::INFINITY; 2];
    let mut max = [f32::NEG_INFINITY; 2];
    for x in [rect.origin.x, rect.max_x()] {
        for y in [rect.origin.y, rect.max_y()] {
            let w = matrix[0][3] * x + matrix[1][3] * y + matrix[3][3];
            if w <= 0.0 {
                return None;
            }
            for i in 0..2 {
                let coord = (matrix[0][i] * x + matrix[1][i] * y + matrix[3][i]) / w;
                if !coord.is_finite() {
                    return None;
                }
                min[i] = min[i].min(coord);
                max[i] = max[i].max(coord);
            }
        }
    }
    Some(Rect::new(min[0], min[1], max[0] - min[0], max[1] - min[1]))
}
