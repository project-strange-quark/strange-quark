// SPDX-License-Identifier: MIT

use std::collections::BTreeMap;

use squark_types::{
    NodeId, Point, ScrollOffset,
    fragment::{FragmentId, FragmentTree, ScrollFrame},
};

#[derive(Clone, Copy, Debug, Default)]
struct Position {
    offset: Point,
    generation: u64,
}

#[derive(Debug, Default)]
pub(crate) struct ScrollState {
    positions: BTreeMap<NodeId, Position>,
    fragments: BTreeMap<NodeId, FragmentId>,
}

impl ScrollState {
    pub fn reindex(&mut self, tree: &FragmentTree) {
        self.fragments = tree
            .nodes
            .values()
            .filter_map(|fragment| fragment.scroll.map(|scroll| (scroll.owner, fragment.id)))
            .collect();
    }

    pub fn forget(&mut self, owner: NodeId) {
        self.positions.remove(&owner);
    }

    pub fn apply_compositor(&mut self, tree: &mut FragmentTree, offsets: &[ScrollOffset]) {
        for scroll in offsets {
            let ScrollOffset {
                owner,
                offset,
                generation,
            } = *scroll;
            if self.fragments.contains_key(&owner)
                && offset.x.is_finite()
                && offset.y.is_finite()
                && scroll.is_newer_than(
                    self.positions
                        .get(&owner)
                        .map(|position| position.generation),
                )
            {
                self.positions
                    .insert(owner, Position { offset, generation });
                self.publish(tree, owner);
            }
        }
    }

    /// A changed scrollport can constrain an existing offset. The clamp is
    /// a document edit and must also reach a compositor holding old bounds.
    pub fn restore_offsets(&mut self, tree: &mut FragmentTree) {
        for &owner in self.fragments.keys() {
            publish(tree, &self.fragments, &mut self.positions, owner);
        }
    }

    pub fn scroll_to(&mut self, tree: &mut FragmentTree, owner: NodeId, request: Point) {
        let Some(frame) = frame_mut(tree, &self.fragments, owner) else {
            return;
        };
        let offset = clamp(*frame, request);
        let position = self.positions.entry(owner).or_default();
        if position.offset != offset {
            position.offset = offset;
            position.generation = position.generation.saturating_add(1);
        }
        frame.offset = position.offset;
        frame.generation = position.generation;
    }

    pub(crate) fn publish(&mut self, tree: &mut FragmentTree, owner: NodeId) {
        publish(tree, &self.fragments, &mut self.positions, owner);
    }
}

fn publish(
    tree: &mut FragmentTree,
    fragments: &BTreeMap<NodeId, FragmentId>,
    positions: &mut BTreeMap<NodeId, Position>,
    owner: NodeId,
) {
    let Some(frame) = frame_mut(tree, fragments, owner) else {
        return;
    };
    if let Some(position) = positions.get_mut(&owner) {
        let offset = clamp(*frame, position.offset);
        if position.offset != offset {
            position.offset = offset;
            position.generation = position.generation.saturating_add(1);
        }
        frame.offset = offset;
        frame.generation = position.generation;
    }
}

fn frame_mut<'a>(
    tree: &'a mut FragmentTree,
    fragments: &BTreeMap<NodeId, FragmentId>,
    owner: NodeId,
) -> Option<&'a mut ScrollFrame> {
    tree.nodes.get_mut(fragments.get(&owner)?)?.scroll.as_mut()
}

pub(crate) fn clamp(frame: ScrollFrame, point: Point) -> Point {
    let axis = |value: f32, start: f32, end: f32, allowed| {
        if allowed {
            value.clamp(start.min(0.0), end.max(0.0))
        } else {
            0.0
        }
    };
    let view = frame.scrollport;
    let content = frame.content;
    Point {
        x: axis(
            point.x,
            content.origin.x - view.origin.x,
            content.max_x() - view.max_x(),
            frame.scroll_x,
        ),
        y: axis(
            point.y,
            content.origin.y - view.origin.y,
            content.max_y() - view.max_y(),
            frame.scroll_y,
        ),
    }
}
