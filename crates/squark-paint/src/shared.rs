// SPDX-License-Identifier: MIT
//! Resolve the engine snapshot into the inputs used by display-list construction.

use std::sync::Arc;

use kurbo::Shape;
use squark_types::{
    NodeId, Point, Rect, Size, fragment as css,
    host::ListenerFlags,
    render::{GlyphKey, PaintError, ResourceSeq},
};

use crate::{
    BorderSide, BorderStyle, ClipShape, ColorSpace, FillRule, Filter, Frame, HitTestTree,
    PaintColor, PathPaint, PropertyTrees, RoundedRect, Shadow, input,
    properties::{IDENTITY, multiply, translation},
};

pub fn build_frame(
    fragments: &css::FragmentTree,
    mirror: &dyn css::MirrorRead,
    resource_updates: ResourceSeq,
    dpr: f64,
) -> Result<Frame, PaintError> {
    let mut frame = crate::build_resolved_frame(
        &project(fragments, mirror, dpr),
        &Mirror(mirror),
        resource_updates,
    )?;
    frame.ime_area = fragments.ime_area;
    Ok(frame)
}

pub fn build_hit_test(
    fragments: &css::FragmentTree,
    mirror: &dyn css::MirrorRead,
    dpr: f64,
) -> Result<(HitTestTree, PropertyTrees), PaintError> {
    crate::build_resolved_hit_test(&project(fragments, mirror, dpr), &Mirror(mirror))
}

struct Mirror<'a>(&'a dyn css::MirrorRead);

impl input::MirrorRead for Mirror<'_> {
    fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.0.node(id)?.links.parent
    }

    fn is_element(&self, id: NodeId) -> bool {
        self.0
            .node(id)
            .is_some_and(|node| node.kind == css::NodeKind::Element)
    }

    fn listeners(&self, id: NodeId) -> ListenerFlags {
        if id.is_none() {
            self.0.window_listeners()
        } else {
            self.0.node(id).map_or_default(|node| node.listeners)
        }
    }
}

fn project(
    tree: &css::FragmentTree,
    mirror: &dyn css::MirrorRead,
    dpr: f64,
) -> input::FragmentTree {
    let nodes = tree
        .nodes
        .iter()
        .map(|(&id, fragment)| {
            (
                id,
                input::Fragment {
                    id: fragment.id,
                    owner: fragment.owner,
                    kind: fragment.kind,
                    parent: fragment.parent,
                    containing_block: fragment.containing_block,
                    children: fragment.children.clone(),
                    border_box: fragment.border_box,
                    padding: edges(fragment.padding),
                    border: edges(fragment.border),
                    scroll: fragment.scroll.map(|frame| input::ScrollFrame {
                        owner: frame.owner,
                        scrollport: frame.scrollport,
                        content: frame.content,
                        offset: frame.offset,
                        generation: frame.generation,
                        gutter: edges(frame.gutter),
                        scroll_x: frame.scroll_x,
                        scroll_y: frame.scroll_y,
                        clip_x: frame.clip_x,
                        clip_y: frame.clip_y,
                        wheel_axes: fragment.style.box_paint.overflow.map(|overflow| {
                            if fragment.kind == css::FragmentKind::Viewport {
                                !matches!(overflow, css::Overflow::Hidden | css::Overflow::Clip)
                            } else {
                                matches!(overflow, css::Overflow::Auto | css::Overflow::Scroll)
                            }
                        }),
                        force_scrollbars: fragment
                            .style
                            .box_paint
                            .overflow
                            .map(|overflow| overflow == css::Overflow::Scroll),
                    }),
                    sticky: fragment.sticky.map(|constraints| input::StickyConstraints {
                        scroll_owner: constraints.scroll_owner,
                        containing_rect: constraints.containing_rect,
                        insets: edges(constraints.insets),
                    }),
                    content: content(fragment, mirror, dpr),
                    style: Arc::new(style(fragment)),
                },
            )
        })
        .collect();
    input::FragmentTree {
        root: tree.root,
        nodes,
    }
}

fn edges<T>(value: css::Edges<T>) -> [T; 4] {
    [value.top, value.right, value.bottom, value.left]
}

pub fn resolve_length(value: css::LengthPercentage, reference: f32) -> f32 {
    value.px + value.fraction * reference
}

pub fn resolve_position(position: css::Position2D, size: Size) -> Point {
    Point {
        x: resolve_length(position.x, size.width),
        y: resolve_length(position.y, size.height),
    }
}

fn radii(corners: [css::Position2D; 4], size: Size) -> [Size; 4] {
    corners.map(|corner| {
        let corner = resolve_position(corner, size);
        Size {
            width: corner.x,
            height: corner.y,
        }
    })
}

fn style(fragment: &css::Fragment) -> input::PaintValues {
    use css::DisplayKind as D;
    let style = &fragment.style;
    let stacking = &style.stacking;
    let box_paint = &style.box_paint;
    let art = &style.art;
    let props = &style.properties;
    let display = match stacking.display {
        D::None => input::Display::None,
        D::Inline | D::Contents => input::Display::Inline,
        D::InlineBlock | D::InlineFlex | D::InlineGrid => input::Display::InlineBlock,
        _ => input::Display::Block,
    };
    let border = fragment.border_box;
    let reference = reference_box(fragment, props.transform_box);
    let origin = resolve_position(props.transform_origin, reference.size);
    let perspective = props.perspective.map(|distance| {
        let origin = resolve_position(props.perspective_origin, border.size);
        let x = border.origin.x + origin.x;
        let y = border.origin.y + origin.y;
        let mut matrix = IDENTITY;
        matrix[2][3] = -1.0 / distance.max(1.0);
        multiply(
            multiply(translation(x, y, 0.0), matrix),
            translation(-x, -y, 0.0),
        )
    });
    input::PaintValues {
        display,
        position: stacking.position,
        z_index: stacking.z_index,
        floating: stacking.floating,
        flex_or_grid_item: stacking.flex_or_grid_item,
        top_layer: stacking.top_layer_order,
        isolation: stacking.isolation,
        stacking_containment: stacking.containment.layout || stacking.containment.paint,
        visible: stacking.visibility == css::Visibility::Visible,
        background: color(art.background_color),
        background_propagated: box_paint.background_propagated,
        background_clip: area(box_paint.background_clip),
        background_layers: box_paint
            .backgrounds
            .iter()
            .map(|layer| input::BackgroundLayer {
                area: area(layer.clip),
                origin: reference_box(fragment, layer.origin),
                layer: layer.clone(),
            })
            .collect(),
        radii: radii(box_paint.radii, border.size),
        border: std::array::from_fn(|i| {
            border_side(edges(box_paint.border)[i], edges(fragment.border)[i])
        }),
        outline: (box_paint.outline.width > 0.0).then(|| input::Outline {
            side: border_side(box_paint.outline.side, box_paint.outline.width),
            offset: box_paint.outline.offset,
        }),
        shadows: art.box_shadows.iter().copied().map(shadow).collect(),
        text_color: color(art.color),
        text_shadows: art.text_shadows.iter().copied().map(shadow).collect(),
        image_sampling: match style.image_rendering {
            css::ImageRendering::Pixelated | css::ImageRendering::CrispEdges => {
                crate::SamplingMode::Nearest
            }
            css::ImageRendering::Auto
            | css::ImageRendering::Smooth
            | css::ImageRendering::HighQuality => crate::SamplingMode::Linear,
        },
        object_fit: art.object_fit,
        object_position: Point {
            x: art.object_position.x.fraction,
            y: art.object_position.y.fraction,
        },
        object_position_offset: Point {
            x: art.object_position.x.px,
            y: art.object_position.y.px,
        },
        transform: (!props.transform.is_empty())
            .then(|| resolve_transforms(&props.transform, reference.size)),
        transform_origin: [
            reference.origin.x - border.origin.x + origin.x,
            reference.origin.y - border.origin.y + origin.y,
            props.transform_origin_z,
        ],
        transform_reference: reference.size,
        animations: Arc::from(&*props.animations),
        perspective,
        preserve_3d: props.preserve_3d,
        clip: props
            .clip
            .as_ref()
            .map(|clip| clip_shape(clip, reference_box(fragment, clip.reference_box))),
        opacity: props.opacity,
        filters: props.filters.iter().copied().map(filter).collect(),
        backdrop: props.backdrop_filters.iter().copied().map(filter).collect(),
        blend: props.blend,
        overscroll: box_paint.overscroll,
        pointer_events: style.interaction.pointer_events != css::PointerEvents::None,
        cursor: style.interaction.cursor,
    }
}

fn content(
    fragment: &css::Fragment,
    mirror: &dyn css::MirrorRead,
    dpr: f64,
) -> input::FragmentContent {
    match &fragment.content {
        css::FragmentContent::None => input::FragmentContent::None,
        css::FragmentContent::Canvas { id, bitmap_size } => input::FragmentContent::Canvas {
            id: *id,
            bitmap_size: *bitmap_size,
        },
        css::FragmentContent::Image {
            key,
            intrinsic_size,
        } => input::FragmentContent::Image {
            key: *key,
            intrinsic_size: *intrinsic_size,
        },
        css::FragmentContent::Text(runs) => input::FragmentContent::Text(
            runs.iter()
                .map(|run| {
                    let decoration = |span: &css::DecorationSpan| {
                        (
                            Rect::new(span.x[0], span.y, span.x[1] - span.x[0], span.thickness),
                            color(span.color),
                        )
                    };
                    input::PositionedRun {
                        glyphs: run
                            .glyphs
                            .iter()
                            .map(|glyph| {
                                (
                                    GlyphKey::for_position(
                                        run.font,
                                        glyph.glyph_id,
                                        glyph.position,
                                        dpr,
                                    ),
                                    glyph.position,
                                )
                            })
                            .collect(),
                        under_and_overlines: run
                            .decoration_spans
                            .iter()
                            .filter(|span| span.kind != css::DecorationKind::LineThrough)
                            .map(decoration)
                            .collect(),
                        line_through: run
                            .decoration_spans
                            .iter()
                            .filter(|span| span.kind == css::DecorationKind::LineThrough)
                            .map(decoration)
                            .collect(),
                    }
                })
                .collect(),
        ),
        css::FragmentContent::Svg(svg) => {
            let reference = reference_box(fragment, css::GeometryBox::Content);
            let origin = svg_paint_origin(fragment, mirror, reference.origin);
            input::FragmentContent::Svg(
                svg.paths
                    .iter()
                    .map(|svg| {
                        let path = path(&svg.commands, Point::default(), false);
                        let bounds = path.bounding_box();
                        let bounds = Rect::new(
                            bounds.x0 as f32,
                            bounds.y0 as f32,
                            bounds.width() as f32,
                            bounds.height() as f32,
                        );
                        let matrix = multiply(translation(origin.x, origin.y, 0.0), svg.transform);
                        input::SvgPath {
                            paint: PathPaint {
                                path: Arc::new(path),
                                fill: svg.fill.as_ref().map(|paint| {
                                    (svg_brush(paint, bounds), fill_rule(svg.fill_rule))
                                }),
                                stroke: svg.stroke.as_ref().map(|stroke| {
                                    (svg_brush(&stroke.paint, bounds), svg_stroke(stroke))
                                }),
                            },
                            clip: svg.clip.as_ref().map(|clip| {
                                clip_shape(
                                    clip,
                                    Rect::new(
                                        0.0,
                                        0.0,
                                        reference.size.width,
                                        reference.size.height,
                                    ),
                                )
                            }),
                            transform: (matrix != IDENTITY).then_some(matrix),
                        }
                    })
                    .collect(),
            )
        }
    }
}

fn svg_paint_origin(
    fragment: &css::Fragment,
    mirror: &dyn css::MirrorRead,
    mut origin: Point,
) -> Point {
    let name = mirror.node(fragment.owner).and_then(|node| node.name);
    let is_svg_root = name.is_some_and(|name| {
        let matches = |atom, expected: &str| {
            mirror
                .atom(atom)
                .is_some_and(|value| value.iter().copied().eq(expected.encode_utf16()))
        };
        matches(name.local, "svg")
            && name
                .namespace
                .is_some_and(|ns| matches(ns, "http://www.w3.org/2000/svg"))
    });
    if is_svg_root {
        // Blink chooses the SVG viewport's origin in CSS pixels before applying
        // CSS transforms. Keep the content inset from that border-box origin.
        let border = fragment.border_box.origin;
        let inset = |edge: f32| (f64::from(edge) + 0.5).floor() as f32 - edge;
        origin.x += inset(border.x);
        origin.y += inset(border.y);
    }
    origin
}

fn svg_brush(paint: &css::SvgPaint, bounds: Rect) -> crate::PathBrush {
    match paint {
        css::SvgPaint::Color(value) => crate::PathBrush::Color(color(*value)),
        css::SvgPaint::Gradient(gradient) => {
            crate::PathBrush::Gradient(Arc::new(crate::resolve_gradient(gradient, bounds)))
        }
    }
}

fn svg_stroke(style: &css::SvgStroke) -> kurbo::Stroke {
    let cap = match style.line_cap {
        css::LineCap::Butt => kurbo::Cap::Butt,
        css::LineCap::Round => kurbo::Cap::Round,
        css::LineCap::Square => kurbo::Cap::Square,
    };
    let join = match style.line_join {
        css::LineJoin::Miter => kurbo::Join::Miter,
        css::LineJoin::Round => kurbo::Join::Round,
        css::LineJoin::Bevel => kurbo::Join::Bevel,
    };
    kurbo::Stroke::new(f64::from(style.width))
        .with_caps(cap)
        .with_join(join)
        .with_miter_limit(f64::from(style.miter_limit))
        .with_dashes(
            f64::from(style.dash_offset),
            style.dash_array.iter().map(|&length| f64::from(length)),
        )
}

fn reference_box(fragment: &css::Fragment, area: css::GeometryBox) -> Rect {
    let shape = RoundedRect::new(fragment.border_box, [Size::default(); 4]);
    match area {
        css::GeometryBox::Padding => shape.inset(edges(fragment.border)).rect,
        css::GeometryBox::Content | css::GeometryBox::Fill | css::GeometryBox::View => {
            shape
                .inset(edges(fragment.border))
                .inset(edges(fragment.padding))
                .rect
        }
        css::GeometryBox::Border | css::GeometryBox::Stroke => shape.rect,
    }
}

fn area(clip: css::BackgroundClip) -> input::BoxArea {
    match clip {
        css::BackgroundClip::Border => input::BoxArea::Border,
        css::BackgroundClip::Padding => input::BoxArea::Padding,
        css::BackgroundClip::Content => input::BoxArea::Content,
        css::BackgroundClip::Text => input::BoxArea::Text,
    }
}

fn clip_shape(clip: &css::Clip, reference: Rect) -> ClipShape {
    match &clip.shape {
        css::ClipShape::Box => ClipShape::Rect(reference),
        css::ClipShape::Inset {
            edges: inset,
            radii: corners,
        } => {
            let reference = RoundedRect::new(reference, [Size::default(); 4])
                .inset([
                    resolve_length(inset.top, reference.size.height),
                    resolve_length(inset.right, reference.size.width),
                    resolve_length(inset.bottom, reference.size.height),
                    resolve_length(inset.left, reference.size.width),
                ])
                .rect;
            ClipShape::RoundedRect(RoundedRect::new(reference, radii(*corners, reference.size)))
        }
        css::ClipShape::Ellipse { center, radii } => {
            let center = resolve_position(*center, reference.size);
            let radii = resolve_position(*radii, reference.size);
            let ellipse = kurbo::Ellipse::new(
                (
                    f64::from(reference.origin.x + center.x),
                    f64::from(reference.origin.y + center.y),
                ),
                (f64::from(radii.x), f64::from(radii.y)),
                0.0,
            );
            ClipShape::Path(Arc::new(ellipse.to_path(0.01)), FillRule::NonZero)
        }
        css::ClipShape::Polygon {
            points,
            fill_rule: rule,
        } => {
            let commands: Vec<_> = points
                .iter()
                .enumerate()
                .map(|(i, &point)| {
                    let point = resolve_position(point, reference.size);
                    if i == 0 {
                        css::PathCommand::Move(point)
                    } else {
                        css::PathCommand::Line(point)
                    }
                })
                .collect();
            ClipShape::Path(
                Arc::new(path(&commands, reference.origin, true)),
                fill_rule(*rule),
            )
        }
        css::ClipShape::Path {
            commands,
            fill_rule: rule,
        } => ClipShape::Path(
            Arc::new(path(commands, reference.origin, true)),
            fill_rule(*rule),
        ),
    }
}

fn path(commands: &[css::PathCommand], origin: Point, close: bool) -> kurbo::BezPath {
    let convert = |p: Point| (f64::from(p.x + origin.x), f64::from(p.y + origin.y));
    let mut path = kurbo::BezPath::new();
    let mut open = false;
    for command in commands {
        match *command {
            css::PathCommand::Move(point) => {
                if close && open {
                    path.close_path();
                }
                path.move_to(convert(point));
                open = true;
            }
            css::PathCommand::Line(point) => path.line_to(convert(point)),
            css::PathCommand::Quad { control, to } => path.quad_to(convert(control), convert(to)),
            css::PathCommand::Cubic {
                control1,
                control2,
                to,
            } => path.curve_to(convert(control1), convert(control2), convert(to)),
            css::PathCommand::Close => {
                path.close_path();
                open = false;
            }
        }
    }
    if close && open {
        path.close_path();
    }
    path
}

fn fill_rule(rule: css::FillRule) -> FillRule {
    match rule {
        css::FillRule::NonZero => FillRule::NonZero,
        css::FillRule::EvenOdd => FillRule::EvenOdd,
    }
}

fn border_side(side: css::BorderSide, width: f32) -> BorderSide {
    use css::BorderStyle as Style;
    let style = match side.style {
        Style::None => BorderStyle::None,
        Style::Hidden => BorderStyle::Hidden,
        Style::Solid => BorderStyle::Solid,
        Style::Dotted => BorderStyle::Dotted,
        Style::Dashed => BorderStyle::Dashed,
        Style::Double => BorderStyle::Double,
        Style::Groove => BorderStyle::Groove,
        Style::Ridge => BorderStyle::Ridge,
        Style::Inset => BorderStyle::Inset,
        Style::Outset => BorderStyle::Outset,
    };
    BorderSide {
        style,
        width,
        color: color(side.color),
    }
}

fn shadow(value: css::Shadow) -> Shadow {
    Shadow {
        offset: value.offset,
        blur: value.blur,
        spread: value.spread,
        color: color(value.color),
        inset: value.inset,
    }
}

pub(crate) fn filter(value: css::Filter) -> Filter {
    match value {
        css::Filter::Blur(v) => Filter::Blur(v),
        css::Filter::Brightness(v) => Filter::Brightness(v),
        css::Filter::Contrast(v) => Filter::Contrast(v),
        css::Filter::DropShadow(v) => Filter::DropShadow(shadow(v)),
        css::Filter::Grayscale(v) => Filter::Grayscale(v),
        css::Filter::HueRotate(v) => Filter::HueRotate(v),
        css::Filter::Invert(v) => Filter::Invert(v),
        css::Filter::Opacity(v) => Filter::Opacity(v),
        css::Filter::Saturate(v) => Filter::Saturate(v),
        css::Filter::Sepia(v) => Filter::Sepia(v),
    }
}

pub(crate) fn resolve_transforms(transforms: &[css::Transform], size: Size) -> crate::Matrix {
    transforms.iter().fold(IDENTITY, |result, transform| {
        let matrix = match *transform {
            css::Transform::Matrix(matrix) => matrix,
            css::Transform::Translate { xy, z } => {
                let xy = resolve_position(xy, size);
                translation(xy.x, xy.y, z)
            }
            css::Transform::Rotate { .. }
            | css::Transform::Scale { .. }
            | css::Transform::Skew { .. } => transform.matrix(size),
        };
        multiply(result, matrix)
    })
}

pub(crate) fn color(value: css::PaintColor) -> PaintColor {
    let [r, g, b] = value.components;
    PaintColor {
        space: color_space(value.space),
        rgba: [r, g, b, value.alpha],
    }
}

pub(crate) fn color_space(space: css::ColorSpace) -> ColorSpace {
    use css::ColorSpace as Space;
    match space {
        Space::Srgb => ColorSpace::Srgb,
        Space::SrgbLinear => ColorSpace::SrgbLinear,
        Space::DisplayP3 => ColorSpace::DisplayP3,
        Space::DisplayP3Linear => ColorSpace::DisplayP3Linear,
        Space::A98Rgb => ColorSpace::A98Rgb,
        Space::ProphotoRgb => ColorSpace::ProPhotoRgb,
        Space::Rec2020 => ColorSpace::Rec2020,
        Space::XyzD50 => ColorSpace::XyzD50,
        Space::XyzD65 => ColorSpace::XyzD65,
        Space::Hsl => ColorSpace::Hsl,
        Space::Hwb => ColorSpace::Hwb,
        Space::Lab => ColorSpace::Lab,
        Space::Lch => ColorSpace::Lch,
        Space::Oklab => ColorSpace::Oklab,
        Space::Oklch => ColorSpace::Oklch,
    }
}
