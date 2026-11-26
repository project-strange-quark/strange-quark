// SPDX-License-Identifier: MIT

use std::{collections::BTreeMap, sync::Mutex};

use squark_paint::{HitSnapshot, PropertyTrees, ScrollNode};
use squark_types::{NodeId, Point, ScrollOffset, fragment::OverscrollBehavior, host::DeltaMode};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Offset {
    point: Point,
    generation: u64,
}

impl From<&ScrollNode> for Offset {
    fn from(node: &ScrollNode) -> Self {
        Self {
            point: node.initial_offset,
            generation: node.generation,
        }
    }
}

#[derive(Debug, Default)]
struct State {
    current: BTreeMap<NodeId, Offset>,
    presented: BTreeMap<NodeId, Offset>,
    echo: Vec<ScrollOffset>,
}

/// Wheel updates are staged until the render latch. Only presented offsets are
/// visible to document hit testing or sent back through ScrollSync.
#[derive(Debug, Default)]
pub struct ScrollOffsets(Mutex<State>);

impl ScrollOffsets {
    pub fn wheel(
        &self,
        snapshot: &HitSnapshot,
        chain: &[NodeId],
        delta: [f64; 2],
        mode: DeltaMode,
    ) -> bool {
        let mut state = self.0.lock().unwrap();
        let mut changed = false;
        let mut remainder = delta;
        for &owner in chain {
            let Some(node) = snapshot
                .scrolls
                .iter()
                .skip(1)
                .find(|node| node.owner == owner)
            else {
                continue;
            };
            changed |= wheel_node(node, &mut state.current, &mut remainder, mode);
            if remainder == [0.0; 2] {
                break;
            }
        }
        changed
    }

    pub(crate) fn latch(&self, scrolls: &mut [ScrollNode]) -> bool {
        let mut state = self.0.lock().unwrap();
        let mut changed = false;
        for scroll in scrolls.iter_mut().skip(1) {
            let document = Offset::from(&*scroll);
            if let Some(&offset) = state.current.get(&scroll.owner)
                && offset.generation >= document.generation
            {
                let point = clamped(scroll, offset.point);
                let conflict =
                    offset.generation == document.generation && offset.point != document.point;
                scroll.initial_offset = point;
                // A simultaneous document edit can reuse the user generation.
                // Keep the compositor's offset and echo it under a newer generation.
                scroll.generation = offset
                    .generation
                    .saturating_add(u64::from(conflict || point != offset.point));
            }
            let resolved = Offset::from(&*scroll);
            if resolved != Offset::default() {
                state.current.insert(scroll.owner, resolved);
            }
            changed |= document != resolved;
        }
        changed
    }

    pub(crate) fn apply_presented(&self, properties: &mut PropertyTrees) {
        let state = self.0.lock().unwrap();
        for scroll in properties.scrolls.iter_mut().skip(1) {
            if let Some(offset) = state.presented.get(&scroll.owner)
                && offset.generation >= scroll.generation
            {
                scroll.initial_offset = clamped(scroll, offset.point);
                scroll.generation = offset.generation;
            }
        }
    }

    pub(crate) fn presented(&self, scrolls: &[ScrollNode]) {
        let mut state = self.0.lock().unwrap();
        let next: BTreeMap<_, _> = scrolls
            .iter()
            .skip(1)
            .map(|node| (node.owner, Offset::from(node)))
            .collect();
        let changes: Vec<_> = next
            .iter()
            .filter_map(|(&owner, &offset)| {
                (state.presented.get(&owner).copied().unwrap_or_default() != offset).then_some(
                    ScrollOffset {
                        owner,
                        offset: offset.point,
                        generation: offset.generation,
                    },
                )
            })
            .collect();
        state.echo.extend(changes);
        state.presented = next;
    }

    pub(crate) fn take_echo(&self) -> Vec<ScrollOffset> {
        std::mem::take(&mut self.0.lock().unwrap().echo)
    }
}

fn clamped(node: &ScrollNode, offset: Point) -> Point {
    let [[min_x, max_x], [min_y, max_y]] = node.offset_limits();
    Point {
        x: if node.axes[0] {
            offset.x.clamp(min_x, max_x)
        } else {
            0.0
        },
        y: if node.axes[1] {
            offset.y.clamp(min_y, max_y)
        } else {
            0.0
        },
    }
}

fn wheel_node(
    node: &ScrollNode,
    offsets: &mut BTreeMap<NodeId, Offset>,
    remaining: &mut [f64; 2],
    mode: DeltaMode,
) -> bool {
    let saved = offsets
        .get(&node.owner)
        .copied()
        .filter(|offset| offset.generation >= node.generation)
        .unwrap_or_else(|| Offset::from(node));
    let before = clamped(node, saved.point);
    let mut after = [before.x, before.y];
    let limit = node.offset_limits();
    let units = match mode {
        DeltaMode::Pixel => [1.0; 2],
        DeltaMode::Line => [40.0; 2],
        DeltaMode::Page => [
            f64::from(node.scrollport.size.width).max(1.0),
            f64::from(node.scrollport.size.height).max(1.0),
        ],
    };
    for axis in 0..2 {
        if !node.axes[axis] || remaining[axis] == 0.0 {
            continue;
        }
        if !node.wheel_axes[axis] {
            if node.overscroll[axis] != OverscrollBehavior::Auto {
                remaining[axis] = 0.0;
            }
            continue;
        }
        let target = f64::from(after[axis]) + remaining[axis] * units[axis];
        let [min, max] = limit[axis].map(f64::from);
        let position = target.clamp(min, max);
        after[axis] = position as f32;
        remaining[axis] = if node.overscroll[axis] == OverscrollBehavior::Auto {
            (target - position) / units[axis]
        } else {
            0.0
        };
    }
    let after = Point {
        x: after[0],
        y: after[1],
    };
    if before == after || saved.generation == u64::MAX {
        return false;
    }
    offsets.insert(
        node.owner,
        Offset {
            point: after,
            generation: saved.generation + 1,
        },
    );
    true
}

#[cfg(test)]
mod tests {
    use squark_paint::{ScrollId, TransformId};
    use squark_types::Rect;

    use super::*;

    fn scroll_node_for_test(owner: u32, extent: f32) -> ScrollNode {
        ScrollNode {
            parent: ScrollId(0),
            owner: NodeId::from_raw(owner),
            space: TransformId(0),
            scrollport: Rect::new(0.0, 0.0, 40.0, 20.0),
            overflow: Rect::new(0.0, 0.0, 40.0, extent),
            initial_offset: Point::default(),
            generation: 0,
            axes: [false, true],
            wheel_axes: [false, true],
            overscroll: [OverscrollBehavior::Auto; 2],
        }
    }

    #[test]
    fn wheel_clamps_and_chains_the_unused_delta() {
        let inner = scroll_node_for_test(2, 25.0);
        let outer = scroll_node_for_test(3, 60.0);
        let mut offsets = BTreeMap::new();
        let mut delta = [0.0, 20.0];
        assert!(wheel_node(
            &inner,
            &mut offsets,
            &mut delta,
            DeltaMode::Pixel
        ));
        assert_eq!(offsets[&inner.owner].point.y, 5.0);
        assert_eq!(delta, [0.0, 15.0]);
        assert!(wheel_node(
            &outer,
            &mut offsets,
            &mut delta,
            DeltaMode::Pixel
        ));
        assert_eq!(offsets[&outer.owner].point.y, 15.0);
        assert_eq!(delta, [0.0, 0.0]);
    }

    #[test]
    fn contain_stops_chaining_and_hidden_overflow_does_not_wheel_scroll() {
        let mut node = scroll_node_for_test(2, 25.0);
        node.overscroll[1] = OverscrollBehavior::Contain;
        let mut offsets = BTreeMap::new();
        let mut delta = [0.0, 20.0];
        assert!(wheel_node(
            &node,
            &mut offsets,
            &mut delta,
            DeltaMode::Pixel
        ));
        assert_eq!(delta, [0.0, 0.0]);
        assert_eq!(offsets[&node.owner].point.y, 5.0);
        node.wheel_axes[1] = false;
        let mut delta = [0.0, -10.0];
        assert!(!wheel_node(
            &node,
            &mut offsets,
            &mut delta,
            DeltaMode::Pixel
        ));
        assert_eq!(delta, [0.0, 0.0]);
        node.overscroll[1] = OverscrollBehavior::Auto;
        let mut delta = [0.0, -10.0];
        assert!(!wheel_node(
            &node,
            &mut offsets,
            &mut delta,
            DeltaMode::Pixel
        ));
        assert_eq!(delta, [0.0, -10.0]);
        assert_eq!(offsets[&node.owner].point.y, 5.0);
    }

    #[test]
    fn wheel_and_document_scrolls_share_one_generation_per_owner() {
        let scroll = ScrollOffsets::default();
        let mut nodes = [
            scroll_node_for_test(0, 20.0),
            scroll_node_for_test(1, 50.0),
            scroll_node_for_test(2, 60.0),
        ];
        let owner = nodes[1].owner;
        nodes[1].initial_offset.y = 5.0;
        nodes[1].generation = 7;
        assert!(!scroll.latch(&mut nodes));
        scroll.presented(&nodes);
        assert_eq!(
            scroll.take_echo(),
            [ScrollOffset {
                owner,
                offset: nodes[1].initial_offset,
                generation: 7,
            }]
        );

        let mut delta = [0.0, 9.0];
        {
            let mut state = scroll.0.lock().unwrap();
            assert!(wheel_node(
                &nodes[1],
                &mut state.current,
                &mut delta,
                DeltaMode::Pixel
            ));
            assert_eq!(state.current[&owner].generation, 8);
            assert!(!state.current.contains_key(&nodes[2].owner));
        }
        assert!(scroll.latch(&mut nodes));
        assert_eq!(nodes[1].initial_offset.y, 14.0);
        scroll.presented(&nodes);
        assert_eq!(scroll.take_echo()[0].generation, 8);

        nodes[1].initial_offset.y = 22.0;
        nodes[1].generation = 9;
        assert!(!scroll.latch(&mut nodes));
        assert!(scroll.take_echo().is_empty());
        scroll.presented(&nodes);
        assert_eq!(
            scroll.take_echo(),
            [ScrollOffset {
                owner,
                offset: nodes[1].initial_offset,
                generation: 9,
            }]
        );
        scroll.presented(&nodes);
        assert!(scroll.take_echo().is_empty());
    }

    #[test]
    fn conflicting_document_offset_keeps_user_scroll_with_a_new_generation() {
        let scroll = ScrollOffsets::default();
        let mut nodes = [scroll_node_for_test(0, 20.0), scroll_node_for_test(1, 50.0)];
        let node = &mut nodes[1];
        assert!(wheel_node(
            node,
            &mut scroll.0.lock().unwrap().current,
            &mut [0.0, 9.0],
            DeltaMode::Pixel
        ));
        node.initial_offset.y = 12.0;
        node.generation = 1;

        assert!(scroll.latch(&mut nodes));
        scroll.presented(&nodes);
        assert_eq!(
            scroll.take_echo(),
            [ScrollOffset {
                owner: nodes[1].owner,
                offset: Point { x: 0.0, y: 9.0 },
                generation: 2,
            }]
        );
    }
}
