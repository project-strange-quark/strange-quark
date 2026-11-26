// SPDX-License-Identifier: MIT

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use squark_compositor::{Compositor, FrameMailbox, HitSnapshots, Painter, ScrollOffsets};
use squark_paint::{Damage, hit_test};
use squark_types::{
    NodeId, Point, Rect, ScrollOffset, Size,
    fragment::{
        AnimatedValue, AnimationDirection, AnimationFill, AnimationKeyframe, ArtValues, AtomId,
        CompositorAnimation, Easing, Filter, Fragment, FragmentId, FragmentKind, FragmentTree,
        LengthPercentage, MirrorRead, NodeKind, NodeRead, PaintColor, PaintValues, Position2D,
        ScrollFrame, Transform,
    },
    host::DeltaMode,
    render::{
        FrameId, FramePainter, FrameTick, ResourceSeq, ResourceUpdate, Viewport, resource_channel,
    },
};

const ROOT: FragmentId = FragmentId(1);
const BOX: FragmentId = FragmentId(2);
const TARGET: NodeId = NodeId::from_raw(0x2_000002);

struct Mirror;

impl MirrorRead for Mirror {
    fn document(&self) -> NodeId {
        NodeId::NONE
    }
    fn node(&self, id: NodeId) -> Option<NodeRead<'_>> {
        (id == TARGET).then(|| NodeRead {
            kind: NodeKind::Element,
            links: Default::default(),
            name: None,
            attrs: Vec::new(),
            text: None,
            state: Default::default(),
            own: Default::default(),
            descendants: Default::default(),
            listeners: Default::default(),
        })
    }
    fn atom(&self, _: AtomId) -> Option<&[u16]> {
        None
    }
}

fn tree(x: f32) -> FragmentTree {
    let viewport = Fragment {
        id: ROOT,
        owner: NodeId::NONE,
        kind: FragmentKind::Viewport,
        parent: None,
        containing_block: None,
        children: Box::new([BOX]),
        border_box: Rect::new(0.0, 0.0, 200.0, 200.0),
        padding: Default::default(),
        border: Default::default(),
        first_baseline: None,
        last_baseline: None,
        scrollable_overflow: Rect::default(),
        scroll: None,
        sticky: None,
        content: squark_types::fragment::FragmentContent::None,
        style: Arc::new(PaintValues::default()),
    };
    let child = Fragment {
        id: BOX,
        owner: TARGET,
        kind: FragmentKind::Box,
        parent: Some(ROOT),
        containing_block: Some(ROOT),
        children: Box::new([]),
        border_box: Rect::new(x, 10.0, 20.0, 20.0),
        padding: Default::default(),
        border: Default::default(),
        first_baseline: None,
        last_baseline: None,
        scrollable_overflow: Rect::default(),
        scroll: None,
        sticky: None,
        content: squark_types::fragment::FragmentContent::None,
        style: Arc::new(PaintValues {
            art: ArtValues {
                background_color: PaintColor::srgb(1.0, 0.0, 0.0, 1.0),
                ..Default::default()
            },
            ..PaintValues::default()
        }),
    };
    FragmentTree {
        root: ROOT,
        ime_area: None,
        nodes: [(ROOT, viewport), (BOX, child)].into(),
    }
}

fn tick(sequence: u64, epoch: u64) -> FrameTick {
    let now = Instant::now();
    FrameTick {
        sequence,
        vsync: now,
        present_at: now,
        commit_deadline: now,
        viewport: Viewport {
            css: Size {
                width: 200.0,
                height: 200.0,
            },
            device: [400, 400],
            scale: 2.0,
            epoch,
        },
    }
}

#[test]
fn newest_commit_wins_and_damage_recovers_when_the_base_was_skipped() {
    let mailbox = Arc::new(FrameMailbox::default());
    let snapshots = Arc::new(HitSnapshots::default());
    let (sender, _resources) = resource_channel();
    let mut painter = Painter::new(mailbox.clone(), sender);
    let mut compositor = Compositor::new(mailbox, snapshots.clone());

    let tick = tick(10, 1);
    assert_eq!(
        painter.commit(&tick, &tree(10.0), &Mirror).unwrap(),
        FrameId(1)
    );
    assert_eq!(
        compositor
            .adopt_latest(tick.viewport, ResourceSeq(0))
            .unwrap()
            .damage,
        &Damage::Full
    );
    assert!(snapshots.latest().is_none());
    assert!(compositor.mark_presented());
    let first_snapshot = snapshots.latest().unwrap();
    assert_eq!(first_snapshot.frame, FrameId(1));
    assert!(
        compositor
            .adopt_latest(tick.viewport, ResourceSeq(0))
            .is_none()
    );
    assert_eq!(compositor.current().unwrap().id, FrameId(1));

    painter.commit(&tick, &tree(30.0), &Mirror).unwrap();
    painter.commit(&tick, &tree(60.0), &Mirror).unwrap();
    let adoption = compositor
        .adopt_latest(tick.viewport, ResourceSeq(0))
        .unwrap();
    assert_eq!(adoption.frame.id, FrameId(3));
    assert_eq!(adoption.damage, &Damage::Full);
    assert!(matches!(
        adoption.frame.frame.damage,
        Damage::Rects {
            base: FrameId(2),
            ..
        }
    ));

    painter.commit(&tick, &tree(80.0), &Mirror).unwrap();
    assert_eq!(
        compositor
            .adopt_latest(tick.viewport, ResourceSeq(0))
            .unwrap()
            .damage,
        &Damage::Rects {
            base: FrameId(3),
            rects: Box::new([
                Rect::new(59.5, 9.5, 21.0, 21.0),
                Rect::new(79.5, 9.5, 21.0, 21.0)
            ]),
        },
    );
    assert_eq!(snapshots.latest().unwrap().frame, FrameId(1));
    assert!(compositor.mark_presented());
    assert!(!compositor.mark_presented());
    let latest_snapshot = snapshots.latest().unwrap();
    assert_eq!(latest_snapshot.frame, FrameId(4));
    assert_eq!(
        hit_test(&first_snapshot, Point { x: 15.0, y: 15.0 })
            .unwrap()
            .target,
        TARGET
    );
    assert_eq!(
        hit_test(&latest_snapshot, Point { x: 85.0, y: 15.0 })
            .unwrap()
            .target,
        TARGET
    );
    assert!(hit_test(&latest_snapshot, Point { x: 15.0, y: 15.0 }).is_none());
}

#[test]
fn only_a_compatible_viewport_and_applied_resources_can_publish_a_hit_snapshot() {
    let mailbox = Arc::new(FrameMailbox::default());
    let snapshots = Arc::new(HitSnapshots::default());
    let (sender, resources) = resource_channel();
    let mut painter = Painter::new(mailbox.clone(), sender.clone());
    let mut compositor = Compositor::new(mailbox, snapshots.clone());

    let original = tick(1, 1);
    let resized = tick(2, 2);
    sender
        .send(ResourceUpdate::ResizeCanvas {
            id: TARGET,
            size: [200, 200],
        })
        .unwrap();
    painter.commit(&original, &tree(10.0), &Mirror).unwrap();
    sender
        .send(ResourceUpdate::ResizeCanvas {
            id: TARGET,
            size: [300, 300],
        })
        .unwrap();
    painter.commit(&resized, &tree(30.0), &Mirror).unwrap();

    assert!(
        compositor
            .adopt_latest(original.viewport, ResourceSeq(2))
            .is_none()
    );
    let applied = resources.try_recv().unwrap().seq;
    assert_eq!(applied, ResourceSeq(1));
    assert!(compositor.adopt_latest(resized.viewport, applied).is_none());
    assert!(snapshots.latest().is_none());
    let applied = resources.try_recv().unwrap().seq;
    assert_eq!(applied, ResourceSeq(2));
    let adoption = compositor.adopt_latest(resized.viewport, applied).unwrap();
    assert_eq!(adoption.frame.id, FrameId(2));
    assert_eq!(adoption.damage, &Damage::Full);
    assert!(compositor.mark_presented());
    assert_eq!(snapshots.latest().unwrap().viewport, resized.viewport);
    assert_eq!(
        painter.hit_test(&tree(30.0), &Mirror, Point { x: 35.0, y: 15.0 }),
        Some(TARGET)
    );
}

#[test]
fn a_rejected_commit_leaves_the_previous_commit_available() {
    let mailbox = Arc::new(FrameMailbox::default());
    let (sender, _resources) = resource_channel();
    let mut painter = Painter::new(mailbox.clone(), sender);
    let tick = tick(1, 1);
    painter.commit(&tick, &tree(10.0), &Mirror).unwrap();
    let mut invalid = tree(50.0);
    invalid.nodes.remove(&BOX);
    assert!(painter.commit(&tick, &invalid, &Mirror).is_err());
    assert_eq!(mailbox.take_latest().unwrap().id, FrameId(1));
    assert_eq!(
        painter.commit(&tick, &tree(50.0), &Mirror).unwrap(),
        FrameId(2)
    );
}

#[test]
fn late_commit_waits_for_the_following_latch() {
    let mailbox = Arc::new(FrameMailbox::default());
    let (sender, _resources) = resource_channel();
    let mut painter = Painter::new(mailbox.clone(), sender);
    let mut compositor = Compositor::new(mailbox, Arc::default());
    let tick = tick(1, 1);
    painter.commit(&tick, &tree(10.0), &Mirror).unwrap();
    compositor
        .adopt_latest(tick.viewport, ResourceSeq(0))
        .unwrap();

    let deadline = Instant::now() - Duration::from_millis(1);
    painter.commit(&tick, &tree(30.0), &Mirror).unwrap();
    assert!(
        compositor
            .adopt_before(tick.viewport, ResourceSeq(0), Some(deadline))
            .is_none()
    );
    assert_eq!(compositor.current().unwrap().id, FrameId(1));
    let next = compositor
        .adopt_before(tick.viewport, ResourceSeq(0), Some(Instant::now()))
        .unwrap();
    assert_eq!(next.frame.id, FrameId(2));
}

#[test]
fn retained_animations_publish_the_sampled_transform_and_restore_unfilled_values() {
    let mailbox = Arc::new(FrameMailbox::default());
    let hits = Arc::new(HitSnapshots::default());
    let (sender, _resources) = resource_channel();
    let mut painter = Painter::new(mailbox.clone(), sender).with_hit_snapshots(Arc::clone(&hits));
    let mut compositor = Compositor::new(mailbox, Arc::clone(&hits));
    let tick = tick(1, 1);
    let track = |id, from, to| CompositorAnimation {
        id,
        start: tick.vsync,
        delay_seconds: 0.0,
        duration: Duration::from_secs(2),
        iterations: 1.0,
        direction: AnimationDirection::Normal,
        fill: AnimationFill::None,
        keyframes: Box::new([
            AnimationKeyframe {
                offset: 0.0,
                value: from,
                easing: Easing::Linear,
            },
            AnimationKeyframe {
                offset: 1.0,
                value: to,
                easing: Easing::Linear,
            },
        ]),
    };
    let mut tree = tree(10.0);
    Arc::make_mut(&mut tree.nodes.get_mut(&BOX).unwrap().style)
        .properties
        .animations = Box::new([
        track(
            1,
            AnimatedValue::Transform(Box::new([])),
            AnimatedValue::Transform(Box::new([Transform::Translate {
                xy: Position2D {
                    x: LengthPercentage {
                        px: 60.0,
                        fraction: 0.0,
                    },
                    y: Default::default(),
                },
                z: 0.0,
            }])),
        ),
        track(2, AnimatedValue::Opacity(1.0), AnimatedValue::Opacity(0.0)),
        track(
            3,
            AnimatedValue::Filter(Box::new([])),
            AnimatedValue::Filter(Box::new([Filter::Blur(6.0)])),
        ),
    ]);
    let id = painter.commit(&tick, &tree, &Mirror).unwrap();
    compositor
        .adopt_latest(tick.viewport, ResourceSeq(0))
        .unwrap();
    assert!(compositor.latch_animations(tick.vsync).running);
    assert!(compositor.mark_presented());
    let old = hits.latest().unwrap();
    let point = Point { x: 45.0, y: 15.0 };
    assert!(hit_test(&old, point).is_none());

    let sample = compositor.latch_animations(tick.vsync + Duration::from_secs(1));
    assert!(sample.running && sample.changed);
    let frame = compositor.current().unwrap();
    assert_eq!(frame.id, id);
    let effect = &frame.frame.property_trees.effects[1];
    assert_eq!(effect.opacity, 0.5);
    assert_eq!(&*effect.filters, &[squark_paint::Filter::Blur(3.0)]);
    assert!(hit_test(&hits.latest().unwrap(), point).is_none());
    assert!(compositor.mark_presented());
    assert_eq!(
        hit_test(&hits.latest().unwrap(), point).unwrap().target,
        TARGET
    );
    assert_eq!(painter.hit_test(&tree, &Mirror, point), Some(TARGET));

    let end = compositor.latch_animations(tick.vsync + Duration::from_secs(2));
    assert!(!end.running && end.changed);
    assert!(compositor.mark_presented());
    let effect = &compositor.current().unwrap().frame.property_trees.effects[1];
    assert_eq!(effect.opacity, 1.0);
    assert!(effect.filters.is_empty());
    assert!(hit_test(&hits.latest().unwrap(), point).is_none());
}

#[test]
fn wheel_presents_new_geometry_without_a_document_commit() {
    let mailbox = Arc::new(FrameMailbox::default());
    let snapshots = Arc::new(HitSnapshots::default());
    let scroll = Arc::new(ScrollOffsets::default());
    let (sender, _resources) = resource_channel();
    let mut painter =
        Painter::new(mailbox.clone(), sender).with_scroll_offsets(Arc::clone(&scroll));
    let mut compositor =
        Compositor::new(mailbox, Arc::clone(&snapshots)).with_scroll_offsets(Arc::clone(&scroll));
    let tick = tick(1, 1);
    let mut tree = tree(10.0);
    let root = tree.nodes.get_mut(&ROOT).unwrap();
    root.scroll = Some(ScrollFrame {
        owner: root.owner,
        offset: Point::default(),
        generation: 0,
        scrollport: root.border_box,
        content: Rect::new(0.0, 0.0, 200.0, 400.0),
        gutter: Default::default(),
        scroll_x: false,
        scroll_y: true,
        clip_x: true,
        clip_y: true,
    });
    let frame = painter.commit(&tick, &tree, &Mirror).unwrap();
    compositor
        .adopt_latest(tick.viewport, ResourceSeq(0))
        .unwrap();
    assert!(compositor.mark_presented());
    let visible = snapshots.latest().unwrap();
    let pointer = Point { x: 15.0, y: 5.0 };
    assert!(hit_test(&visible, pointer).is_none());

    assert!(scroll.wheel(&visible, &[NodeId::NONE], [0.0, 12.0], DeltaMode::Pixel));
    assert!(
        compositor
            .adopt_latest(tick.viewport, ResourceSeq(0))
            .is_none()
    );
    assert!(compositor.latch_scroll());
    assert!(compositor.take_scroll_echo().is_empty());
    assert!(hit_test(&snapshots.latest().unwrap(), pointer).is_none());
    assert!(compositor.mark_presented());

    let latched = snapshots.latest().unwrap();
    assert_eq!(latched.frame, frame);
    assert_eq!(hit_test(&latched, pointer).unwrap().target, TARGET);
    assert_eq!(painter.hit_test(&tree, &Mirror, pointer), Some(TARGET));
    assert_eq!(
        compositor.take_scroll_echo(),
        [ScrollOffset {
            owner: NodeId::NONE,
            offset: Point { x: 0.0, y: 12.0 },
            generation: 1,
        }]
    );
    assert!(!compositor.latch_scroll());
    assert!(!compositor.mark_presented());
}
