// SPDX-License-Identifier: MIT

use std::time::{Duration, Instant};

use squark_types::{
    NodeId, Rect, Size,
    engine::{
        CssLength, IntersectionEntry, IntersectionOptions, LogicalSize, ResizeBox, ResizeDelivery,
        ResizeEntry,
    },
    fragment::{DisplayKind, Fragment, FragmentKind, PositionKind},
};

use crate::{
    fragments::inset,
    query::{Geometry, bounding},
};

#[derive(Default)]
pub(crate) struct Observers {
    pub resize: Vec<ResizeObservation>,
    pub intersection: Vec<IntersectionObservation>,
}

pub(crate) struct ResizeObservation {
    pub observer: u32,
    pub target: NodeId,
    pub kind: ResizeBox,
    pub last: Option<LogicalSize>,
}

pub(crate) struct IntersectionObservation {
    pub observer: u32,
    pub target: NodeId,
    pub options: IntersectionOptions,
    pub last: Option<(usize, bool)>,
}

impl Observers {
    pub fn observe_resize(&mut self, observer: u32, target: NodeId, kind: ResizeBox) {
        if let Some(entry) = self
            .resize
            .iter_mut()
            .find(|r| r.observer == observer && r.target == target)
        {
            if entry.kind != kind {
                entry.kind = kind;
                entry.last = None;
            }
        } else {
            self.resize.push(ResizeObservation {
                observer,
                target,
                kind,
                last: None,
            });
        }
    }

    pub fn observe_intersection(
        &mut self,
        observer: u32,
        target: NodeId,
        mut options: IntersectionOptions,
    ) {
        if self
            .intersection
            .iter()
            .any(|r| r.observer == observer && r.target == target)
        {
            return;
        }
        options
            .thresholds
            .retain(|value| (0.0..=1.0).contains(value));
        options.thresholds.sort_by(f64::total_cmp);
        self.intersection.push(IntersectionObservation {
            observer,
            target,
            options,
            last: None,
        });
    }

    pub fn forget(&mut self, node: NodeId) {
        self.resize.retain(|r| r.target != node);
        self.intersection
            .retain(|r| r.target != node && r.options.root != Some(node));
    }

    pub fn resize_entries(
        &mut self,
        geometry: &Geometry<'_>,
        scale: f64,
        deeper_than: u32,
    ) -> ResizeDelivery {
        let mut delivery = ResizeDelivery {
            next_depth: u32::MAX,
            ..Default::default()
        };
        for observation in &mut self.resize {
            let target = observation.target;
            let mut depth = 0;
            let mut last = Some(target);
            while let Some(node) = last {
                depth += 1;
                last = geometry.document.flat_parent(node);
            }
            let vertical = geometry
                .document
                .computed_style(target)
                .is_some_and(|css| css.writing_mode.is_vertical());
            let logical = |size: Size| {
                let (inline_size, block_size) = if vertical {
                    (size.height, size.width)
                } else {
                    (size.width, size.height)
                };
                LogicalSize {
                    inline_size: f64::from(inline_size),
                    block_size: f64::from(block_size),
                }
            };
            let fragment = geometry.boxes(target).find(|f| {
                f.kind == FragmentKind::Replaced || f.style.stacking.display != DisplayKind::Inline
            });
            let content = fragment.map_or_else(Rect::default, |f| {
                let inner = inset(inset(f.border_box, f.border), f.padding);
                f.scroll.map_or(inner, |scroll| inset(inner, scroll.gutter))
            });
            let border = fragment.map_or_else(Size::default, |f| f.border_box.size);
            let device = |coordinate| (f64::from(coordinate) * scale).round();
            let device_content = Size {
                width: (device(content.max_x()) - device(content.origin.x)) as f32,
                height: (device(content.max_y()) - device(content.origin.y)) as f32,
            };
            let [content_size, border_size, device_size] =
                [content.size, border, device_content].map(logical);
            let size = match observation.kind {
                ResizeBox::ContentBox => content_size,
                ResizeBox::BorderBox => border_size,
                ResizeBox::DevicePixelContentBox => device_size,
            };
            if observation.last == Some(size) {
                continue;
            }
            if depth <= deeper_than {
                delivery.skipped = true;
                continue;
            }
            observation.last = Some(size);
            delivery.next_depth = delivery.next_depth.min(depth);
            let content_rect = fragment.map_or_else(Rect::default, |f| {
                Rect::new(
                    f.padding.left,
                    f.padding.top,
                    content.size.width,
                    content.size.height,
                )
            });
            delivery.entries.push(ResizeEntry {
                observer: observation.observer,
                target,
                depth,
                content_rect,
                content_box: vec![content_size],
                border_box: vec![border_size],
                device_pixel_content_box: vec![device_size],
            });
        }
        if delivery.entries.is_empty() {
            delivery.next_depth = deeper_than;
        }
        delivery
    }

    pub fn intersection_entries(
        &mut self,
        geometry: &Geometry<'_>,
        origin: Instant,
        now: Instant,
    ) -> Vec<IntersectionEntry> {
        let time = now.saturating_duration_since(origin);
        self.intersection
            .iter_mut()
            .filter_map(|observation| {
                let entry = intersect(geometry, observation, time);
                let index = observation
                    .options
                    .thresholds
                    .partition_point(|value| *value <= entry.ratio);
                let state = (index, entry.is_intersecting);
                (observation.last.replace(state) != Some(state)).then_some(entry)
            })
            .collect()
    }
}

fn intersect(
    geometry: &Geometry<'_>,
    observation: &IntersectionObservation,
    time: Duration,
) -> IntersectionEntry {
    let target = observation.target;
    let root = explicit_root(geometry, observation.options.root);
    let root_bounds = root.map(|root| {
        let bounds = root_clip(root);
        let rect = geometry.client_rect(root, bounds);
        expand(rect, observation.options.root_margin)
    });
    let mut bounds = Rect::default();
    let mut intersection = Rect::default();
    let mut is_intersecting = false;
    if let (Some(root), Some(clip)) = (root, root_bounds) {
        let boxes: Vec<_> = geometry.boxes(target).collect();
        let eligible = |fragment: &&Fragment| {
            root.kind == FragmentKind::Viewport || establishes_boundary(geometry, fragment, root)
        };
        let targets: Vec<_> = boxes.into_iter().filter(eligible).collect();
        bounds = bounding(
            targets
                .iter()
                .map(|f| geometry.client_rect(f, f.border_box)),
        );
        let mut visible = targets
            .into_iter()
            .filter_map(|fragment| {
                let rect = geometry.client_rect(fragment, fragment.border_box);
                clip_ancestors(geometry, fragment, root, rect).and_then(|rect| overlap(rect, clip))
            })
            .peekable();
        is_intersecting = visible.peek().is_some();
        intersection = bounding(visible);
    }
    let area = |rect: Rect| f64::from(rect.size.width) * f64::from(rect.size.height);
    let ratio = if area(bounds) > 0.0 {
        (area(intersection) / area(bounds)).min(1.0)
    } else {
        u8::from(is_intersecting).into()
    };
    IntersectionEntry {
        observer: observation.observer,
        target,
        time,
        root_bounds,
        bounds,
        intersection,
        ratio,
        is_intersecting,
    }
}

fn explicit_root<'a>(geometry: &Geometry<'a>, root: Option<NodeId>) -> Option<&'a Fragment> {
    match root {
        Some(root) if root != geometry.document.root() => geometry.boxes(root).next(),
        _ => geometry.tree.nodes.get(&geometry.tree.root),
    }
}

fn establishes_boundary(geometry: &Geometry<'_>, target: &Fragment, root: &Fragment) -> bool {
    if target.id == root.id {
        return false;
    }
    let parent = |f: &Fragment| {
        if matches!(
            f.style.stacking.position,
            PositionKind::Absolute | PositionKind::Fixed
        ) {
            f.containing_block
        } else {
            f.parent
        }
        .and_then(|id| geometry.tree.nodes.get(&id))
    };
    std::iter::successors(parent(target), |fragment| parent(fragment)).any(|f| f.id == root.id)
}

fn root_clip(root: &Fragment) -> Rect {
    root.scroll
        .filter(|s| s.clip_x || s.clip_y)
        .map_or(root.border_box, |s| s.scrollport)
}

fn clip_ancestors(
    geometry: &Geometry<'_>,
    fragment: &Fragment,
    root: &Fragment,
    mut rect: Rect,
) -> Option<Rect> {
    if fragment.style.stacking.position == PositionKind::Fixed
        && fragment.containing_block == Some(geometry.tree.root)
    {
        return Some(rect);
    }
    for parent in geometry.ancestors(fragment).take_while(|f| f.id != root.id) {
        if let Some(scroll) = parent.scroll.filter(|s| s.clip_x || s.clip_y) {
            let port = geometry.client_rect(parent, scroll.scrollport);
            let clip = Rect::new(
                if scroll.clip_x {
                    port.origin.x
                } else {
                    rect.origin.x
                },
                if scroll.clip_y {
                    port.origin.y
                } else {
                    rect.origin.y
                },
                if scroll.clip_x {
                    port.size.width
                } else {
                    rect.size.width
                },
                if scroll.clip_y {
                    port.size.height
                } else {
                    rect.size.height
                },
            );
            rect = overlap(rect, clip)?;
        }
    }
    Some(rect)
}

fn expand(rect: Rect, margin: [CssLength; 4]) -> Rect {
    let [top, right, bottom, left] = margin.map(|length| match length {
        CssLength::Px(px) => px as f32,
        CssLength::Percent(p) => rect.size.width * (p / 100.0) as f32,
    });
    Rect::new(
        rect.origin.x - left,
        rect.origin.y - top,
        (rect.size.width + left + right).max(0.0),
        (rect.size.height + top + bottom).max(0.0),
    )
}

fn overlap(a: Rect, b: Rect) -> Option<Rect> {
    let x = a.origin.x.max(b.origin.x);
    let y = a.origin.y.max(b.origin.y);
    let width = a.max_x().min(b.max_x()) - x;
    let height = a.max_y().min(b.max_y()) - y;
    (width >= 0.0 && height >= 0.0).then(|| Rect::new(x, y, width, height))
}
