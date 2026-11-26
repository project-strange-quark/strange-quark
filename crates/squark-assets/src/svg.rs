// SPDX-License-Identifier: MIT

use resvg::{
    tiny_skia::PathSegment,
    usvg::{self, Color, Group, Node, Paint, PaintOrder},
};
use squark_types::{
    Point,
    fragment::{
        FillRule, LineCap, LineJoin, PaintColor, PathCommand, SvgGeometry, SvgPaint, SvgPath,
        SvgStroke,
    },
};

pub fn svg_geometry(xml: &str) -> Option<SvgGeometry> {
    let tree = usvg::Tree::from_str(xml, &crate::images::svg_options()).ok()?;
    let mut paths = Vec::new();
    collect_paths(tree.root(), 1.0, &mut paths);
    Some(SvgGeometry {
        paths: paths.into_boxed_slice(),
    })
}

fn collect_paths(group: &Group, opacity: f32, paths: &mut Vec<SvgPath>) {
    let opacity = opacity * group.opacity().get();
    for child in group.children() {
        match child {
            Node::Group(group) => collect_paths(group, opacity, paths),
            Node::Path(path) if path.is_visible() => {
                let t = path.abs_transform();
                // Normalize uniform viewBox scales into viewport-space paths so
                // paint rasterizes their coverage at the final CSS dimensions.
                let uniform = t.kx == 0.0 && t.ky == 0.0 && t.sx == t.sy && t.sx > 0.0;
                let scale = if uniform { t.sx } else { 1.0 };
                let commands = path
                    .data()
                    .segments()
                    .map(|segment| {
                        let point = |p: resvg::tiny_skia::Point| {
                            if uniform {
                                Point {
                                    x: p.x * scale + t.tx,
                                    y: p.y * scale + t.ty,
                                }
                            } else {
                                Point { x: p.x, y: p.y }
                            }
                        };
                        match segment {
                            PathSegment::MoveTo(to) => PathCommand::Move(point(to)),
                            PathSegment::LineTo(to) => PathCommand::Line(point(to)),
                            PathSegment::QuadTo(control, to) => PathCommand::Quad {
                                control: point(control),
                                to: point(to),
                            },
                            PathSegment::CubicTo(c1, c2, to) => PathCommand::Cubic {
                                control1: point(c1),
                                control2: point(c2),
                                to: point(to),
                            },
                            PathSegment::Close => PathCommand::Close,
                        }
                    })
                    .collect();
                let fill = path.fill();
                let fill_rule = match fill.map(|fill| fill.rule()) {
                    Some(usvg::FillRule::EvenOdd) => FillRule::EvenOdd,
                    _ => FillRule::NonZero,
                };
                let t = if uniform {
                    resvg::tiny_skia::Transform::identity()
                } else {
                    t
                };
                let mut shape = SvgPath {
                    commands,
                    fill: fill.and_then(|fill| paint(fill.paint(), opacity * fill.opacity().get())),
                    fill_rule,
                    stroke: path.stroke().and_then(|stroke| {
                        Some(SvgStroke {
                            paint: paint(stroke.paint(), opacity * stroke.opacity().get())?,
                            width: stroke.width().get() * scale,
                            dash_array: stroke
                                .dasharray()
                                .unwrap_or_default()
                                .iter()
                                .map(|dash| dash * scale)
                                .collect(),
                            dash_offset: stroke.dashoffset() * scale,
                            line_cap: match stroke.linecap() {
                                usvg::LineCap::Butt => LineCap::Butt,
                                usvg::LineCap::Round => LineCap::Round,
                                usvg::LineCap::Square => LineCap::Square,
                            },
                            line_join: match stroke.linejoin() {
                                usvg::LineJoin::Miter | usvg::LineJoin::MiterClip => {
                                    LineJoin::Miter
                                }
                                usvg::LineJoin::Round => LineJoin::Round,
                                usvg::LineJoin::Bevel => LineJoin::Bevel,
                            },
                            miter_limit: stroke.miterlimit().get(),
                        })
                    }),
                    transform: [
                        [t.sx, t.ky, 0.0, 0.0],
                        [t.kx, t.sy, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                        [t.tx, t.ty, 0.0, 1.0],
                    ],
                    clip: None,
                };
                if shape.fill.is_some()
                    && shape.stroke.is_some()
                    && path.paint_order() == PaintOrder::StrokeAndFill
                {
                    paths.push(SvgPath {
                        fill: None,
                        ..shape.clone()
                    });
                    shape.stroke = None;
                }
                paths.push(shape);
            }
            _ => {}
        }
    }
}

fn paint(paint: &Paint, alpha: f32) -> Option<SvgPaint> {
    match paint {
        Paint::Color(Color { red, green, blue }) => {
            let [r, g, b] = [red, green, blue].map(|&channel| f32::from(channel) / 255.0);
            Some(SvgPaint::Color(PaintColor::srgb(r, g, b, alpha)))
        }
        _ => None,
    }
}

/// Bounding box without stroke or clipping, in local and outer SVG coordinates.
pub fn svg_bounds(xml: &str, id: &str) -> Option<(squark_types::Rect, squark_types::Rect)> {
    let tree = usvg::Tree::from_str(xml, &crate::images::svg_options()).ok()?;
    let node = tree.node_by_id(id)?;
    let rect = |r: usvg::Rect| squark_types::Rect::new(r.x(), r.y(), r.width(), r.height());
    Some((rect(node.bounding_box()), rect(node.abs_bounding_box())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_svg_viewbox_and_stroke_in_viewport_pixels() {
        let geometry = svg_geometry(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="40" viewBox="0 0 10 10">
            <path d="M1,2 L3,4" fill="none" stroke="red" stroke-width="0.5" stroke-dasharray="1 2"/>
        </svg>"#,
        )
        .unwrap();
        let path = &geometry.paths[0];
        assert_eq!(
            path.commands[0],
            PathCommand::Move(Point { x: 14.0, y: 8.0 })
        );
        assert_eq!(
            path.commands[1],
            PathCommand::Line(Point { x: 22.0, y: 16.0 })
        );
        let stroke = path.stroke.as_ref().unwrap();
        assert_eq!(stroke.width, 2.0);
        assert_eq!(stroke.dash_array.as_ref(), &[4.0, 8.0]);
        assert!(path.fill.is_none());
    }
}
