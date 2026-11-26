// SPDX-License-Identifier: MIT

use std::sync::Arc;

use squark_paint::{
    ClipId, ClipShape, CommittedFrame, Damage, Matrix, Primitive, ScrollId, TransformId,
    project_bounds, world_transforms,
};
use squark_types::{Rect, Size, render::Viewport};

use super::Target;

pub(super) const TILE_SIZE: u32 = 512;
pub(super) const TEXTURE_BUDGET: u64 = 256 * 1024 * 1024;
pub(super) type PixelRect = [u32; 4];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileStats {
    pub rasterized: usize,
    pub shifted: usize,
    pub reused: usize,
}

pub(super) struct TileCache {
    pub scene: Target,
    pub tiles: Vec<Tile>,
    pub previous: Option<CommittedFrame>,
    pub viewport: Viewport,
}

pub(super) struct Tile {
    pub area: PixelRect,
    pub target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Update {
    Keep,
    Rasterize,
    Shift {
        source: [u32; 2],
        destination: PixelRect,
    },
}

impl TileCache {
    pub fn new(device: &wgpu::Device, viewport: Viewport) -> Self {
        let [width, height] = viewport.device;
        let tiles = (0..height)
            .step_by(TILE_SIZE as usize)
            .flat_map(|y| {
                (0..width).step_by(TILE_SIZE as usize).map(move |x| {
                    let w = TILE_SIZE.min(width - x);
                    let h = TILE_SIZE.min(height - y);
                    Tile {
                        area: [x, y, w, h],
                        target: Target::new(device, [w, h]),
                    }
                })
            })
            .collect();
        Self {
            scene: Target::new(device, viewport.device),
            tiles,
            previous: None,
            viewport,
        }
    }

    pub fn updates(&self, frame: &CommittedFrame) -> Vec<Update> {
        let Some(old) = &self.previous else {
            return vec![Update::Rasterize; self.tiles.len()];
        };
        let a = &old.frame;
        let b = &frame.frame;
        if a.resource_updates != b.resource_updates {
            return vec![Update::Rasterize; self.tiles.len()];
        }
        if a.property_trees == b.property_trees && same_items(old, frame) {
            return vec![Update::Keep; self.tiles.len()];
        }
        if let Some(motion) = scroll_motion(old, frame) {
            return self
                .tiles
                .iter()
                .map(|tile| motion.update(tile.area))
                .collect();
        }
        if let Damage::Rects { base, rects } = &b.damage
            && *base == old.id
            && a.property_trees == b.property_trees
        {
            let mut damage = Vec::with_capacity(rects.len());
            for &rect in rects {
                if let Some(pixels) = pixel_bounds(rect, frame.viewport, false) {
                    damage.push(pixels);
                }
            }
            return self
                .tiles
                .iter()
                .map(|tile| {
                    if damage
                        .iter()
                        .any(|rect| intersection(tile.area, *rect).is_some())
                    {
                        Update::Rasterize
                    } else {
                        Update::Keep
                    }
                })
                .collect();
        }
        vec![Update::Rasterize; self.tiles.len()]
    }
}

pub(super) fn intersection(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x = a[0].max(b[0]);
    let y = a[1].max(b[1]);
    let right = (a[0] + a[2]).min(b[0] + b[2]);
    let bottom = (a[1] + a[3]).min(b[1] + b[3]);
    (right > x && bottom > y).then(|| [x, y, right - x, bottom - y])
}

fn pixel_bounds(rect: Rect, viewport: Viewport, aligned: bool) -> Option<PixelRect> {
    let scale = viewport.scale;
    let [mut x, mut y, mut right, mut bottom] =
        [rect.origin.x, rect.origin.y, rect.max_x(), rect.max_y()]
            .map(|edge| f64::from(edge) * scale);
    if ![x, y, right, bottom].iter().all(|edge| edge.is_finite()) {
        return None;
    }
    if aligned {
        if [x, y, right, bottom]
            .iter()
            .any(|edge| (edge - edge.round()).abs() > 1e-4)
        {
            return None;
        }
        [x, y, right, bottom] = [x.round(), y.round(), right.round(), bottom.round()];
    } else {
        [x, y, right, bottom] = [x.floor(), y.floor(), right.ceil(), bottom.ceil()];
    }
    let [width, height] = viewport.device;
    let x = x.clamp(0.0, f64::from(width)) as u32;
    let y = y.clamp(0.0, f64::from(height)) as u32;
    let right = right.clamp(0.0, f64::from(width)) as u32;
    let bottom = bottom.clamp(0.0, f64::from(height)) as u32;
    (right > x && bottom > y).then(|| [x, y, right - x, bottom - y])
}

#[derive(Clone, Copy)]
struct Motion {
    port: PixelRect,
    common: PixelRect,
    delta: [i64; 2],
}

impl Motion {
    fn new(port: PixelRect, delta: [i64; 2]) -> Option<Self> {
        let [x, y, w, h] = port.map(i64::from);
        let [dx, dy] = delta;
        if dx <= -w || dx >= w || dy <= -h || dy >= h || delta == [0; 2] {
            return None;
        }
        let common = [
            (x + dx.max(0)) as u32,
            (y + dy.max(0)) as u32,
            (w - dx.abs()) as u32,
            (h - dy.abs()) as u32,
        ];
        Some(Self {
            port,
            common,
            delta,
        })
    }

    fn update(self, tile: PixelRect) -> Update {
        let Some(part) = intersection(tile, self.port) else {
            return Update::Keep;
        };
        if intersection(tile, self.common) != Some(part) {
            return Update::Rasterize;
        }
        Update::Shift {
            source: [
                (i64::from(part[0]) - self.delta[0]) as u32,
                (i64::from(part[1]) - self.delta[1]) as u32,
            ],
            destination: part,
        }
    }
}

/// A scrollport can move already-composited pixels when only its content
/// moves, and every stationary item covering it is spatially uniform.
fn scroll_motion(old: &CommittedFrame, next: &CommittedFrame) -> Option<Motion> {
    if !same_items(old, next) {
        return None;
    }
    let a = &old.frame.property_trees;
    let b = &next.frame.property_trees;
    if a.transforms != b.transforms
        || a.clips != b.clips
        || a.scrolls.len() != b.scrolls.len()
        || a.transforms.iter().any(|node| node.sticky.is_some())
    {
        return None;
    }
    let mut changed = a
        .scrolls
        .iter()
        .zip(&*b.scrolls)
        .enumerate()
        .filter(|(_, (a, b))| a != b);
    let (index, (previous, scroller)) = changed.next()?;
    if changed.next().is_some() || index == 0 {
        return None;
    }
    let mut geometry = scroller.clone();
    geometry.initial_offset = previous.initial_offset;
    geometry.generation = previous.generation;
    if &geometry != previous {
        return None;
    }
    let id = ScrollId(index as u32);
    let world = world_transforms(b);
    let space = world[scroller.space.0 as usize];
    if !translation_only(space) {
        return None;
    }
    let port = project_bounds(scroller.scrollport, space)?;
    let port_pixels = pixel_bounds(port, next.viewport, true)?;
    let viewport_rect = Rect::new(0.0, 0.0, next.viewport.css.width, next.viewport.css.height);
    // Reused source pixels must have been visible in the previous scene.
    if !contains(viewport_rect, port) {
        return None;
    }
    let scale = next.viewport.scale;
    let delta = [
        f64::from(previous.initial_offset.x - scroller.initial_offset.x) * scale,
        f64::from(previous.initial_offset.y - scroller.initial_offset.y) * scale,
    ];
    if delta
        .iter()
        .any(|n| !n.is_finite() || (n - n.round()).abs() > 1e-4)
    {
        return None;
    }
    let motion = Motion::new(port_pixels, delta.map(|n| n.round() as i64))?;

    let moving = |mut transform: TransformId| {
        while transform != TransformId(0) {
            let node = &b.transforms[transform.0 as usize];
            if node.scroll == id {
                return true;
            }
            transform = node.parent;
        }
        false
    };
    let stationary_clip = |mut clip: ClipId| -> Option<bool> {
        let mut clips_to_port = false;
        while clip != ClipId(0) {
            let node = &b.clips[clip.0 as usize];
            if !moving(node.space) {
                let rect = match &node.shape {
                    Some(ClipShape::Rect(rect)) => *rect,
                    Some(ClipShape::RoundedRect(shape)) if shape.radii == [Size::default(); 4] => {
                        shape.rect
                    }
                    _ => return None,
                };
                let transform = world[node.space.0 as usize];
                if !translation_only(transform) {
                    return None;
                }
                let clip_bounds = project_bounds(rect, transform)?;
                if !contains(clip_bounds, port) {
                    return None;
                }
                clips_to_port |= clip_bounds == port;
            }
            clip = node.parent;
        }
        Some(clips_to_port)
    };

    for item in next.frame.display_list.iter() {
        let props = item.properties;
        if moving(props.transform) {
            if !stationary_clip(props.clip)? {
                return None;
            }
        } else {
            let transform = world[props.transform.0 as usize];
            let bounds = project_bounds(item.bounds, transform)?;
            // Glyph bearings and path strokes can paint outside their fragment.
            if matches!(item.primitive, Primitive::GlyphRun(_) | Primitive::Path(_)) {
                return None;
            }
            let fringe = (1.0 / scale) as f32;
            let ink = Rect::new(
                bounds.origin.x - fringe,
                bounds.origin.y - fringe,
                bounds.size.width + 2.0 * fringe,
                bounds.size.height + 2.0 * fringe,
            );
            if let Some(overlap) = ink.intersection(&port)
                && overlap.size.width > 0.0
                && overlap.size.height > 0.0
            {
                let Primitive::RoundedRect { shape, .. } = item.primitive else {
                    return None;
                };
                if shape.radii != [Size::default(); 4]
                    || !translation_only(transform)
                    || !contains(bounds, port)
                {
                    return None;
                }
                stationary_clip(props.clip)?;
            }
        }
    }
    Some(motion)
}

fn same_items(a: &CommittedFrame, b: &CommittedFrame) -> bool {
    Arc::ptr_eq(&a.frame.display_list, &b.frame.display_list)
        || a.frame.display_list == b.frame.display_list
}

fn contains(outer: Rect, inner: Rect) -> bool {
    outer.origin.x <= inner.origin.x
        && outer.origin.y <= inner.origin.y
        && outer.max_x() >= inner.max_x()
        && outer.max_y() >= inner.max_y()
}

fn translation_only(matrix: Matrix) -> bool {
    matrix[0] == [1.0, 0.0, 0.0, 0.0]
        && matrix[1] == [0.0, 1.0, 0.0, 0.0]
        && matrix[2] == [0.0, 0.0, 1.0, 0.0]
        && matrix[3][3] == 1.0
}

pub(super) fn copy(
    encoder: &mut wgpu::CommandEncoder,
    from: &Target,
    source: [u32; 2],
    to: &Target,
    destination: PixelRect,
) {
    let [x, y, width, height] = destination;
    let origin = |[x, y]: [u32; 2]| wgpu::Origin3d { x, y, z: 0 };
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            origin: origin(source),
            ..from.texture.as_image_copy()
        },
        wgpu::TexelCopyTextureInfo {
            origin: origin([x, y]),
            ..to.texture.as_image_copy()
        },
        crate::extent(width, height),
    );
}
