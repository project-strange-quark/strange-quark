// SPDX-License-Identifier: MIT
//! Document-to-render handoff and static frame adoption.

mod scroll;

pub use scroll::ScrollOffsets;

use std::{sync::Arc, time::Instant};

use arc_swap::ArcSwapOption;
use squark_paint::{
    AnimationSample, CommittedFrame, Damage, FramePublication, HitSnapshot, build_frame,
    build_hit_test, frame_damage, hit_test_tree,
};
use squark_types::{
    NodeId, Point, ScrollOffset,
    fragment::{FragmentTree, MirrorRead},
    render::{FrameId, FramePainter, FrameTick, PaintError, ResourceSender, ResourceSeq, Viewport},
};

/// A single slot. Publishing never queues behind an older pending commit.
#[derive(Debug, Default)]
pub struct FrameMailbox(ArcSwapOption<CommittedFrame>);

impl FrameMailbox {
    pub fn publish(&self, frame: Arc<CommittedFrame>) {
        self.0.store(Some(frame));
    }

    pub fn take_latest(&self) -> Option<Arc<CommittedFrame>> {
        self.0.swap(None)
    }
}

/// Document-side painter. Its previous commit is the base of damage even
/// when the render thread skips that commit.
#[derive(Debug)]
pub struct Painter {
    mailbox: Arc<FrameMailbox>,
    resources: ResourceSender,
    previous: Option<Arc<CommittedFrame>>,
    scroll: Arc<ScrollOffsets>,
    hit_snapshots: Arc<HitSnapshots>,
}

impl Painter {
    pub fn new(mailbox: Arc<FrameMailbox>, resources: ResourceSender) -> Self {
        Self {
            mailbox,
            resources,
            previous: None,
            scroll: Arc::default(),
            hit_snapshots: Arc::default(),
        }
    }

    pub fn with_scroll_offsets(mut self, scroll: Arc<ScrollOffsets>) -> Self {
        self.scroll = scroll;
        self
    }

    pub fn with_hit_snapshots(mut self, snapshots: Arc<HitSnapshots>) -> Self {
        self.hit_snapshots = snapshots;
        self
    }
}

impl FramePainter for Painter {
    fn commit(
        &mut self,
        tick: &FrameTick,
        fragments: &FragmentTree,
        mirror: &dyn MirrorRead,
    ) -> Result<FrameId, PaintError> {
        #[cfg(debug_assertions)]
        puffin::profile_scope!("paint and commit");
        let paint_started = Instant::now();
        let previous_id = self
            .previous
            .as_ref()
            .map_or(FrameId(0), |previous| previous.id);
        let id = FrameId(
            previous_id
                .0
                .checked_add(1)
                .ok_or(PaintError::FrameIdExhausted)?,
        );
        let mut frame = build_frame(
            fragments,
            mirror,
            self.resources.watermark(),
            tick.viewport.scale,
        )?;
        let viewport = tick.viewport;
        if let Some(previous) = &self.previous {
            frame.damage = frame_damage(previous, viewport, &frame);
        }
        let frame = Arc::new(CommittedFrame {
            id,
            viewport,
            frame,
            publication: Some(FramePublication {
                sequence: tick.sequence,
                paint_started,
                committed: Instant::now(),
            }),
        });
        self.mailbox.publish(Arc::clone(&frame));
        self.previous = Some(frame);
        Ok(id)
    }

    fn hit_test(
        &self,
        fragments: &FragmentTree,
        mirror: &dyn MirrorRead,
        point: Point,
    ) -> Option<NodeId> {
        let root = fragments.nodes.get(&fragments.root)?;
        if !root.border_box.contains(point) {
            return None;
        }
        let dpr = self
            .previous
            .as_ref()
            .map_or(1.0, |frame| frame.viewport.scale);
        let (hit, mut properties) = build_hit_test(fragments, mirror, dpr).ok()?;
        self.scroll.apply_presented(&mut properties);
        if let Some(at) = self
            .hit_snapshots
            .latest()
            .and_then(|snapshot| snapshot.sampled_at)
        {
            properties.sample_animations(at);
        }
        hit_test_tree(&hit, &properties, point)
    }
}

#[derive(Debug, Default)]
pub struct HitSnapshots(ArcSwapOption<HitSnapshot>);

impl HitSnapshots {
    pub fn latest(&self) -> Option<Arc<HitSnapshot>> {
        self.0.load_full()
    }
}

/// Render-thread state. The candidate remains pending until its viewport
/// and resource prefix are available. An adopted commit remains current
/// when the document thread misses a latch.
#[derive(Debug)]
pub struct Compositor {
    mailbox: Arc<FrameMailbox>,
    hit_snapshots: Arc<HitSnapshots>,
    scroll: Arc<ScrollOffsets>,
    pending: Option<Arc<CommittedFrame>>,
    current: Option<CommittedFrame>,
    presentation_dirty: bool,
    sampled_at: Option<Instant>,
}

#[derive(Debug)]
pub struct Adoption<'a> {
    pub frame: &'a CommittedFrame,
    pub damage: &'a Damage,
}

impl Compositor {
    pub fn new(mailbox: Arc<FrameMailbox>, hit_snapshots: Arc<HitSnapshots>) -> Self {
        Self {
            mailbox,
            hit_snapshots,
            scroll: Arc::default(),
            pending: None,
            current: None,
            presentation_dirty: false,
            sampled_at: None,
        }
    }

    pub fn with_scroll_offsets(mut self, scroll: Arc<ScrollOffsets>) -> Self {
        self.scroll = scroll;
        self
    }

    pub fn take_scroll_echo(&self) -> Vec<ScrollOffset> {
        self.scroll.take_echo()
    }

    pub fn current(&self) -> Option<&CommittedFrame> {
        self.current.as_ref()
    }

    /// Apply pending scroll offsets to the retained frame at the render latch.
    pub fn latch_scroll(&mut self) -> bool {
        let changed = self
            .current
            .as_mut()
            .is_some_and(|frame| self.scroll.latch(&mut frame.frame.property_trees.scrolls));
        self.presentation_dirty |= changed;
        changed
    }

    pub fn latch_animations(&mut self, now: Instant) -> AnimationSample {
        self.sampled_at = Some(now);
        let sample = self
            .current
            .as_mut()
            .map_or_default(|frame| frame.frame.property_trees.sample_animations(now));
        self.presentation_dirty |= sample.changed;
        sample
    }

    /// Call at the render latch after applying the available resource prefix.
    pub fn adopt_latest(
        &mut self,
        viewport: Viewport,
        applied: ResourceSeq,
    ) -> Option<Adoption<'_>> {
        self.adopt_before(viewport, applied, None)
    }

    pub fn adopt_before(
        &mut self,
        viewport: Viewport,
        applied: ResourceSeq,
        deadline: Option<Instant>,
    ) -> Option<Adoption<'_>> {
        if let Some(latest) = self.mailbox.take_latest() {
            self.pending = Some(latest);
        }
        let pending = self.pending.as_ref()?;
        if pending.viewport.epoch < viewport.epoch {
            self.pending = None;
            return None;
        }
        if pending.viewport != viewport || pending.frame.resource_updates > applied {
            return None;
        }
        if let (Some(commit), Some(deadline)) = (pending.publication, deadline)
            && commit.committed > deadline
        {
            return None;
        }
        let mut frame = (*self.pending.take().expect("candidate is pending")).clone();
        self.scroll.latch(&mut frame.frame.property_trees.scrolls);
        self.presentation_dirty = true;
        let previous = self.current.replace(frame);
        let frame = self.current.as_ref().expect("candidate was adopted");
        let damage = match (&frame.frame.damage, previous.as_ref()) {
            (damage @ Damage::Rects { base, .. }, Some(previous))
                if *base == previous.id
                    && frame.viewport == previous.viewport
                    && frame.frame.property_trees.scrolls
                        == previous.frame.property_trees.scrolls =>
            {
                damage
            }
            _ => &Damage::Full,
        };
        Some(Adoption { frame, damage })
    }

    /// Publish input geometry for a frame after the render surface accepts it.
    /// A failed or skipped presentation leaves the previous geometry visible.
    pub fn mark_presented(&mut self) -> bool {
        let Some(frame) = &self.current else {
            return false;
        };
        if !std::mem::take(&mut self.presentation_dirty) {
            return false;
        }
        self.scroll.presented(&frame.frame.property_trees.scrolls);
        let mut snapshot = HitSnapshot::new(frame);
        snapshot.sampled_at = self.sampled_at;
        self.hit_snapshots.0.store(Some(Arc::new(snapshot)));
        true
    }
}
