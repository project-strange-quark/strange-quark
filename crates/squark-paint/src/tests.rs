// SPDX-License-Identifier: MIT

use std::{collections::BTreeMap, sync::Arc};

use crate::{ClipShape, Primitive, RoundedRect, build_resolved_frame, hit_test_tree, input::*};
use kurbo::Shape;
use squark_types::{
    NodeId, Point, Rect, Size,
    host::ListenerFlags,
    render::{ImageKey, PaintError, ResourceSeq},
};

fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

fn node(id: u64) -> NodeId {
    NodeId::new(u32::try_from(id).unwrap(), 0).unwrap()
}

fn fragment(id: u64, parent: Option<u64>, children: &[u64], style: PaintValues) -> Fragment {
    Fragment {
        id: FragmentId(id),
        owner: node(id),
        kind: if parent.is_none() {
            FragmentKind::Viewport
        } else {
            FragmentKind::Box
        },
        parent: parent.map(FragmentId),
        containing_block: parent.map(FragmentId),
        children: children.iter().copied().map(FragmentId).collect(),
        border_box: Rect::new(0.0, 0.0, 100.0, 100.0),
        padding: [0.0; 4],
        border: [0.0; 4],
        scroll: None,
        sticky: None,
        content: FragmentContent::None,
        style: Arc::new(style),
    }
}

fn ink() -> PaintValues {
    PaintValues {
        background: crate::PaintColor::srgb([0.0, 0.0, 1.0, 1.0]),
        ..PaintValues::default()
    }
}

fn tree(fragments: impl IntoIterator<Item = Fragment>) -> FragmentTree {
    FragmentTree {
        root: FragmentId(1),
        nodes: fragments
            .into_iter()
            .map(|fragment| (fragment.id, fragment))
            .collect(),
    }
}

#[derive(Debug, Default)]
struct Mirror {
    parent: BTreeMap<NodeId, NodeId>,
    listeners: BTreeMap<NodeId, ListenerFlags>,
}

impl MirrorRead for Mirror {
    fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent.get(&id).copied()
    }

    fn is_element(&self, id: NodeId) -> bool {
        id.slot() > 1
    }

    fn listeners(&self, id: NodeId) -> ListenerFlags {
        self.listeners.get(&id).copied().unwrap_or_default()
    }
}

#[test]
fn stacking_contexts_sort_and_group_in_css_paint_order() {
    let tree = tree([
        fragment(1, None, &[2, 3, 4, 5, 6, 7, 8, 9, 10, 13], ink()),
        fragment(
            2,
            Some(1),
            &[],
            PaintValues {
                position: Position::Absolute,
                z_index: Some(3),
                ..ink()
            },
        ),
        fragment(
            3,
            Some(1),
            &[],
            PaintValues {
                position: Position::Absolute,
                z_index: Some(-1),
                ..ink()
            },
        ),
        fragment(
            4,
            Some(1),
            &[],
            PaintValues {
                display: Display::Inline,
                ..ink()
            },
        ),
        fragment(
            5,
            Some(1),
            &[],
            PaintValues {
                floating: true,
                ..ink()
            },
        ),
        fragment(6, Some(1), &[], ink()),
        fragment(
            7,
            Some(1),
            &[11],
            PaintValues {
                position: Position::Relative,
                ..ink()
            },
        ),
        fragment(
            8,
            Some(1),
            &[12],
            PaintValues {
                opacity: 0.5,
                ..ink()
            },
        ),
        fragment(
            9,
            Some(1),
            &[],
            PaintValues {
                position: Position::Absolute,
                z_index: Some(-2),
                ..ink()
            },
        ),
        fragment(
            10,
            Some(1),
            &[],
            PaintValues {
                position: Position::Absolute,
                z_index: Some(3),
                ..ink()
            },
        ),
        fragment(
            11,
            Some(7),
            &[],
            PaintValues {
                position: Position::Relative,
                z_index: Some(5),
                ..ink()
            },
        ),
        fragment(
            12,
            Some(8),
            &[],
            PaintValues {
                position: Position::Relative,
                z_index: Some(99),
                ..ink()
            },
        ),
        fragment(
            13,
            Some(1),
            &[],
            PaintValues {
                top_layer: Some(0),
                ..ink()
            },
        ),
    ]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(0)).unwrap();
    let order: Vec<_> = frame.display_list.iter().map(|item| item.id.0.0).collect();
    assert_eq!(order, [1, 9, 3, 6, 5, 4, 7, 8, 12, 2, 10, 11, 13]);
    let regions: Vec<_> = frame
        .hit_test
        .regions
        .iter()
        .map(|region| region.target.slot())
        .collect();
    assert_eq!(regions, [9, 3, 6, 5, 4, 7, 8, 12, 2, 10, 11, 13]);
    assert_eq!(
        frame.display_list[7].properties.effect,
        frame.display_list[8].properties.effect
    );
    assert_eq!(frame.property_trees.effects[1].opacity, 0.5);
}

#[test]
fn invisible_and_pointer_excluded_regions_pass_hits_through() {
    let tree = tree([
        fragment(1, None, &[2, 3, 4], ink()),
        fragment(2, Some(1), &[], PaintValues::default()),
        fragment(
            3,
            Some(1),
            &[],
            PaintValues {
                pointer_events: false,
                ..ink()
            },
        ),
        fragment(
            4,
            Some(1),
            &[],
            PaintValues {
                visible: false,
                ..ink()
            },
        ),
    ]);
    let mut mirror = Mirror::default();
    mirror.parent.insert(node(2), node(1));
    mirror
        .listeners
        .insert(node(1), ListenerFlags::NON_PASSIVE_WHEEL);
    mirror
        .listeners
        .insert(NodeId::NONE, ListenerFlags::NON_PASSIVE_TOUCHMOVE);
    let frame = build_resolved_frame(&tree, &mirror, ResourceSeq(4)).unwrap();
    assert_eq!(frame.resource_updates, ResourceSeq(4));
    assert_eq!(frame.display_list.len(), 2);
    assert_eq!(frame.hit_test.regions[0].listeners.bits(), 3);
    assert_eq!(
        hit_test_tree(&frame.hit_test, &frame.property_trees, point(5.0, 5.0)),
        Some(node(2))
    );
}

#[test]
fn property_trees_preserve_ancestor_clips_and_transform_spaces() {
    let mut scroller = fragment(
        2,
        Some(1),
        &[3, 4],
        PaintValues {
            transform: Some([
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [50.0, 0.0, 0.0, 1.0],
            ]),
            opacity: 0.6,
            radii: [Size {
                width: 10.0,
                height: 10.0,
            }; 4],
            ..ink()
        },
    );
    scroller.border_box = Rect::new(0.0, 0.0, 50.0, 50.0);
    scroller.scroll = Some(ScrollFrame {
        owner: node(2),
        scrollport: scroller.border_box,
        content: Rect::new(0.0, 0.0, 100.0, 100.0),
        scroll_x: true,
        scroll_y: true,
        clip_x: true,
        clip_y: true,
        wheel_axes: [true; 2],
        force_scrollbars: [false; 2],
        offset: point(2.0, 3.0),
        generation: 7,
        gutter: [0.0; 4],
    });
    let tree = tree([
        fragment(1, None, &[2], PaintValues::default()),
        scroller,
        fragment(3, Some(2), &[], ink()),
        fragment(
            4,
            Some(2),
            &[],
            PaintValues {
                transform: Some([[0.0; 4]; 4]),
                position: Position::Relative,
                z_index: Some(1),
                ..ink()
            },
        ),
    ]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(0)).unwrap();
    let props = &frame.property_trees;
    assert_eq!(
        (
            props.transforms.len(),
            props.clips.len(),
            props.effects.len(),
            props.scrolls.len()
        ),
        (4, 2, 2, 2)
    );
    let scroller = &frame.display_list[0];
    let child = &frame.display_list[1];
    assert_eq!(
        props.transforms[child.properties.transform.0 as usize].parent,
        scroller.properties.transform
    );
    assert_eq!(
        props.clips[child.properties.clip.0 as usize].space,
        scroller.properties.transform
    );
    assert_eq!(
        props.scrolls[child.properties.scroll.0 as usize].space,
        scroller.properties.transform
    );
    assert_eq!(child.properties.effect, scroller.properties.effect);
    assert_eq!(&*frame.hit_test.regions[1].scroll_chain, &[node(2)]);
    assert_eq!(props.scrolls[1].initial_offset, point(2.0, 3.0));
    assert_eq!(props.scrolls[1].generation, 7);
    let hit = |x, y| hit_test_tree(&frame.hit_test, props, point(x, y));
    assert_eq!(hit(70.0, 20.0), Some(node(3)));
    assert_eq!(hit(51.0, 1.0), None);
    assert_eq!(hit(49.0, 20.0), None);
    assert_eq!(hit(101.0, 20.0), None);
}

#[test]
fn path_clips_respect_even_odd_and_absolute_children_can_escape_overflow() {
    let mut path = kurbo::Rect::new(0.0, 0.0, 100.0, 100.0).to_path(0.01);
    path.extend(kurbo::Rect::new(20.0, 20.0, 80.0, 80.0).path_elements(0.01));
    let mut clipper = fragment(
        2,
        Some(1),
        &[3],
        PaintValues {
            clip: Some(ClipShape::Path(Arc::new(path), crate::FillRule::EvenOdd)),
            ..PaintValues::default()
        },
    );
    clipper.scroll = Some(ScrollFrame {
        owner: node(2),
        scrollport: Rect::new(0.0, 0.0, 10.0, 10.0),
        content: Rect::new(0.0, 0.0, 100.0, 100.0),
        scroll_x: false,
        scroll_y: false,
        clip_x: true,
        clip_y: true,
        wheel_axes: [true; 2],
        force_scrollbars: [false; 2],
        offset: Point::default(),
        generation: 0,
        gutter: [0.0; 4],
    });
    let mut abs = fragment(
        3,
        Some(2),
        &[],
        PaintValues {
            position: Position::Absolute,
            ..ink()
        },
    );
    abs.containing_block = Some(FragmentId(1));
    let tree = tree([
        fragment(1, None, &[2], PaintValues::default()),
        clipper,
        abs,
    ]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(0)).unwrap();
    let hit = |x, y| hit_test_tree(&frame.hit_test, &frame.property_trees, point(x, y));
    assert_eq!(hit(15.0, 15.0), Some(node(3)));
    assert_eq!(hit(40.0, 40.0), None);
}

#[test]
fn cover_uses_bitmap_source_geometry_and_clips_the_content_box() {
    let mut image = fragment(
        2,
        Some(1),
        &[],
        PaintValues {
            object_fit: ObjectFit::Cover,
            ..ink()
        },
    );
    image.content = FragmentContent::Image {
        key: ImageKey(5),
        intrinsic_size: Size {
            width: 200.0,
            height: 100.0,
        },
    };
    image.padding = [10.0; 4];
    let tree = tree([fragment(1, None, &[2], PaintValues::default()), image]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(1)).unwrap();
    let image = &frame.display_list[1];
    let Primitive::Image(quad) = image.primitive else {
        panic!("expected replaced image")
    };
    assert_eq!(quad.src, Rect::new(0.0, 0.0, 200.0, 100.0));
    assert_eq!(quad.dst, Rect::new(-30.0, 10.0, 160.0, 80.0));
    assert_eq!(image.bounds, Rect::new(10.0, 10.0, 80.0, 80.0));
    assert_eq!(
        frame.property_trees.clips[image.properties.clip.0 as usize].shape,
        Some(ClipShape::RoundedRect(RoundedRect::new(
            image.bounds,
            [Size::default(); 4]
        ))),
    );
}

#[test]
fn invalid_tree_links_are_rejected_before_any_frame_is_built() {
    let tree = tree([
        fragment(1, None, &[2], ink()),
        fragment(2, Some(1), &[1], ink()),
    ]);
    assert!(matches!(
        build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(0)),
        Err(PaintError::InvalidFragment(1)),
    ));
}

#[test]
fn outlines_and_text_decorations_keep_their_paint_order() {
    use crate::{BorderSide, BorderStyle, ColorSpace, PaintColor, Shadow};
    use squark_types::render::{FontInstanceId, GlyphKey};

    let color = PaintColor {
        space: ColorSpace::Srgb,
        rgba: [0.0, 0.0, 0.0, 1.0],
    };
    let border = BorderSide {
        width: 2.0,
        style: BorderStyle::Dashed,
        color,
    };
    let shadow = Shadow {
        offset: point(2.0, 2.0),
        blur: 4.0,
        spread: 0.0,
        color,
        inset: false,
    };
    let panel = fragment(
        2,
        Some(1),
        &[3],
        PaintValues {
            border: [border; 4],
            shadows: Box::new([
                shadow,
                Shadow {
                    inset: true,
                    ..shadow
                },
            ]),
            outline: Some(Outline {
                side: border,
                offset: 1.0,
            }),
            ..ink()
        },
    );
    let key = GlyphKey {
        font: FontInstanceId(42),
        glyph_id: 5,
        phase: [0, 0],
        scale: 1,
    };
    let mut text = fragment(3, Some(2), &[], PaintValues::default());
    text.kind = FragmentKind::Text;
    text.content = FragmentContent::Text(Box::new([PositionedRun {
        glyphs: Box::new([(key, point(3.0, 10.0))]),
        under_and_overlines: Box::new([(Rect::new(3.0, 12.0, 10.0, 1.0), color)]),
        line_through: Box::new([(Rect::new(3.0, 8.0, 10.0, 1.0), color)]),
    }]));
    let tree = tree([fragment(1, None, &[2], PaintValues::default()), panel, text]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(1)).unwrap();
    let items: Vec<_> = frame
        .display_list
        .iter()
        .map(|item| &item.primitive)
        .collect();
    assert!(matches!(
        items.as_slice(),
        [
            Primitive::Shadow { shadow: Shadow { inset: false, .. }, .. },
            Primitive::RoundedRect { .. },
            Primitive::Shadow { shadow: Shadow { inset: true, .. }, .. },
            Primitive::Border { .. },
            Primitive::RoundedRect { .. },
            Primitive::GlyphRun(run),
            Primitive::RoundedRect { .. },
            Primitive::Border { .. },
        ] if *run.glyphs == [(key, point(3.0, 10.0))]
    ));
    assert_eq!(
        frame.display_list.last().unwrap().bounds,
        Rect::new(-3.0, -3.0, 106.0, 106.0)
    );
}

#[test]
fn nested_positioned_auto_boxes_keep_preorder_in_the_outer_context() {
    let positioned = || PaintValues {
        position: Position::Relative,
        ..ink()
    };
    let tree = tree([
        fragment(1, None, &[2, 4], PaintValues::default()),
        fragment(2, Some(1), &[3], positioned()),
        fragment(3, Some(2), &[], positioned()),
        fragment(4, Some(1), &[], positioned()),
    ]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(0)).unwrap();
    let order: Vec<_> = frame.display_list.iter().map(|item| item.id.0.0).collect();
    assert_eq!(order, [2, 3, 4]);
}

#[test]
fn perspective_hit_testing_projects_onto_the_transformed_quad() {
    let tree = tree([
        fragment(1, None, &[2], PaintValues::default()),
        fragment(
            2,
            Some(1),
            &[],
            PaintValues {
                transform: Some([
                    [1.0, 0.0, 0.0, 0.01],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ]),
                ..ink()
            },
        ),
    ]);
    let frame = build_resolved_frame(&tree, &Mirror::default(), ResourceSeq(0)).unwrap();
    let hit = |x, y| hit_test_tree(&frame.hit_test, &frame.property_trees, point(x, y));
    assert_eq!(hit(45.0, 20.0), Some(node(2)));
    assert_eq!(hit(60.0, 20.0), None);
}
