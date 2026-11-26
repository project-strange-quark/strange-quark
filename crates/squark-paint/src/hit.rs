// SPDX-License-Identifier: MIT

use std::{sync::Arc, time::Instant};

use kurbo::Shape;
use squark_types::{
    NodeId, Point, Rect, Size,
    fragment::Cursor,
    host::ListenerFlags,
    render::{FrameId, Viewport},
};

use crate::{
    ClipId, ClipShape, CommittedFrame, FillRule, Matrix, PropertyRefs, PropertyTrees, RoundedRect,
    input::MirrorRead,
    properties::{StickyNode, multiply, translation},
};

#[derive(Clone, Debug)]
pub struct HitTestTree {
    /// Back-to-front CSS paint order.
    pub regions: Arc<[HitRegion]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HitRegion {
    pub target: NodeId,
    pub props: PropertyRefs,
    pub shape: RoundedRect,
    pub cursor: Cursor,
    pub pointer_events: bool,
    pub listeners: ListenerFlags,
    pub scroll_chain: Box<[NodeId]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub target: NodeId,
    pub cursor: Cursor,
    pub listeners: ListenerFlags,
    pub scroll_chain: Box<[NodeId]>,
}

#[derive(Debug)]
pub struct HitSnapshot {
    pub frame: FrameId,
    pub viewport: Viewport,
    pub regions: HitTestTree,
    pub resolved: Arc<ResolvedHitProperties>,
    pub scrolls: Box<[crate::ScrollNode]>,
    pub sampled_at: Option<Instant>,
}

impl HitSnapshot {
    /// Resolve the same transforms and scroll offsets used to render the frame.
    pub fn new(committed: &CommittedFrame) -> Self {
        Self {
            frame: committed.id,
            viewport: committed.viewport,
            regions: committed.frame.hit_test.clone(),
            resolved: Arc::new(ResolvedHitProperties::new(&committed.frame.property_trees)),
            scrolls: committed.frame.property_trees.scrolls.clone(),
            sampled_at: None,
        }
    }
}

pub fn hit_test(snapshot: &HitSnapshot, css: Point) -> Option<Hit> {
    let Size { width, height } = snapshot.viewport.css;
    if !squark_types::Rect::new(0.0, 0.0, width, height).contains(css) {
        return None;
    }
    let region = hit_region(&snapshot.regions, &snapshot.resolved, css)?;
    Some(Hit {
        target: region.target,
        cursor: region.cursor,
        listeners: region.listeners,
        scroll_chain: region.scroll_chain.clone(),
    })
}

pub fn hit_test_tree(
    tree: &HitTestTree,
    properties: &PropertyTrees,
    point: Point,
) -> Option<NodeId> {
    let resolved = ResolvedHitProperties::new(properties);
    hit_region(tree, &resolved, point).map(|region| region.target)
}

fn hit_region<'a>(
    tree: &'a HitTestTree,
    properties: &ResolvedHitProperties,
    point: Point,
) -> Option<&'a HitRegion> {
    tree.regions.iter().rev().find(|region| {
        region.pointer_events
            && properties.inverse[region.props.transform.0 as usize]
                .and_then(|inverse| inverse.project(point))
                .is_some_and(|local| rounded_contains(region.shape, local))
            && properties.inside_clip(region.props.clip, point)
    })
}

#[derive(Debug)]
pub struct ResolvedHitProperties {
    inverse: Box<[Option<InversePlane>]>,
    clips: Arc<[crate::ClipNode]>,
}

impl ResolvedHitProperties {
    fn new(properties: &PropertyTrees) -> Self {
        let world = world_transforms(properties);
        let inverse = world.into_iter().map(InversePlane::new).collect();
        Self {
            inverse,
            clips: properties.clips.clone(),
        }
    }

    fn inside_clip(&self, mut clip: ClipId, point: Point) -> bool {
        while clip != ClipId(0) {
            let node = &self.clips[clip.0 as usize];
            if let Some(shape) = &node.shape
                && !self.inverse[node.space.0 as usize]
                    .and_then(|inverse| inverse.project(point))
                    .is_some_and(|local| clip_contains(shape, local))
            {
                return false;
            }
            clip = node.parent;
        }
        true
    }
}

pub fn world_transforms(properties: &PropertyTrees) -> Vec<Matrix> {
    let mut world = Vec::with_capacity(properties.transforms.len());
    for node in properties.transforms.iter() {
        let mut local = node.local;
        if let Some(sticky) = node.sticky {
            let scroll = &properties.scrolls[sticky.scroll.0 as usize];
            let offset = sticky_offset(sticky, scroll.scrollport, scroll.initial_offset);
            local = multiply(translation(offset.x, offset.y, 0.0), local);
        }
        if node.scroll != crate::ScrollId(0) {
            let offset = properties.scrolls[node.scroll.0 as usize].initial_offset;
            local = multiply(local, translation(-offset.x, -offset.y, 0.0));
        }
        if let Some(&parent) = world.get(node.parent.0 as usize) {
            local = multiply(parent, local);
        }
        world.push(local);
    }
    world
}

fn sticky_offset(sticky: StickyNode, port: Rect, scroll: Point) -> Point {
    let normal = sticky.normal_rect;
    let containing = sticky.containing_rect;
    let [top, right, bottom, left] = sticky.insets;
    Point {
        x: sticky_axis(
            [normal.origin.x, normal.max_x()],
            [containing.origin.x, containing.max_x()],
            [port.origin.x + scroll.x, port.max_x() + scroll.x],
            [left, right],
        ),
        y: sticky_axis(
            [normal.origin.y, normal.max_y()],
            [containing.origin.y, containing.max_y()],
            [port.origin.y + scroll.y, port.max_y() + scroll.y],
            [top, bottom],
        ),
    }
}

fn sticky_axis(
    [start, end]: [f32; 2],
    [containing_start, containing_end]: [f32; 2],
    [port_start, port_end]: [f32; 2],
    [start_inset, end_inset]: [Option<f32>; 2],
) -> f32 {
    let view_start = port_start + start_inset.unwrap_or_default();
    let mut shift = start_inset.map_or(0.0, |_| (view_start - start).max(0.0));
    if let Some(end_inset) = end_inset {
        let view_end = (port_end - end_inset).max(view_start + end - start);
        shift = shift.min(view_end - end);
    }
    shift.clamp(
        (containing_start - start).min(0.0),
        (containing_end - end).max(0.0),
    )
}

/// Projective inverse restricted to the painted z=0 plane. Inverting the
/// full 4x4 and inserting a screen-space z=0 would miss tilted planes.
#[derive(Clone, Copy, Debug)]
pub struct InversePlane(pub [[f64; 3]; 3]);

impl InversePlane {
    pub fn new(world: Matrix) -> Option<Self> {
        let [x, y, w] = [0, 1, 3].map(|i| [0, 1, 3].map(|j| f64::from(world[i][j])));
        let cross = |a: [f64; 3], b: [f64; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let adjugate = [cross(y, w), cross(w, x), cross(x, y)];
        let determinant: f64 = (0..3).map(|i| x[i] * adjugate[0][i]).sum();
        if determinant == 0.0 || !determinant.is_finite() {
            return None;
        }
        Some(Self(
            adjugate.map(|row| row.map(|value| value / determinant)),
        ))
    }

    pub fn project(self, point: Point) -> Option<Point> {
        let x = f64::from(point.x);
        let y = f64::from(point.y);
        let [u, v, w] = self.0.map(|row| row[0] * x + row[1] * y + row[2]);
        let local = Point {
            x: (u / w) as f32,
            y: (v / w) as f32,
        };
        (w > 0.0 && local.x.is_finite() && local.y.is_finite()).then_some(local)
    }
}

fn clip_contains(shape: &ClipShape, point: Point) -> bool {
    match shape {
        ClipShape::Rect(rect) => rect.contains(point),
        ClipShape::RoundedRect(shape) => rounded_contains(*shape, point),
        ClipShape::Path(path, rule) => {
            let winding = path.winding(kurbo::Point::new(f64::from(point.x), f64::from(point.y)));
            match rule {
                FillRule::NonZero => winding != 0,
                FillRule::EvenOdd => winding.rem_euclid(2) == 1,
            }
        }
        ClipShape::AxisAligned {
            rect,
            axes: [clip_x, clip_y],
        } => {
            (!clip_x || (point.x >= rect.origin.x && point.x < rect.max_x()))
                && (!clip_y || (point.y >= rect.origin.y && point.y < rect.max_y()))
        }
    }
}

fn rounded_contains(shape: RoundedRect, point: Point) -> bool {
    let rect = shape.rect;
    if !rect.contains(point) {
        return false;
    }
    let corners = [
        (point.x - rect.origin.x, point.y - rect.origin.y),
        (rect.max_x() - point.x, point.y - rect.origin.y),
        (rect.max_x() - point.x, rect.max_y() - point.y),
        (point.x - rect.origin.x, rect.max_y() - point.y),
    ];
    corners
        .into_iter()
        .zip(shape.radii)
        .all(|((x, y), radius)| {
            x >= radius.width
                || y >= radius.height
                || (1.0 - x / radius.width).powi(2) + (1.0 - y / radius.height).powi(2) <= 1.0
        })
}

pub(crate) fn element_target(mirror: &dyn MirrorRead, mut id: NodeId) -> Option<NodeId> {
    while !id.is_none() {
        if mirror.is_element(id) {
            return Some(id);
        }
        id = mirror.parent(id)?;
    }
    None
}

pub(crate) fn listeners(mirror: &dyn MirrorRead, mut id: NodeId) -> ListenerFlags {
    let mut bits = mirror.listeners(NodeId::NONE).bits();
    while !id.is_none() {
        bits |= mirror.listeners(id).bits();
        id = mirror.parent(id).unwrap_or(NodeId::NONE);
    }
    ListenerFlags::from_bits(bits).expect("union of validated listener flags")
}
