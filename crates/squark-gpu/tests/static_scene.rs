// SPDX-License-Identifier: MIT

use std::sync::Arc;

use squark_gpu::{Error, StaticRenderer, TileStats};
use squark_paint::{CommittedFrame, Damage, build_frame, frame_damage};
use squark_types::{
    NodeId, Point, Rect, Size,
    fragment::{
        ArtValues, AtomId, BackgroundClip, BackgroundImage, BackgroundLayer, BlendMode, BorderSide,
        BorderStyle, Clip, ClipShape, DisplayKind, Edges, FillRule, Filter, Fragment,
        FragmentContent, FragmentId, FragmentKind, FragmentTree, GeometryBox, Gradient,
        GradientGeometry, GradientStop, HueInterpolation, ImageRendering, ImageSize,
        LengthPercentage, MirrorRead, Name, NodeKind, NodeRead, Overflow, PaintColor, PaintValues,
        PathCommand, Position2D, PositionKind, PositionedGlyph, PositionedRun, RadialExtent,
        Repeat, ScrollFrame, Shadow, StickyConstraints, SvgGeometry, SvgPaint, SvgPath, Transform,
    },
    render::{
        Bitmap, BitmapFormat, FontInstanceId, FrameId, GlyphKey, ImageKey, ResourceKey,
        ResourceMessage, ResourceSeq, ResourceUpdate, Viewport, resource_channel,
    },
};

const ROOT: FragmentId = FragmentId(1);
const WHITE: [u8; 4] = [255; 4];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];

fn point(x: f32, y: f32) -> Point {
    Point { x, y }
}

fn color(r: f32, g: f32, b: f32) -> PaintColor {
    PaintColor::srgb(r, g, b, 1.0)
}

fn solid(background_color: PaintColor) -> PaintValues {
    PaintValues {
        art: ArtValues {
            background_color,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn scale(x: f32, y: f32) -> Transform {
    Transform::Matrix([
        [x, 0.0, 0.0, 0.0],
        [0.0, y, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])
}

fn percent(fraction: f32) -> LengthPercentage {
    LengthPercentage { px: 0.0, fraction }
}

fn length(px: f32) -> LengthPercentage {
    LengthPercentage { px, fraction: 0.0 }
}

fn position(x: f32, y: f32) -> Position2D {
    Position2D {
        x: percent(x),
        y: percent(y),
    }
}

fn gpu() -> Option<StaticRenderer> {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return None;
    }
    let gpu = pollster::block_on(StaticRenderer::headless()).unwrap();
    eprintln!("static renderer adapter: {}", gpu.adapter_name());
    Some(gpu)
}

struct Mirror {
    svg: Option<NodeId>,
}

impl MirrorRead for Mirror {
    fn document(&self) -> NodeId {
        NodeId::new(1, 0).unwrap()
    }
    fn node(&self, id: NodeId) -> Option<NodeRead<'_>> {
        (!id.is_none()).then(|| NodeRead {
            kind: if id == self.document() {
                NodeKind::Document
            } else {
                NodeKind::Element
            },
            links: Default::default(),
            name: (self.svg == Some(id)).then_some(Name {
                local: AtomId(1),
                namespace: Some(AtomId(2)),
                prefix: None,
            }),
            attrs: Vec::new(),
            text: None,
            state: Default::default(),
            own: Default::default(),
            descendants: Default::default(),
            listeners: Default::default(),
        })
    }
    fn atom(&self, id: AtomId) -> Option<&[u16]> {
        static LOCAL: std::sync::LazyLock<Vec<u16>> =
            std::sync::LazyLock::new(|| "svg".encode_utf16().collect());
        static NAMESPACE: std::sync::LazyLock<Vec<u16>> =
            std::sync::LazyLock::new(|| "http://www.w3.org/2000/svg".encode_utf16().collect());
        match id.0 {
            1 => Some(&LOCAL),
            2 => Some(&NAMESPACE),
            _ => None,
        }
    }
}

struct Scene {
    tree: FragmentTree,
    viewport: Viewport,
    mirror: Mirror,
}

impl Scene {
    fn new(device: [u32; 2], scale: f64, style: PaintValues) -> Self {
        let css = Size {
            width: device[0] as f32 / scale as f32,
            height: device[1] as f32 / scale as f32,
        };
        let root = fragment(
            ROOT,
            None,
            Rect::new(0.0, 0.0, css.width, css.height),
            style,
        );
        Self {
            tree: FragmentTree {
                root: ROOT,
                ime_area: None,
                nodes: [(ROOT, root)].into(),
            },
            viewport: Viewport {
                css,
                device,
                scale,
                epoch: 1,
            },
            mirror: Mirror { svg: None },
        }
    }

    fn add(&mut self, parent: FragmentId, rect: Rect, style: PaintValues) -> &mut Fragment {
        let id = FragmentId(self.tree.nodes.len() as u64 + 1);
        let parent_fragment = self.tree.nodes.get_mut(&parent).unwrap();
        let mut children = parent_fragment.children.to_vec();
        children.push(id);
        parent_fragment.children = children.into_boxed_slice();
        self.tree
            .nodes
            .entry(id)
            .or_insert_with(|| fragment(id, Some(parent), rect, style))
    }

    fn commit(&self, prefix: ResourceSeq) -> CommittedFrame {
        CommittedFrame {
            id: FrameId(1),
            viewport: self.viewport,
            frame: build_frame(&self.tree, &self.mirror, prefix, self.viewport.scale).unwrap(),
            publication: None,
        }
    }

    fn render(&self, gpu: &mut StaticRenderer, prefix: ResourceSeq) -> Result<Pixels, Error> {
        gpu.render_rgba(&self.commit(prefix)).map(|data| Pixels {
            width: self.viewport.device[0] as usize,
            data,
        })
    }
}

fn fragment(
    id: FragmentId,
    parent: Option<FragmentId>,
    rect: Rect,
    mut style: PaintValues,
) -> Fragment {
    style.stacking.display = DisplayKind::Block;
    Fragment {
        id,
        owner: NodeId::new(id.0 as u32, 0).unwrap(),
        kind: if id == ROOT {
            FragmentKind::Viewport
        } else {
            FragmentKind::Box
        },
        parent,
        containing_block: parent,
        children: Box::new([]),
        border_box: rect,
        padding: Edges::default(),
        border: Edges::default(),
        first_baseline: None,
        last_baseline: None,
        scrollable_overflow: rect,
        scroll: None,
        sticky: None,
        content: FragmentContent::None,
        style: Arc::new(style),
    }
}

struct Pixels {
    width: usize,
    data: Vec<u8>,
}

impl Pixels {
    fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let at = (x + y * self.width) * 4;
        self.data[at..at + 4].try_into().unwrap()
    }

    fn near(&self, x: usize, y: usize, pixel: [u8; 4], delta: u8) {
        let actual = self.at(x, y);
        assert!(
            actual
                .into_iter()
                .zip(pixel)
                .all(|(a, b)| a.abs_diff(b) <= delta),
            "({x}, {y}) expected {pixel:?}, got {actual:?}"
        );
    }
}

#[test]
fn renders_propagated_canvas_and_box_geometry() {
    let Some(mut gpu) = gpu() else { return };
    let canvas = PaintColor::srgb(0.0, 0.0, 1.0, 0.5);
    let mut viewport = solid(canvas);
    viewport.box_paint.radii = [position(0.5, 0.5); 4];
    let mut scene = Scene::new([73, 43], 1.0, viewport);
    let mut body = solid(canvas);
    body.box_paint.background_propagated = true;
    scene.add(ROOT, Rect::new(3.0, 2.0, 68.0, 39.0), body);
    let mut box_style = solid(color(1.0, 0.0, 0.0));
    box_style.box_paint.radii = [Position2D {
        x: length(6.0),
        y: length(4.0),
    }; 4];
    box_style.box_paint.border.top = BorderSide {
        style: BorderStyle::Double,
        color: color(0.0, 0.0, 0.0),
    };
    box_style.art.box_shadows = Box::new([Shadow {
        offset: point(5.0, 1.0),
        blur: 4.0,
        spread: 1.0,
        color: color(0.0, 0.0, 0.0),
        inset: false,
    }]);
    scene
        .add(ROOT, Rect::new(10.0, 8.0, 34.0, 20.0), box_style)
        .border
        .top = 6.0;
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    assert_eq!(rgba.data.len(), 73 * 43 * 4);
    rgba.near(0, 0, [127, 127, 255, 255], 1);
    rgba.near(5, 38, [127, 127, 255, 255], 1);
    assert_eq!(rgba.at(25, 18), RED);
    assert_eq!(rgba.at(25, 8), [0, 0, 0, 255]);
    assert_eq!(rgba.at(25, 11), RED);
    assert_eq!(rgba.at(25, 13), [0, 0, 0, 255]);
    assert!(rgba.at(46, 19)[2] < 90);
    assert_eq!(rgba.at(59, 19), rgba.at(0, 0));
}

#[test]
fn a_blurred_box_shadow_accounts_for_the_finite_width_and_height() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([30, 20], 1.0, PaintValues::default());
    let mut style = PaintValues::default();
    style.art.box_shadows = Box::new([Shadow {
        offset: point(0.0, 0.0),
        blur: 8.0,
        spread: 0.0,
        color: color(0.0, 0.0, 0.0),
        inset: false,
    }]);
    scene.add(ROOT, Rect::new(10.0, 10.0, 2.0, 2.0), style);
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    // At this pixel the Gaussian integral over the 2×2 box is about .032.
    // A single-edge falloff would make the shadow much darker.
    rgba.near(8, 10, [247, 247, 247, 255], 2);
    assert_eq!(rgba.at(10, 10), WHITE);
}

#[test]
fn resources_carry_atlas_pixels_and_glyph_bearings_across_frames() {
    let Some(mut gpu) = gpu() else { return };
    let (sender, receiver) = resource_channel();
    let image = ImageKey(8);
    let glyph = GlyphKey::for_position(FontInstanceId(4), 11, point(12.25, 8.0), 2.0);
    let color_glyph = GlyphKey {
        glyph_id: 12,
        ..glyph
    };
    let empty_glyph = GlyphKey {
        glyph_id: 13,
        ..glyph
    };
    let bitmap = Bitmap {
        size: [2, 2],
        stride: 12,
        format: BitmapFormat::Rgba8SrgbPremultiplied,
        pixels: [
            255, 0, 0, 255, 0, 255, 0, 255, 17, 17, 17, 17, 0, 0, 255, 255, 0, 0, 0, 0,
        ]
        .into(),
    };
    let before = sender
        .send(ResourceUpdate::PutImage { key: image, bitmap })
        .unwrap();
    let coverage = Bitmap {
        size: [3, 2],
        stride: 5,
        format: BitmapFormat::R8Coverage,
        pixels: [255, 128, 255, 17, 17, 255, 255, 255].into(),
    };
    let color_bitmap = Bitmap {
        size: [2, 2],
        stride: 8,
        format: BitmapFormat::Rgba8SrgbPremultiplied,
        pixels: [0, 128, 0, 128].repeat(4).into(),
    };
    sender
        .send(ResourceUpdate::PutGlyph {
            key: glyph,
            bitmap: coverage.clone(),
            bearing: [1, 2],
        })
        .unwrap();
    sender
        .send(ResourceUpdate::PutGlyph {
            key: color_glyph,
            bitmap: color_bitmap,
            bearing: [0, 2],
        })
        .unwrap();
    let before_free = sender
        .send(ResourceUpdate::PutGlyph {
            key: empty_glyph,
            bitmap: Bitmap {
                size: [0, 0],
                stride: 0,
                pixels: Arc::new([]),
                ..coverage
            },
            bearing: [0, 0],
        })
        .unwrap();
    let mut scene = Scene::new([73, 33], 2.0, PaintValues::default());
    scene
        .add(ROOT, Rect::new(1.0, 1.0, 8.0, 8.0), PaintValues::default())
        .content = FragmentContent::Image {
        key: image,
        intrinsic_size: Size {
            width: 2.0,
            height: 2.0,
        },
    };
    let style = PaintValues::default();
    let run = scene.add(ROOT, Rect::new(10.0, 3.0, 15.0, 10.0), style);
    run.kind = FragmentKind::Text;
    run.content = FragmentContent::Text(Box::new([PositionedRun {
        font: glyph.font,
        baseline: 8.0,
        bidi_level: 0,
        source_ranges: Box::new([]),
        decoration_spans: Box::new([]),
        glyphs: [11, 12, 13]
            .into_iter()
            .enumerate()
            .map(|(i, glyph_id)| PositionedGlyph {
                glyph_id,
                position: point(12.25 + i as f32 * 4.0, 8.0),
                cluster: 0,
            })
            .collect(),
    }]));
    assert!(scene.render(&mut gpu, before).is_err());
    let freed = sender
        .send(ResourceUpdate::Free {
            key: ResourceKey::Image(image),
        })
        .unwrap();
    assert_eq!(gpu.drain_resources(&receiver).unwrap(), freed);
    let rgba = scene.render(&mut gpu, before_free).unwrap();
    assert_eq!(rgba.at(3, 3), RED);
    assert_eq!(rgba.at(3, 16), BLUE);
    assert_eq!(rgba.at(16, 16), WHITE);
    assert_eq!(rgba.at(25, 14), [0, 0, 0, 255]);
    rgba.near(26, 14, [130, 130, 130, 255], 1);
    rgba.near(32, 14, [127, 255, 127, 255], 1);
    assert!(scene.render(&mut gpu, freed).is_err());
    gpu.retire_resources(freed).unwrap();
    assert!(scene.render(&mut gpu, before_free).is_err());
    assert!(
        gpu.apply_resources([ResourceMessage {
            seq: ResourceSeq(freed.0 + 2),
            update: ResourceUpdate::Free {
                key: ResourceKey::Glyph(glyph)
            }
        }])
        .is_err()
    );
    assert_eq!(gpu.applied_resources(), freed);
}

#[test]
fn glyph_coverage_correction_depends_on_text_color() {
    let Some(mut gpu) = gpu() else { return };
    let black = color(0.0, 0.0, 0.0);
    let white = color(1.0, 1.0, 1.0);
    let mut scene = Scene::new([12, 4], 1.0, PaintValues::default());
    scene.add(ROOT, Rect::new(6.0, 0.0, 6.0, 4.0), solid(black));
    let font = FontInstanceId(1);
    for (x, ink) in [(1.0, black), (7.0, white)] {
        let mut style = PaintValues::default();
        style.art.color = ink;
        let fragment = scene.add(ROOT, Rect::new(x, 1.0, 3.0, 1.0), style);
        fragment.kind = FragmentKind::Text;
        fragment.content = FragmentContent::Text(Box::new([PositionedRun {
            font,
            baseline: 2.0,
            bidi_level: 0,
            source_ranges: Box::new([]),
            decoration_spans: Box::new([]),
            glyphs: Box::new([PositionedGlyph {
                glyph_id: 1,
                position: point(x, 2.0),
                cluster: 0,
            }]),
        }]));
    }
    let (sender, receiver) = resource_channel();
    let prefix = sender
        .send(ResourceUpdate::PutGlyph {
            key: GlyphKey::for_position(font, 1, point(1.0, 2.0), 1.0),
            bitmap: Bitmap {
                size: [3, 1],
                stride: 3,
                format: BitmapFormat::R8Coverage,
                pixels: [0, 128, 255].into(),
            },
            bearing: [0, 1],
        })
        .unwrap();
    gpu.drain_resources(&receiver).unwrap();
    let rgba = scene.render(&mut gpu, prefix).unwrap();
    let black = [0, 0, 0, 255];
    assert_eq!(rgba.at(1, 1), WHITE);
    rgba.near(2, 1, [130, 130, 130, 255], 1);
    assert_eq!(rgba.at(3, 1), black);
    assert_eq!(rgba.at(7, 1), black);
    rgba.near(8, 1, [143, 143, 143, 255], 1);
    assert_eq!(rgba.at(9, 1), WHITE);
}

fn stops() -> Box<[GradientStop]> {
    Box::new([
        GradientStop::Color {
            position: None,
            color: color(1.0, 0.0, 0.0),
        },
        GradientStop::Color {
            position: None,
            color: color(0.0, 0.0, 1.0),
        },
    ])
}

fn gradient(shape: GradientGeometry) -> PaintValues {
    let mut style = PaintValues::default();
    style.box_paint.backgrounds = Box::new([BackgroundLayer {
        origin: GeometryBox::Border,
        clip: BackgroundClip::Border,
        size: ImageSize::Explicit([None; 2]),
        position: position(0.0, 0.0),
        repeat: [Repeat::Repeat; 2],
        image: BackgroundImage::Gradient(Arc::new(Gradient {
            geometry: shape,
            space: squark_types::fragment::ColorSpace::Srgb,
            hue: HueInterpolation::Shorter,
            stops: stops(),
            repeating: false,
        })),
    }]);
    style
}

fn rectangle(x: f32, y: f32, w: f32, h: f32) -> [PathCommand; 5] {
    [
        PathCommand::Move(point(x, y)),
        PathCommand::Line(point(x + w, y)),
        PathCommand::Line(point(x + w, y + h)),
        PathCommand::Line(point(x, y + h)),
        PathCommand::Close,
    ]
}

#[test]
fn renders_all_gradient_geometries_and_vello_paths_through_nested_clips() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([111, 65], 1.0, PaintValues::default());
    scene.add(
        ROOT,
        Rect::new(2.0, 2.0, 30.0, 30.0),
        gradient(GradientGeometry::LinearTo {
            horizontal: 1,
            vertical: 0,
        }),
    );
    scene.add(
        ROOT,
        Rect::new(35.0, 2.0, 30.0, 30.0),
        gradient(GradientGeometry::Radial {
            center: position(0.5, 0.5),
            extent: RadialExtent::ClosestSide,
            circle: true,
        }),
    );
    scene.add(
        ROOT,
        Rect::new(68.0, 2.0, 30.0, 30.0),
        gradient(GradientGeometry::Conic {
            center: position(0.5, 0.5),
            from_radians: 0.0,
        }),
    );
    let mut outer = PaintValues::default();
    outer.properties.clip = Some(Clip {
        reference_box: GeometryBox::Border,
        shape: ClipShape::Inset {
            edges: Edges {
                left: length(2.0),
                right: length(4.0),
                ..Default::default()
            },
            radii: [Position2D {
                x: length(5.0),
                y: length(3.0),
            }; 4],
        },
    });
    let parent = scene.add(ROOT, Rect::new(4.0, 35.0, 32.0, 25.0), outer).id;
    let mut inner = PaintValues::default();
    inner.properties.transform = Box::new([scale(1.0, 1.0)]);
    let hole: Vec<_> = rectangle(0.0, 0.0, 30.0, 23.0)
        .into_iter()
        .chain(rectangle(7.0, 6.0, 16.0, 10.0))
        .collect();
    inner.properties.clip = Some(Clip {
        reference_box: GeometryBox::Border,
        shape: ClipShape::Path {
            commands: hole.into(),
            fill_rule: FillRule::EvenOdd,
        },
    });
    let svg = scene.add(parent, Rect::new(4.0, 35.0, 30.0, 23.0), inner);
    svg.kind = FragmentKind::Replaced;
    svg.content = FragmentContent::Svg(Arc::new(SvgGeometry {
        paths: Box::new([SvgPath {
            commands: rectangle(0.0, 0.0, 30.0, 23.0).into(),
            fill: Some(SvgPaint::Color(color(0.0, 0.0, 1.0))),
            fill_rule: FillRule::NonZero,
            stroke: None,
            clip: None,
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }]),
    }));
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    rgba.near(17, 17, [123, 0, 132, 255], 2);
    rgba.near(49, 16, [243, 0, 12, 255], 3);
    assert!(rgba.at(86, 7)[0] > rgba.at(80, 7)[0]);
    assert_eq!(rgba.at(7, 46), BLUE);
    assert_eq!(rgba.at(19, 46), WHITE);
    assert_eq!(rgba.at(5, 46), WHITE);
    assert_eq!(rgba.at(33, 46), WHITE);
}

#[test]
fn conic_gradients_have_no_seam_when_their_center_is_a_pixel_center() {
    let Some(mut gpu) = gpu() else { return };
    // A 31px box puts the center at (15.5, 15.5), the center of pixel (15, 15),
    // so the pixels level with it and below it sit exactly on the axes, where
    // the shader once drew the color from the opposite side.
    let mut scene = Scene::new([31, 31], 1.0, PaintValues::default());
    scene.add(
        ROOT,
        Rect::new(0.0, 0.0, 31.0, 31.0),
        gradient(GradientGeometry::Conic {
            center: position(0.5, 0.5),
            from_radians: 0.0,
        }),
    );
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    let between = |x: usize, y: usize, [a, b]: [(usize, usize); 2]| {
        let (a, b) = (rgba.at(a.0, a.1), rgba.at(b.0, b.1));
        let mean: [u8; 4] =
            std::array::from_fn(|i| ((u16::from(a[i]) + u16::from(b[i])) / 2) as u8);
        rgba.near(x, y, mean, 6);
    };
    // Right (90 degrees) and left (270 degrees) of the center, on its row.
    for x in [20, 25, 30] {
        between(x, 15, [(x, 14), (x, 16)]);
    }
    for x in [0, 5, 10] {
        between(x, 15, [(x, 14), (x, 16)]);
    }
    // Below the center (180 degrees), on its column.
    for y in [20, 25, 30] {
        between(15, y, [(14, y), (16, y)]);
    }
    // A quarter of the way from red to blue on the right, three quarters on the left.
    rgba.near(30, 15, [191, 0, 64, 255], 12);
    rgba.near(0, 15, [64, 0, 191, 255], 12);
}

fn svg_rect(rect: Rect, fill: PaintColor) -> FragmentContent {
    FragmentContent::Svg(Arc::new(SvgGeometry {
        paths: Box::new([SvgPath {
            commands: rectangle(
                rect.origin.x,
                rect.origin.y,
                rect.size.width,
                rect.size.height,
            )
            .into(),
            fill: Some(SvgPaint::Color(fill)),
            fill_rule: FillRule::NonZero,
            stroke: None,
            clip: None,
            transform: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }]),
    }))
}

#[test]
fn snaps_inline_svg_paint_origin_in_css_pixels_without_moving_hits() {
    let Some(mut gpu) = gpu() else { return };
    for scale in [1, 2] {
        let mut scene = Scene::new(
            [30 * scale, 20 * scale],
            f64::from(scale),
            PaintValues::default(),
        );
        let svg = scene.add(ROOT, Rect::new(10.5, 4.5, 8.0, 9.0), PaintValues::default());
        svg.kind = FragmentKind::Replaced;
        svg.padding.left = 1.0;
        svg.content = svg_rect(Rect::new(0.0, 0.0, 6.0, 8.0), color(0.0, 0.0, 1.0));
        let owner = svg.owner;
        scene.mirror.svg = Some(owner);

        let hit = squark_paint::HitSnapshot::new(&scene.commit(ResourceSeq(0)));
        assert_eq!(
            squark_paint::hit_test(&hit, point(10.75, 4.75))
                .unwrap()
                .target,
            owner
        );
        let pixels = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
        let scale = scale as usize;
        assert_eq!(pixels.at(12 * scale - 1, 7 * scale), WHITE);
        assert_eq!(pixels.at(12 * scale, 7 * scale), BLUE);
        assert_eq!(pixels.at(14 * scale, 5 * scale - 1), WHITE);
        assert_eq!(pixels.at(14 * scale, 5 * scale), BLUE);
    }
}

#[test]
fn rasterizes_fractionally_translated_svg_on_the_device_grid() {
    let Some(mut gpu) = gpu() else { return };
    let render = |gpu: &mut StaticRenderer, offset, x| {
        let mut scene = Scene::new([30, 20], 1.0, PaintValues::default());
        let mut style = PaintValues::default();
        if offset != 0.0 {
            style.properties.transform = Box::new([Transform::Translate {
                xy: Position2D {
                    x: length(offset),
                    y: length(0.0),
                },
                z: 0.0,
            }]);
        }
        let svg = scene.add(ROOT, Rect::new(10.0, 4.0, 2.0, 10.0), style);
        svg.kind = FragmentKind::Replaced;
        svg.content = svg_rect(Rect::new(x, 0.0, 0.5, 8.0), color(0.0, 0.0, 0.0));
        scene.mirror.svg = Some(svg.owner);
        scene.render(gpu, ResourceSeq(0)).unwrap()
    };
    let translated = render(&mut gpu, 0.75, 0.0);
    let positioned = render(&mut gpu, 0.0, 0.75);
    assert_eq!(translated.data, positioned.data);
    assert_ne!(translated.at(10, 7), WHITE);
    assert_ne!(translated.at(11, 7), WHITE);
    assert_eq!(translated.at(9, 7), WHITE);
    assert_eq!(translated.at(12, 7), WHITE);
}

#[test]
fn isolates_effect_subtrees_and_applies_filters_and_blend_in_order() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([97, 49], 1.0, PaintValues::default());
    let mut group = PaintValues::default();
    group.properties.opacity = 0.5;
    let parent = scene.add(ROOT, Rect::new(0.0, 0.0, 25.0, 22.0), group).id;
    scene.add(
        parent,
        Rect::new(1.0, 2.0, 14.0, 12.0),
        solid(color(1.0, 0.0, 0.0)),
    );
    scene.add(
        parent,
        Rect::new(9.0, 2.0, 14.0, 12.0),
        solid(color(1.0, 0.0, 0.0)),
    );
    let mut filter = solid(color(1.0, 0.0, 0.0));
    filter.properties.filters = Box::new([
        Filter::Invert(1.0),
        Filter::HueRotate(std::f32::consts::FRAC_PI_2),
    ]);
    scene.add(ROOT, Rect::new(28.0, 2.0, 20.0, 12.0), filter);

    scene.add(
        ROOT,
        Rect::new(55.0, 2.0, 18.0, 16.0),
        solid(color(1.0, 0.5, 0.0)),
    );
    let mut blend = solid(color(0.0, 1.0, 1.0));
    blend.properties.blend = BlendMode::Multiply;
    scene.add(ROOT, Rect::new(58.0, 3.0, 16.0, 11.0), blend);
    let mut blur = solid(color(0.0, 0.0, 0.0));
    blur.properties.filters = Box::new([Filter::Blur(2.0)]);
    scene.add(ROOT, Rect::new(5.0, 29.0, 15.0, 13.0), blur);
    scene.add(
        ROOT,
        Rect::new(40.0, 28.0, 20.0, 14.0),
        solid(color(0.0, 0.0, 1.0)),
    );
    let mut backdrop = PaintValues::default();
    backdrop.properties.backdrop_filters = Box::new([Filter::Invert(1.0)]);
    scene.add(ROOT, Rect::new(46.0, 30.0, 12.0, 8.0), backdrop);

    let mut projected = solid(color(1.0, 0.0, 0.0));
    projected.properties.transform_origin = position(0.0, 0.0);
    projected.properties.transform = Box::new([Transform::Matrix([
        [1.0, 0.0, 0.0, 0.02],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ])]);
    scene.add(ROOT, Rect::new(73.0, 24.0, 20.0, 20.0), projected);
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    rgba.near(4, 5, [255, 127, 127, 255], 1);
    assert_eq!(rgba.at(4, 5), rgba.at(10, 5));
    rgba.near(30, 5, [255, 164, 255, 255], 2);
    rgba.near(60, 8, [0, 128, 0, 255], 1);
    assert!(rgba.at(4, 34)[0] > 120 && rgba.at(4, 34)[0] < 220);
    assert!(rgba.at(12, 34)[0] < 4);
    assert_eq!(rgba.at(44, 34), BLUE);
    assert_eq!(rgba.at(50, 34), [255, 255, 0, 255]);
    assert_eq!(rgba.at(83, 33), RED);
    assert_eq!(rgba.at(90, 33), WHITE);

    let reference = include_bytes!("fixtures/effects.chromium.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(reference));
    let mut reader = decoder.read_info().unwrap();
    let mut expected = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut expected).unwrap();
    assert_eq!((info.width, info.height), (97, 49));
    let expected_channels = info.color_type.samples();
    assert!(expected_channels >= 3);
    let different = rgba
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected[..info.buffer_size()].chunks_exact(expected_channels))
        .filter(|(actual, expected)| {
            actual
                .iter()
                .zip(*expected)
                .any(|(a, b)| a.abs_diff(*b) > 12)
        })
        .count();
    assert!(
        different <= 95,
        "{different}/4753 pixels differ from Chromium by more than 12"
    );
    eprintln!("{different}/4753 effect pixels differ from Chromium by more than 12");
}

#[test]
fn paints_resource_gradients_background_tiles_and_preserved_depth() {
    use squark_paint::{
        DisplayItem, GradientGeometry as ResolvedGeometry, ItemId, Primitive, PropertyRefs,
        RoundedRect,
    };
    use squark_types::render::GradientKey;

    let Some(mut gpu) = gpu() else { return };
    let (sender, receiver) = resource_channel();
    let image = ImageKey(4);
    sender
        .send(ResourceUpdate::PutImage {
            key: image,
            bitmap: Bitmap {
                size: [1, 1],
                stride: 4,
                format: BitmapFormat::Rgba8SrgbPremultiplied,
                pixels: BLUE.into(),
            },
        })
        .unwrap();
    let ramp = GradientKey(9);
    let prefix = sender
        .send(ResourceUpdate::PutGradient {
            key: ramp,
            ramp: Bitmap {
                size: [2, 1],
                stride: 8,
                format: BitmapFormat::Rgba8SrgbPremultiplied,
                pixels: RED.into_iter().chain(BLUE).collect(),
            },
        })
        .unwrap();
    assert_eq!(gpu.drain_resources(&receiver).unwrap(), prefix);
    let mut scene = Scene::new([53, 31], 1.0, PaintValues::default());
    let mut repeat = PaintValues::default();
    repeat.box_paint.backgrounds = Box::new([BackgroundLayer {
        image: BackgroundImage::Image(image),
        origin: GeometryBox::Border,
        clip: BackgroundClip::Border,
        size: ImageSize::Explicit([Some(length(4.0)), Some(length(6.0))]),
        position: position(0.0, 0.0),
        repeat: [Repeat::Space, Repeat::NoRepeat],
    }]);
    scene.add(ROOT, Rect::new(1.0, 3.0, 15.0, 8.0), repeat);
    let mut preserve = PaintValues::default();
    preserve.properties.preserve_3d = true;
    let group = scene
        .add(ROOT, Rect::new(20.0, 0.0, 25.0, 25.0), preserve)
        .id;
    let mut front = solid(color(1.0, 0.0, 0.0));
    front.properties.transform = Box::new([Transform::Translate {
        xy: Default::default(),
        z: 10.0,
    }]);
    scene.add(group, Rect::new(22.0, 6.0, 20.0, 10.0), front);
    let mut back = solid(color(0.0, 0.0, 1.0));
    back.properties.transform = Box::new([Transform::Translate {
        xy: Default::default(),
        z: -5.0,
    }]);
    scene.add(group, Rect::new(24.0, 4.0, 20.0, 10.0), back);
    let mut frame = scene.commit(prefix);
    let items = Arc::get_mut(&mut frame.frame.display_list).unwrap();
    let rect = Rect::new(0.0, 19.0, 20.0, 10.0);
    items.push(DisplayItem {
        id: ItemId(ROOT, 0),
        bounds: rect,
        properties: PropertyRefs::default(),
        primitive: Primitive::GradientRamp {
            shape: RoundedRect::new(rect, Default::default()),
            key: ramp,
            geometry: ResolvedGeometry::Linear {
                start: point(0.0, 24.0),
                end: point(20.0, 24.0),
            },
            stop_range: [0.0, 1.0],
            repeating: false,
        },
    });
    let rgba = Pixels {
        width: 53,
        data: gpu.render_rgba(&frame).unwrap(),
    };
    assert_eq!(rgba.at(2, 5), BLUE);
    assert_eq!(rgba.at(5, 5), WHITE);
    assert_eq!(rgba.at(8, 5), BLUE);
    assert_eq!(rgba.at(15, 5), BLUE);
    assert_eq!(rgba.at(30, 8), RED);
    assert_eq!(rgba.at(42, 8), BLUE);
    rgba.near(10, 24, [121, 0, 134, 255], 2);
}

#[test]
fn retained_scroll_moves_sticky_content_and_the_classic_scrollbar() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([60, 58], 1.0, PaintValues::default());
    let mut scroller_style = PaintValues::default();
    scroller_style.box_paint.overflow = [Overflow::Hidden, Overflow::Auto];
    let scrollport = Rect::new(5.0, 5.0, 40.0, 40.0);
    let scroller = scene.add(ROOT, scrollport, scroller_style);
    let scroll_owner = scroller.owner;
    let parent = scroller.id;
    scroller.scroll = Some(ScrollFrame {
        owner: scroll_owner,
        offset: Point::default(),
        generation: 0,
        scrollport,
        content: Rect::new(5.0, 5.0, 40.0, 120.0),
        gutter: Edges {
            right: 8.0,
            ..Default::default()
        },
        scroll_x: true,
        scroll_y: true,
        clip_x: true,
        clip_y: true,
    });
    let mut header_style = solid(color(1.0, 0.0, 0.0));
    header_style.stacking.position = PositionKind::Sticky;
    let header = scene.add(parent, Rect::new(6.0, 15.0, 24.0, 8.0), header_style);
    header.sticky = Some(StickyConstraints {
        scroll_owner,
        containing_rect: Rect::new(5.0, 5.0, 40.0, 58.0),
        insets: Edges {
            top: Some(2.0),
            ..Default::default()
        },
    });

    let mut frame = scene.commit(ResourceSeq(0));
    let mut draw = |frame: &CommittedFrame| Pixels {
        width: 60,
        data: gpu.render_rgba(frame).unwrap(),
    };
    let before = draw(&frame);
    assert_eq!(before.at(8, 8), WHITE);
    assert_eq!(before.at(8, 16), RED);
    assert_eq!(before.at(40, 8), [193, 193, 193, 255]);
    assert_eq!(before.at(40, 22), [241, 241, 241, 255]);

    frame.frame.property_trees.scrolls[1].initial_offset.y = 30.0;
    let scrolled = draw(&frame);
    assert_eq!(scrolled.at(8, 8), RED);
    assert_eq!(scrolled.at(8, 16), WHITE);
    assert_eq!(scrolled.at(40, 8), [241, 241, 241, 255]);
    assert_eq!(scrolled.at(40, 22), [193, 193, 193, 255]);

    frame.frame.property_trees.scrolls[1].initial_offset.y = 60.0;
    let past_container = draw(&frame);
    assert_eq!(past_container.at(8, 8), WHITE);
}

#[track_caller]
fn assert_tiles_equal(gpu: &mut StaticRenderer, frame: &CommittedFrame) {
    let cached = gpu.render_cached_rgba(frame).unwrap();
    let full = gpu.render_rgba(frame).unwrap();
    if cached != full {
        let width = frame.viewport.device[0] as usize;
        let differences = cached
            .as_chunks::<4>()
            .0
            .iter()
            .zip(full.as_chunks::<4>().0)
            .enumerate()
            .filter(|(_, (cached, full))| cached != full);
        let sample: Vec<_> = differences
            .clone()
            .take(8)
            .map(|(i, (a, b))| (i % width, i / width, a, b))
            .collect();
        panic!(
            "{} different pixels (x, y, cached, full): {:?}",
            differences.count(),
            sample
        );
    }
}

fn canvas_stream(records: &[(u8, Vec<u8>)]) -> Arc<[u8]> {
    let mut bytes = vec![0; 12];
    for (opcode, payload) in records {
        bytes.extend([*opcode, 0, 0, 0]);
        bytes.extend((payload.len() as u32).to_le_bytes());
        bytes.extend(payload);
        bytes.resize(bytes.len() + (8 - payload.len() % 8) % 8, 0);
    }
    let len = bytes.len() as u32;
    bytes[..4].copy_from_slice(b"JCV1");
    bytes[4..8].copy_from_slice(&len.to_le_bytes());
    bytes[8..12].copy_from_slice(&(records.len() as u32).to_le_bytes());
    bytes.into()
}

fn canvas_rect(rect: [f64; 4]) -> Vec<u8> {
    rect.into_iter().flat_map(f64::to_le_bytes).collect()
}

fn canvas_paint(color: [u8; 4]) -> [(u8, Vec<u8>); 2] {
    let mut definition = 1u32.to_le_bytes().to_vec();
    definition.extend(0u32.to_le_bytes());
    definition.extend(color);
    [(20, definition), (18, 1u32.to_le_bytes().to_vec())]
}

fn canvas_draw_image(kind: u32, source: u32, src: [f64; 4], dst: [f64; 4]) -> (u8, Vec<u8>) {
    let bytes = kind
        .to_le_bytes()
        .into_iter()
        .chain(source.to_le_bytes())
        .chain(canvas_rect(src))
        .chain(canvas_rect(dst))
        .collect();
    (23, bytes)
}

fn canvas_fragment(scene: &mut Scene, id: NodeId, rect: Rect, bitmap_size: [u32; 2]) {
    let fragment = scene.add(ROOT, rect, PaintValues::default());
    fragment.content = FragmentContent::Canvas { id, bitmap_size };
    fragment.kind = FragmentKind::Replaced;
}

#[test]
fn canvas_readback_observes_the_requested_bitmap_version_and_pads_outside() {
    let Some(mut gpu) = gpu() else { return };
    let id = NodeId::new(2, 0).unwrap();
    let (send, messages) = squark_types::render::resource_channel();
    send.send(ResourceUpdate::ResizeCanvas { id, size: [3, 2] })
        .unwrap();
    let mut records = canvas_paint([20, 50, 100, 128]).to_vec();
    records.push((14, canvas_rect([0., 0., 3., 2.])));
    let drawn = send
        .send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&records),
        })
        .unwrap();
    let reset = send
        .send(ResourceUpdate::ResizeCanvas { id, size: [3, 2] })
        .unwrap();
    assert_eq!(gpu.drain_resources(&messages).unwrap(), reset);

    let old = gpu.read_canvas(id, drawn, Some(([-1, 1], [5, 2]))).unwrap();
    assert_eq!(old.size, [5, 2]);
    let after_reset = gpu.read_canvas(id, reset, Some(([-1, 1], [5, 2]))).unwrap();
    assert_eq!(after_reset.rgba, [0; 40]);
    assert_eq!(
        old.rgba.as_chunks::<4>().0,
        &[
            [0, 0, 0, 0],
            [20, 50, 100, 128],
            [20, 50, 100, 128],
            [20, 50, 100, 128],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
            [0, 0, 0, 0],
        ]
    );
}

#[test]
fn canvas_draws_ordered_sources_and_keeps_previous_prefixes() {
    let Some(mut gpu) = gpu() else { return };
    let id = NodeId::new(2, 0).unwrap();
    let copy = NodeId::new(3, 0).unwrap();
    let mut scene = Scene::new([40, 30], 1.0, solid(color(1.0, 0.0, 0.0)));
    canvas_fragment(&mut scene, id, Rect::new(5.0, 4.0, 10.0, 10.0), [10, 10]);
    canvas_fragment(&mut scene, copy, Rect::new(20.0, 4.0, 10.0, 10.0), [10, 10]);
    let (resources, inbox) = resource_channel();
    for id in [id, copy] {
        resources
            .send(ResourceUpdate::ResizeCanvas { id, size: [10, 10] })
            .unwrap();
    }
    let image = ImageKey(17);
    resources
        .send(ResourceUpdate::PutImage {
            key: image,
            bitmap: Bitmap {
                size: [2, 2],
                stride: 8,
                format: BitmapFormat::Rgba8SrgbPremultiplied,
                pixels: Arc::from([0, 0, 255, 255].repeat(4)),
            },
        })
        .unwrap();
    let old = resources
        .send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&[canvas_draw_image(
                0,
                image.0 as u32,
                [0.0, 0.0, 2.0, 2.0],
                [0.0, 0.0, 4.0, 10.0],
            )]),
        })
        .unwrap();
    let copied = resources
        .send(ResourceUpdate::SubmitCanvas {
            id: copy,
            stream: canvas_stream(&[canvas_draw_image(
                1,
                id.raw(),
                [0.0, 0.0, 10.0, 10.0],
                [0.0, 0.0, 10.0, 10.0],
            )]),
        })
        .unwrap();
    let [paint, set] = canvas_paint([0, 0, 255, 255]);
    let self_copy = resources
        .send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&[
                paint,
                set,
                (14, canvas_rect([4.0, 0.0, 1.0, 10.0])),
                canvas_draw_image(1, id.raw(), [0.0, 0.0, 5.0, 10.0], [3.0, 0.0, 5.0, 10.0]),
            ]),
        })
        .unwrap();
    let resized = resources
        .send(ResourceUpdate::ResizeCanvas { id, size: [10, 10] })
        .unwrap();
    let reset = resources
        .send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&[(14, canvas_rect([0.0, 0.0, 10.0, 10.0]))]),
        })
        .unwrap();
    let zero = resources
        .send(ResourceUpdate::ResizeCanvas { id, size: [0, 10] })
        .unwrap();
    let freed = resources
        .send(ResourceUpdate::Free {
            key: ResourceKey::Canvas(id),
        })
        .unwrap();
    assert_eq!(gpu.drain_resources(&inbox).unwrap(), freed);
    let black = [0, 0, 0, 255];
    for (prefix, original, right, copy) in [
        (ResourceSeq(0), RED, RED, RED),
        (old, BLUE, RED, RED),
        (copied, BLUE, RED, BLUE),
        (self_copy, BLUE, BLUE, BLUE),
        (resized, RED, RED, BLUE),
        (reset, black, black, BLUE),
        (zero, RED, RED, BLUE),
        (freed, RED, RED, BLUE),
    ] {
        let pixels = scene.render(&mut gpu, prefix).unwrap();
        assert_eq!(
            [pixels.at(6, 8), pixels.at(12, 8), pixels.at(21, 8)],
            [original, right, copy],
            "prefix {}",
            prefix.0
        );
    }
    gpu.retire_resources(freed).unwrap();
}

#[test]
fn retained_canvas_images_use_the_latest_ordered_bitmap_after_replacement() {
    let Some(mut gpu) = gpu() else { return };
    let source = NodeId::new(2, 0).unwrap();
    let destination = NodeId::new(3, 0).unwrap();
    let (send, updates) = resource_channel();
    for id in [source, destination] {
        send.send(ResourceUpdate::ResizeCanvas { id, size: [4, 4] })
            .unwrap();
    }
    let full = [0.0, 0.0, 4.0, 4.0];
    let paint = |color| {
        let [definition, select] = canvas_paint(color);
        send.send(ResourceUpdate::SubmitCanvas {
            id: source,
            stream: canvas_stream(&[definition, select, (14, canvas_rect(full))]),
        })
        .unwrap()
    };
    let copy = || {
        send.send(ResourceUpdate::SubmitCanvas {
            id: destination,
            stream: canvas_stream(&[
                (16, canvas_rect(full)),
                canvas_draw_image(1, source.raw(), full, full),
            ]),
        })
        .unwrap()
    };
    paint(RED);
    let first = copy();
    let retained = copy();
    paint(BLUE);
    let replaced = copy();
    send.send(ResourceUpdate::Free {
        key: ResourceKey::Canvas(source),
    })
    .unwrap();
    let freed = copy();
    assert_eq!(gpu.drain_resources(&updates).unwrap(), freed);
    for (prefix, color) in [
        (first, RED),
        (retained, RED),
        (replaced, BLUE),
        (freed, [0; 4]),
    ] {
        let pixels = gpu.read_canvas(destination, prefix, None).unwrap();
        assert_eq!(pixels.rgba, color.repeat(16), "prefix {prefix:?}");
    }
}

#[test]
fn retained_canvas_images_survive_intervening_patchless_canvas_renders() {
    let Some(mut gpu) = gpu() else { return };
    let source = NodeId::new(2, 0).unwrap();
    let other = NodeId::new(3, 0).unwrap();
    let destination = NodeId::new(4, 0).unwrap();
    let (send, updates) = resource_channel();
    for id in [source, other, destination] {
        send.send(ResourceUpdate::ResizeCanvas { id, size: [4, 4] })
            .unwrap();
    }
    let full = [0.0, 0.0, 4.0, 4.0];
    let paint = |id, color| {
        let [definition, select] = canvas_paint(color);
        send.send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&[definition, select, (14, canvas_rect(full))]),
        })
        .unwrap()
    };
    let copy = || {
        send.send(ResourceUpdate::SubmitCanvas {
            id: destination,
            stream: canvas_stream(&[
                (16, canvas_rect(full)),
                canvas_draw_image(1, source.raw(), full, full),
            ]),
        })
        .unwrap()
    };
    paint(source, RED);
    let first = copy();
    // A solid-path render on another canvas has no image patches and resets
    // Vello's persistent image atlas to 1x1. The cached source must still draw
    // when sampled again without being modified.
    paint(other, BLUE);
    let after_patchless = copy();
    assert_eq!(gpu.drain_resources(&updates).unwrap(), after_patchless);
    for (prefix, color) in [(first, RED), (after_patchless, RED)] {
        let pixels = gpu.read_canvas(destination, prefix, None).unwrap();
        assert_eq!(pixels.rgba, color.repeat(16), "prefix {prefix:?}");
    }
}

#[test]
fn canvas_filters_are_per_draw_and_restore_across_submits() {
    let Some(mut gpu) = gpu() else { return };
    let id = NodeId::new(2, 0).unwrap();
    let mut scene = Scene::new([32, 16], 1.0, PaintValues::default());
    canvas_fragment(&mut scene, id, Rect::new(0.0, 0.0, 24.0, 12.0), [24, 12]);
    let (resources, inbox) = resource_channel();
    resources
        .send(ResourceUpdate::ResizeCanvas { id, size: [24, 12] })
        .unwrap();
    let [paint, set] = canvas_paint(RED);
    let invert = [1u32.to_le_bytes(), 6u32.to_le_bytes(), 1f32.to_le_bytes()].concat();
    resources
        .send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&[
                paint,
                set,
                (14, canvas_rect([0.0, 0.0, 24.0, 12.0])),
                (1, vec![]),
                (28, invert),
            ]),
        })
        .unwrap();
    let unsupported = [1u32.to_le_bytes(), 900u32.to_le_bytes(), 1f32.to_le_bytes()].concat();
    let [paint, set] = canvas_paint(BLUE);
    let seq = resources
        .send(ResourceUpdate::SubmitCanvas {
            id,
            stream: canvas_stream(&[
                (28, unsupported),
                paint,
                set,
                (14, canvas_rect([0.0, 0.0, 6.0, 8.0])),
                (1, vec![]),
                (3, vec![]),
                (
                    4,
                    [2.0f64, 2.0]
                        .into_iter()
                        .flat_map(f64::to_le_bytes)
                        .collect(),
                ),
                (
                    5,
                    [4.0f64, 2.0]
                        .into_iter()
                        .flat_map(f64::to_le_bytes)
                        .collect(),
                ),
                (
                    5,
                    [4.0f64, 4.0]
                        .into_iter()
                        .flat_map(f64::to_le_bytes)
                        .collect(),
                ),
                (
                    5,
                    [2.0f64, 4.0]
                        .into_iter()
                        .flat_map(f64::to_le_bytes)
                        .collect(),
                ),
                (8, vec![]),
                (13, vec![0; 8]),
                (16, canvas_rect([0.0, 0.0, 24.0, 12.0])),
                (2, vec![]),
                (2, vec![]),
                (14, canvas_rect([6.0, 0.0, 6.0, 8.0])),
            ]),
        })
        .unwrap();
    assert_eq!(gpu.drain_resources(&inbox).unwrap(), seq);
    let pixels = scene.render(&mut gpu, seq).unwrap();
    assert_eq!(pixels.at(1, 1), [255, 255, 0, 255]);
    assert_eq!(pixels.at(3, 3), WHITE);
    assert_eq!(pixels.at(7, 1), RED);
    assert_eq!(pixels.at(20, 1), RED);
}

#[test]
fn retained_tiles_rasterize_only_damage_and_match_full_rendering() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([1100, 650], 1.0, solid(color(0.9, 0.9, 0.9)));
    let changed = scene
        .add(
            ROOT,
            Rect::new(39.0, 63.0, 40.0, 80.0),
            solid(color(1.0, 0.0, 0.0)),
        )
        .id;
    // Projected rounded edges cross both tile axes. Their coverage must use
    // the same root coordinates and pixel footprint as the full scene.
    let mut rounded = solid(color(0.0, 0.0, 1.0));
    rounded.box_paint.radii = [position(0.1, 0.1); 4];
    let (sin, cos) = 0.14_f32.sin_cos();
    rounded.properties.transform = Box::new([Transform::Matrix([
        [cos, sin, 0.0, 0.0],
        [-sin, cos, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.25, 0.125, 0.0, 1.0],
    ])]);
    scene.add(ROOT, Rect::new(442.0, 440.0, 300.0, 170.0), rounded);
    let original = scene.commit(ResourceSeq(0));
    assert_tiles_equal(&mut gpu, &original);
    assert_eq!(
        gpu.tile_stats(),
        TileStats {
            rasterized: 6,
            shifted: 0,
            reused: 0
        }
    );
    gpu.render_cached(&original).unwrap();
    assert_eq!(gpu.tile_stats().reused, 6);

    Arc::make_mut(&mut scene.tree.nodes.get_mut(&changed).unwrap().style)
        .art
        .background_color = color(0.0, 1.0, 0.0);
    let mut next = scene.commit(ResourceSeq(0));
    next.id = FrameId(2);
    next.frame.damage = frame_damage(&original, next.viewport, &next.frame);
    assert_tiles_equal(&mut gpu, &next);
    assert_eq!(
        gpu.tile_stats(),
        TileStats {
            rasterized: 1,
            shifted: 0,
            reused: 5
        }
    );

    let mut skipped = original;
    skipped.id = FrameId(4);
    skipped.frame.damage = Damage::Rects {
        base: FrameId(3),
        rects: Box::new([]),
    };
    assert_tiles_equal(&mut gpu, &skipped);
    assert_eq!(gpu.tile_stats().rasterized, 6);
}

#[test]
fn rectangular_static_scroll_reuses_tiles_above_the_exposed_strip() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([1100, 650], 1.0, solid(color(0.9, 0.9, 0.9)));
    let root = scene.tree.nodes.get_mut(&ROOT).unwrap();
    root.scroll = Some(ScrollFrame {
        owner: root.owner,
        offset: Point::default(),
        generation: 0,
        scrollport: root.border_box,
        content: Rect::new(0.0, 0.0, 1100.0, 800.0),
        gutter: Default::default(),
        scroll_x: false,
        scroll_y: true,
        clip_x: true,
        clip_y: true,
    });
    for y in [10.0, 500.0, 538.0, 643.0, 667.0] {
        scene.add(
            ROOT,
            Rect::new(47.0, y, 1050.0, 18.0),
            solid(color(0.8, 0.1, 0.3)),
        );
    }
    let mut frame = scene.commit(ResourceSeq(0));
    gpu.render_cached(&frame).unwrap();
    frame.frame.property_trees.scrolls[1].initial_offset.y = 40.0;
    frame.frame.property_trees.scrolls[1].generation = 1;
    assert_tiles_equal(&mut gpu, &frame);
    assert_eq!(
        gpu.tile_stats(),
        TileStats {
            rasterized: 3,
            shifted: 3,
            reused: 0
        }
    );

    frame.frame.property_trees.scrolls[1].initial_offset.y = 40.5;
    frame.frame.property_trees.scrolls[1].generation = 2;
    assert_tiles_equal(&mut gpu, &frame);
    assert_eq!(gpu.tile_stats().rasterized, 6);
}

#[test]
fn filter_composition_applies_the_effect_clip_after_blur() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([37, 29], 1.0, PaintValues::default());
    let mut filtered = solid(PaintColor::BLACK);
    filtered.properties.filters = Box::new([Filter::Blur(2.0)]);
    filtered.properties.clip = Some(Clip {
        reference_box: GeometryBox::Border,
        shape: ClipShape::Inset {
            edges: Edges {
                left: length(10.0),
                ..Default::default()
            },
            radii: Default::default(),
        },
    });
    scene.add(ROOT, Rect::new(3.0, 4.0, 26.0, 21.0), filtered);
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    assert_eq!(rgba.at(12, 13), WHITE);
    assert_eq!(rgba.at(13, 13), [0, 0, 0, 255]);
    assert_ne!(rgba.at(28, 13), [0, 0, 0, 255]);
    assert_eq!(rgba.at(29, 13), WHITE);
}

#[test]
fn crossing_preserved_planes_draw_in_css_paint_order() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([43, 35], 1.0, PaintValues::default());
    let mut preserve = PaintValues::default();
    preserve.properties.preserve_3d = true;
    let group = scene
        .add(ROOT, Rect::new(0.0, 0.0, 35.0, 25.0), preserve)
        .id;
    let mut flat = solid(color(1.0, 0.0, 0.0));
    flat.properties.transform = Box::new([scale(1.0, 1.0)]);
    scene.add(group, Rect::new(2.0, 4.0, 20.0, 10.0), flat);
    let mut crossing = solid(color(0.0, 0.0, 1.0));
    crossing.properties.transform_origin = position(0.0, 0.0);
    crossing.properties.transform = Box::new([Transform::Matrix([
        [1.0, 0.0, 1.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, -12.0, 1.0],
    ])]);
    scene.add(group, Rect::new(5.0, 5.0, 20.0, 10.0), crossing);
    assert_eq!(
        scene.render(&mut gpu, ResourceSeq(0)).unwrap().at(6, 6),
        BLUE
    );
    assert_eq!(
        scene.render(&mut gpu, ResourceSeq(0)).unwrap().at(6, 6),
        BLUE
    );
}

#[test]
fn shared_fractional_box_edges_snap_to_physical_pixels() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([37, 30], 1.5, PaintValues::default());
    let a = Rect::new(2.0, 2.0, 5.2, 7.2);
    let b = Rect::new(a.max_x(), 2.0, 6.0, 7.2);
    scene.add(ROOT, a, solid(color(1.0, 0.0, 0.0)));
    let mut bordered = solid(color(0.0, 0.0, 1.0));
    bordered.box_paint.border.bottom = BorderSide {
        style: BorderStyle::Solid,
        color: color(1.0, 0.0, 0.0),
    };
    scene.add(ROOT, b, bordered).border.bottom = 2.0 / 1.5;
    let mut rounded = solid(color(1.0, 0.0, 0.0));
    rounded.box_paint.radii = [position(0.25, 0.25); 4];
    scene.add(ROOT, Rect::new(2.3, 11.0, 6.0, 7.0), rounded);
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    assert_eq!(rgba.at(10, 6), RED);
    assert_eq!(rgba.at(11, 6), BLUE);
    assert_eq!(rgba.at(15, 11), BLUE);
    assert_eq!(rgba.at(15, 12), RED);
    assert_eq!(rgba.at(15, 13), RED);
    assert_eq!(rgba.at(15, 14), WHITE);
    assert_eq!(rgba.at(2, 21), WHITE);
    assert_eq!(rgba.at(3, 21), RED);
}

#[test]
fn square_borders_fit_dashes_and_share_corner_dots() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([110, 83], 1.0, PaintValues::default());
    for (y, style, width) in [
        (2.0, BorderStyle::Dashed, 10.0),
        (44.0, BorderStyle::Dotted, 6.0),
    ] {
        let side = BorderSide {
            style,
            color: color(1.0, 0.0, 0.0),
        };
        let mut paint = PaintValues::default();
        paint.box_paint.border = Edges {
            top: side,
            right: side,
            bottom: side,
            left: side,
        };
        scene
            .add(ROOT, Rect::new(2.0, y, 104.0, 36.0), paint)
            .border = Edges {
            top: width,
            right: width,
            bottom: width,
            left: width,
        };
    }

    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    assert_eq!(rgba.at(21, 6), RED);
    assert_eq!(rgba.at(22, 6), WHITE);
    assert_eq!(rgba.at(29, 6), WHITE);
    assert_eq!(rgba.at(30, 6), RED);
    assert_eq!(rgba.at(4, 46), RED);
    assert_eq!(rgba.at(9, 46), WHITE);
    assert_eq!(rgba.at(17, 46), RED);
}

#[test]
fn fractional_rounded_borders_snap_to_single_device_rows() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([68, 52], 1.0, PaintValues::default());
    let mut button = solid(color(1.0, 0.0, 0.0));
    button.box_paint.radii = [Position2D {
        x: length(8.0),
        y: length(8.0),
    }; 4];
    let side = BorderSide {
        style: BorderStyle::Solid,
        color: color(0.0, 0.0, 1.0),
    };
    button.box_paint.border = Edges {
        top: side,
        right: side,
        bottom: side,
        left: side,
    };
    let bounds = Rect::new(9.2, 4.4, 48.0, 29.0);
    let fragment = scene.add(ROOT, bounds, button);
    fragment.border = Edges {
        top: 1.0,
        right: 1.0,
        bottom: 1.0,
        left: 1.0,
    };
    let visible = scene.commit(ResourceSeq(0));
    let hit = squark_paint::HitSnapshot::new(&visible);
    assert!(squark_paint::hit_test(&hit, point(31.0, 4.2)).is_none());
    assert!(squark_paint::hit_test(&hit, point(31.0, 4.5)).is_some());
    let image = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    assert_eq!(image.at(31, 3), WHITE);
    assert_eq!(image.at(31, 4), BLUE);
    assert_eq!(image.at(31, 5), RED);
    assert_eq!(image.at(31, 31), RED);
    assert_eq!(image.at(31, 32), BLUE);
    assert_eq!(image.at(31, 33), WHITE);
}

#[test]
fn box_edges_and_clips_round_half_pixels_up_without_moving_hits() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([68, 52], 1.0, PaintValues::default());
    let side = BorderSide {
        style: BorderStyle::Solid,
        color: color(0.0, 0.0, 1.0),
    };
    let mut paint = solid(color(1.0, 0.0, 0.0));
    paint.box_paint.radii = [Position2D {
        x: length(6.0),
        y: length(6.0),
    }; 4];
    paint.box_paint.border.left = side;
    paint.box_paint.border.right = side;
    let fragment = scene.add(ROOT, Rect::new(9.5, 4.0, 48.0, 24.0), paint);
    fragment.border.left = 1.0;
    fragment.border.right = 1.0;
    let mut clipped = solid(color(1.0, 0.0, 0.0));
    clipped.properties.clip = Some(Clip {
        reference_box: GeometryBox::Border,
        shape: ClipShape::Inset {
            edges: Edges {
                left: length(0.5),
                right: length(0.5),
                ..Default::default()
            },
            radii: Default::default(),
        },
    });
    scene.add(ROOT, Rect::new(9.0, 30.0, 49.0, 15.0), clipped);
    let frame = scene.commit(ResourceSeq(0));
    let hit = squark_paint::HitSnapshot::new(&frame);
    for y in [17.0, 37.0] {
        assert!(squark_paint::hit_test(&hit, point(9.25, y)).is_none());
        assert!(squark_paint::hit_test(&hit, point(9.75, y)).is_some());
    }
    let pixels = scene.render(&mut gpu, ResourceSeq(0)).unwrap();
    assert_eq!(
        [9, 10, 11, 56, 57, 58].map(|x| pixels.at(x, 17)),
        [WHITE, BLUE, RED, RED, BLUE, WHITE,]
    );
    assert_eq!(
        [9, 10, 57, 58].map(|x| pixels.at(x, 37)),
        [WHITE, RED, RED, WHITE]
    );
}

#[test]
fn scaled_replaced_images_match_software_bilinear_weights() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([20, 28], 1.0, PaintValues::default());
    let key = ImageKey(1);
    for (i, image_rendering) in [
        ImageRendering::Auto,
        ImageRendering::Smooth,
        ImageRendering::HighQuality,
        ImageRendering::Pixelated,
        ImageRendering::CrispEdges,
    ]
    .into_iter()
    .enumerate()
    {
        let paint = PaintValues {
            image_rendering,
            ..PaintValues::default()
        };
        scene
            .add(ROOT, Rect::new(2.0, (i * 5 + 2) as f32, 13.0, 3.0), paint)
            .content = FragmentContent::Image {
            key,
            intrinsic_size: Size {
                width: 2.0,
                height: 1.0,
            },
        };
    }
    let (sender, receiver) = resource_channel();
    let prefix = sender
        .send(ResourceUpdate::PutImage {
            key,
            bitmap: Bitmap {
                size: [2, 1],
                stride: 8,
                format: BitmapFormat::Rgba8SrgbPremultiplied,
                pixels: [255, 0, 0, 255, 0, 0, 255, 255].into(),
            },
        })
        .unwrap();

    gpu.drain_resources(&receiver).unwrap();
    let rgba = scene.render(&mut gpu, prefix).unwrap();
    for y in [3, 8, 13] {
        rgba.near(7, y, [175, 0, 80, 255], 1);
    }
    for y in [18, 23] {
        assert_eq!(rgba.at(7, y), RED);
        assert_eq!(rgba.at(9, y), BLUE);
    }
}

#[test]
fn text_shadows_use_glyph_coverage_and_draw_the_first_shadow_on_top() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([35, 22], 1.0, PaintValues::default());
    let mut style = PaintValues::default();
    style.art.color = PaintColor::TRANSPARENT;
    let shadow = Shadow {
        offset: point(9.0, 4.0),
        blur: 0.0,
        spread: 0.0,
        color: color(1.0, 0.0, 0.0),
        inset: false,
    };
    style.art.text_shadows = Box::new([
        shadow,
        Shadow {
            color: color(0.0, 0.0, 1.0),
            ..shadow
        },
    ]);
    let fragment = scene.add(ROOT, Rect::new(2.0, 2.0, 10.0, 10.0), style);
    fragment.kind = FragmentKind::Text;
    let position = point(4.0, 8.0);
    let font = FontInstanceId(1);
    fragment.content = FragmentContent::Text(Box::new([PositionedRun {
        font,
        baseline: 8.0,
        bidi_level: 0,
        source_ranges: Box::new([]),
        decoration_spans: Box::new([]),
        glyphs: Box::new([PositionedGlyph {
            glyph_id: 1,
            position,
            cluster: 0,
        }]),
    }]));
    let (sender, receiver) = resource_channel();
    let prefix = sender
        .send(ResourceUpdate::PutGlyph {
            key: GlyphKey::for_position(font, 1, position, 1.0),
            bitmap: Bitmap {
                size: [3, 3],
                stride: 3,
                format: BitmapFormat::R8Coverage,
                pixels: [255; 9].into(),
            },
            bearing: [0, 3],
        })
        .unwrap();
    gpu.drain_resources(&receiver).unwrap();
    let rgba = scene.render(&mut gpu, prefix).unwrap();
    assert_eq!(rgba.at(5, 6), WHITE);
    assert_eq!(rgba.at(14, 10), RED);
    assert_eq!(rgba.at(16, 10), WHITE);
}

#[test]
fn a_frame_is_submitted_only_after_the_previous_frame_finishes_on_the_gpu() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([64, 48], 1.0, solid(color(1.0, 1.0, 1.0)));
    let mut group = PaintValues::default();
    group.properties.opacity = 0.5;
    let parent = scene.add(ROOT, Rect::new(4.0, 4.0, 40.0, 30.0), group).id;
    scene.add(
        parent,
        Rect::new(8.0, 8.0, 20.0, 16.0),
        solid(color(1.0, 0.0, 0.0)),
    );
    let effects = scene.commit(ResourceSeq(0));
    let tiled = Scene::new([64, 48], 1.0, solid(color(1.0, 0.0, 0.0))).commit(ResourceSeq(0));
    // Isolated effects take the full scene pass; a plain scene takes the tiles.
    for frame in [&effects, &tiled] {
        gpu.render_cached(frame).unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        gpu.queue()
            .on_submitted_work_done(move || flag.store(true, Ordering::SeqCst));
        let mut next = frame.clone();
        next.viewport.epoch += 1;
        gpu.render_cached(&next).unwrap();
        assert!(
            finished.load(Ordering::SeqCst),
            "the previous frame was still on the GPU when the next was submitted"
        );
    }
}

/// Add an `overflow: hidden` box, whose rectangular clip gives the batches
/// inside it a scissor smaller than the viewport.
fn clip_box(scene: &mut Scene, parent: FragmentId, rect: Rect, style: PaintValues) -> FragmentId {
    let mut style = style;
    style.box_paint.overflow = [Overflow::Hidden, Overflow::Hidden];
    let clipper = scene.add(parent, rect, style);
    clipper.scroll = Some(ScrollFrame {
        owner: clipper.owner,
        offset: Point::default(),
        generation: 0,
        scrollport: rect,
        content: rect,
        gutter: Edges::default(),
        scroll_x: false,
        scroll_y: false,
        clip_x: true,
        clip_y: true,
    });
    clipper.id
}

#[test]
fn clipped_opacity_groups_composite_inside_their_clip_and_keep_nested_blur() {
    let Some(mut gpu) = gpu() else { return };
    let mut scene = Scene::new([64, 48], 1.0, PaintValues::default());
    // A plain opacity group whose content sits under a clip inside it: its
    // batches' scissor is that clip, so the group composites inside that
    // smaller area, right up to the clip edge.
    let mut half = PaintValues::default();
    half.properties.opacity = 0.5;
    let group = scene
        .add(ROOT, Rect::new(2.0, 2.0, 26.0, 24.0), half.clone())
        .id;
    let clip = clip_box(
        &mut scene,
        group,
        Rect::new(4.0, 4.0, 20.0, 16.0),
        PaintValues::default(),
    );
    scene.add(
        clip,
        Rect::new(6.0, 6.0, 40.0, 30.0),
        solid(PaintColor::BLACK),
    );
    // An opacity group holding a blurred box whose only content sits under
    // the box's own clip. The blur spreads past that clip, which is the only
    // scissor the group's batches have.
    let group = scene.add(ROOT, Rect::new(30.0, 4.0, 30.0, 40.0), half).id;
    let mut blurred = PaintValues::default();
    blurred.properties.filters = Box::new([Filter::Blur(2.0)]);
    let blurred = clip_box(
        &mut scene,
        group,
        Rect::new(38.0, 16.0, 12.0, 12.0),
        blurred,
    );
    scene.add(
        blurred,
        Rect::new(38.0, 16.0, 12.0, 12.0),
        solid(PaintColor::BLACK),
    );
    let rgba = scene.render(&mut gpu, ResourceSeq(0)).unwrap();

    assert_eq!(rgba.at(5, 5), WHITE);
    rgba.near(6, 6, [127, 127, 127, 255], 1);
    rgba.near(23, 19, [127, 127, 127, 255], 1);
    assert_eq!(rgba.at(24, 19), WHITE);
    assert_eq!(rgba.at(23, 20), WHITE);

    rgba.near(44, 22, [127, 127, 127, 255], 2);
    assert_ne!(rgba.at(36, 22), WHITE, "the blur is cut at the box's clip");
    assert_ne!(rgba.at(44, 14), WHITE, "the blur is cut at the box's clip");
    assert_eq!(rgba.at(31, 5), WHITE);
}
