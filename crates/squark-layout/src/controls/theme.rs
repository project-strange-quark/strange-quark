// SPDX-License-Identifier: MIT

use std::sync::Arc;

use squark_types::{
    Point, Rect,
    fragment::{
        BorderSide, BorderStyle, Edges, FillRule, FragmentContent, LengthPercentage, LineCap,
        LineJoin, Outline, PaintColor, PaintValues, PathCommand, Position2D, SvgGeometry, SvgPaint,
        SvgPath, SvgStroke,
    },
    journal::ElementState as State,
};

use super::{Control, Kind};

const WHITE: PaintColor = PaintColor::srgb(1.0, 1.0, 1.0, 1.0);
const GRAY: PaintColor = gray(118);
const BUTTON: PaintColor = gray(239);
const BLUE: PaintColor = PaintColor::srgb(0.0, 117.0 / 255.0, 1.0, 1.0);
const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

const fn gray(value: u8) -> PaintColor {
    let channel = value as f32 / 255.0;
    PaintColor::srgb(channel, channel, channel, 1.0)
}

fn has(state: State, bit: State) -> bool {
    state.bits() & bit.bits() != 0
}

/// Default Linux appearance keeps the UA box-model edges for geometry and
/// draws the thin rounded frame using the ordinary paint outline.
pub(crate) fn box_chrome(style: &mut PaintValues, border: Edges<f32>, kind: Kind, state: State) {
    let expected = match kind {
        Kind::InputButton => BorderStyle::Outset,
        Kind::Field(_) => BorderStyle::Inset,
        Kind::Menu | Kind::List | Kind::Multiline => BorderStyle::Solid,
        Kind::Checkbox | Kind::Radio => return,
    };
    let disabled = has(state, State::DISABLED);
    let edge_color = if disabled {
        PaintColor { alpha: 0.3, ..GRAY }
    } else {
        GRAY
    };
    let default_fill = match kind {
        Kind::Field(_) | Kind::Multiline if !disabled => WHITE,
        Kind::Menu | Kind::List => BUTTON,
        _ if disabled => PaintColor {
            alpha: 0.3,
            ..BUTTON
        },
        _ => BUTTON,
    };
    let sides = style.box_paint.border;
    let normal_width = if matches!(expected, BorderStyle::Solid) {
        1.0
    } else {
        2.0
    };
    if style.art.background_color != default_fill
        || !style.box_paint.backgrounds.is_empty()
        || style.box_paint.radii != [Position2D::default(); 4]
        || [border.top, border.right, border.bottom, border.left]
            .into_iter()
            .any(|width| width != normal_width)
        || [sides.top, sides.right, sides.bottom, sides.left]
            .into_iter()
            .any(|side| side.style != expected || side.color != edge_color)
    {
        return;
    }
    let (radius, fill, color) = match kind {
        Kind::InputButton => {
            let fill = if disabled {
                gray(238)
            } else if has(state, State::ACTIVE) {
                gray(228)
            } else if has(state, State::HOVER) {
                gray(229)
            } else {
                BUTTON
            };
            (4.0, fill, if disabled { gray(208) } else { GRAY })
        }
        Kind::Menu => (
            2.0,
            if disabled { gray(250) } else { WHITE },
            if disabled { gray(222) } else { GRAY },
        ),
        Kind::List => (2.0, WHITE, if disabled { gray(222) } else { GRAY }),
        _ => (
            2.0,
            if disabled { gray(248) } else { WHITE },
            if disabled { gray(208) } else { GRAY },
        ),
    };
    let radius = LengthPercentage {
        px: radius,
        fraction: 0.0,
    };
    style.box_paint.radii = [Position2D {
        x: radius,
        y: radius,
    }; 4];
    style.art.background_color = fill;
    if style.box_paint.outline.side.style == BorderStyle::None {
        style.box_paint.border = Edges::default();
        style.box_paint.outline = Outline {
            width: 1.0,
            offset: -1.0,
            side: BorderSide {
                style: BorderStyle::Solid,
                color,
            },
        };
    }
}

pub(crate) fn control_art(
    control: &Control,
    state: State,
    border_box: Rect,
    content_box: Rect,
    rtl: bool,
    resize_grip: bool,
) -> FragmentContent {
    let mut paths = Vec::new();
    if !control.native && control.kind != Kind::Multiline {
        return FragmentContent::None;
    }
    let disabled = has(state, State::DISABLED);
    match control.kind {
        Kind::Checkbox | Kind::Radio => {
            let size = border_box.size.width.min(border_box.size.height);
            if size <= 0.0 {
                return FragmentContent::None;
            }
            let origin = Point {
                x: border_box.origin.x - content_box.origin.x
                    + (border_box.size.width - size) / 2.0,
                y: border_box.origin.y - content_box.origin.y
                    + (border_box.size.height - size) / 2.0,
            };
            toggle(&mut paths, control.kind == Kind::Radio, state, origin, size);
        }
        Kind::Menu => {
            // The platform arrow is positioned against the control's edge.
            // Author padding only moves the label inside the content box.
            let x = if rtl {
                border_box.origin.x + 9.0
            } else {
                border_box.max_x() - 9.0
            } - content_box.origin.x;
            let y = border_box.origin.y + border_box.size.height / 2.0 - content_box.origin.y;
            if border_box.size.width >= 18.0 {
                paths.push(stroked(
                    polyline(&[(x - 3.5, y - 2.0), (x, y + 1.6), (x + 3.5, y - 2.0)]),
                    if disabled {
                        gray(128)
                    } else {
                        PaintColor::BLACK
                    },
                    2.0,
                ));
            }
        }
        Kind::Multiline
            if resize_grip
                && !disabled
                && border_box.size.width >= 12.0
                && border_box.size.height >= 12.0 =>
        {
            let (edge, inward) = if rtl {
                (border_box.origin.x + 3.0, 1.0)
            } else {
                (border_box.max_x() - 3.0, -1.0)
            };
            let x = edge - content_box.origin.x;
            let y = border_box.max_y() - content_box.origin.y - 3.0;
            for length in [3.0, 7.0] {
                paths.push(stroked(
                    polyline(&[(x + inward * length, y), (x, y - length)]),
                    GRAY,
                    1.0,
                ));
            }
        }
        Kind::List => {
            let row_height = control.row_height();
            let color = if has(state, State::FOCUS) && !disabled {
                PaintColor::srgb(0.20, 0.40, 0.80, 1.0)
            } else {
                gray(206)
            };
            for row in control.selected_rows() {
                paths.push(filled(
                    rounded_rect(
                        Rect::new(
                            0.0,
                            row as f32 * row_height,
                            content_box.size.width,
                            row_height,
                        ),
                        0.0,
                    ),
                    color,
                ));
            }
        }
        _ => {}
    }
    if paths.is_empty() {
        FragmentContent::None
    } else {
        FragmentContent::Svg(Arc::new(SvgGeometry {
            paths: paths.into_boxed_slice(),
        }))
    }
}

pub(crate) fn broken_image(size: squark_types::Size, rtl: bool) -> FragmentContent {
    if size.width < 16.0 || size.height < 16.0 {
        return FragmentContent::None;
    }
    let x = if rtl { size.width - 16.0 } else { 0.0 };
    let frame = filled(rounded_rect(Rect::new(x, 0.0, 16.0, 16.0), 1.0), GRAY);
    let sky = filled(
        rounded_rect(Rect::new(x + 1.0, 1.0, 14.0, 14.0), 0.0),
        PaintColor::srgb(0.86, 0.93, 1.0, 1.0),
    );
    let landscape = filled(
        polyline(&[
            (x + 1.0, 14.0),
            (x + 6.0, 9.0),
            (x + 9.0, 12.0),
            (x + 15.0, 6.0),
            (x + 15.0, 15.0),
            (x + 1.0, 15.0),
        ]),
        PaintColor::srgb(0.28, 0.55, 0.23, 1.0),
    );
    FragmentContent::Svg(Arc::new(SvgGeometry {
        paths: Box::new([frame, sky, landscape]),
    }))
}

fn toggle(paths: &mut Vec<SvgPath>, radio: bool, state: State, origin: Point, size: f32) {
    let u = size / 13.0;
    let rect = |x: f32, y: f32, w: f32, h: f32| {
        Rect::new(origin.x + x * u, origin.y + y * u, w * u, h * u)
    };
    let disabled = has(state, State::DISABLED);
    let pressed = has(state, State::ACTIVE);
    let hovered = has(state, State::HOVER);
    let indeterminate = !radio && has(state, State::INDETERMINATE);
    let checked = has(state, State::CHECKED);
    let accent = if disabled {
        gray(209)
    } else if pressed {
        PaintColor::srgb(0.0, 0.32, 0.75, 1.0)
    } else if hovered {
        PaintColor::srgb(0.0, 0.40, 0.87, 1.0)
    } else {
        BLUE
    };
    let edge = if disabled {
        gray(209)
    } else if hovered {
        gray(79)
    } else {
        GRAY
    };
    let surface = if disabled { gray(248) } else { WHITE };
    if radio {
        paths.push(filled(
            rounded_rect(rect(0.0, 0.0, 13.0, 13.0), 6.5 * u),
            if checked { accent } else { edge },
        ));
        paths.push(filled(
            rounded_rect(rect(1.1, 1.1, 10.8, 10.8), 5.4 * u),
            surface,
        ));
        if checked {
            paths.push(filled(
                rounded_rect(rect(3.0, 3.0, 7.0, 7.0), 3.5 * u),
                accent,
            ));
        }
    } else if checked || indeterminate {
        paths.push(filled(
            rounded_rect(rect(0.0, 0.0, 13.0, 13.0), 2.5 * u),
            accent,
        ));
        let marks = if indeterminate {
            vec![(3.0, 6.5), (10.0, 6.5)]
        } else {
            vec![(3.0, 6.5), (5.4, 9.0), (10.2, 3.5)]
        };
        let points: Vec<_> = marks
            .into_iter()
            .map(|(x, y)| (origin.x + x * u, origin.y + y * u))
            .collect();
        paths.push(stroked(
            polyline(&points),
            if disabled { gray(238) } else { WHITE },
            1.9 * u,
        ));
    } else {
        paths.push(filled(
            rounded_rect(rect(0.0, 0.0, 13.0, 13.0), 2.5 * u),
            edge,
        ));
        paths.push(filled(
            rounded_rect(rect(1.0, 1.0, 11.0, 11.0), 1.5 * u),
            surface,
        ));
    }
}

fn polyline(points: &[(f32, f32)]) -> Arc<[PathCommand]> {
    points
        .iter()
        .enumerate()
        .map(|(i, &(x, y))| {
            let point = Point { x, y };
            if i == 0 {
                PathCommand::Move(point)
            } else {
                PathCommand::Line(point)
            }
        })
        .collect()
}

fn rounded_rect(rect: Rect, radius: f32) -> Arc<[PathCommand]> {
    let (l, t, r, b) = (rect.origin.x, rect.origin.y, rect.max_x(), rect.max_y());
    let q = radius.min(rect.size.width.min(rect.size.height) / 2.0);
    let c = q * 0.552_284_8;
    let p = |x, y| Point { x, y };
    let cubic = |control1, control2, to| PathCommand::Cubic {
        control1,
        control2,
        to,
    };
    [
        PathCommand::Move(p(l + q, t)),
        PathCommand::Line(p(r - q, t)),
        cubic(p(r - q + c, t), p(r, t + q - c), p(r, t + q)),
        PathCommand::Line(p(r, b - q)),
        cubic(p(r, b - q + c), p(r - q + c, b), p(r - q, b)),
        PathCommand::Line(p(l + q, b)),
        cubic(p(l + q - c, b), p(l, b - q + c), p(l, b - q)),
        PathCommand::Line(p(l, t + q)),
        cubic(p(l, t + q - c), p(l + q - c, t), p(l + q, t)),
        PathCommand::Close,
    ]
    .into()
}

fn filled(commands: Arc<[PathCommand]>, color: PaintColor) -> SvgPath {
    SvgPath {
        commands,
        fill: Some(SvgPaint::Color(color)),
        stroke: None,
        clip: None,
        fill_rule: FillRule::NonZero,
        transform: IDENTITY,
    }
}

pub(crate) fn edit_fill(rect: Rect, color: PaintColor) -> FragmentContent {
    let path = filled(
        rounded_rect(Rect::new(0.0, 0.0, rect.size.width, rect.size.height), 0.0),
        color,
    );
    FragmentContent::Svg(Arc::new(SvgGeometry {
        paths: Box::new([path]),
    }))
}

fn stroked(commands: Arc<[PathCommand]>, color: PaintColor, width: f32) -> SvgPath {
    SvgPath {
        commands,
        fill: None,
        clip: None,
        fill_rule: FillRule::NonZero,
        transform: IDENTITY,
        stroke: Some(SvgStroke {
            paint: SvgPaint::Color(color),
            width,
            dash_array: Box::default(),
            dash_offset: 0.0,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            miter_limit: 4.0,
        }),
    }
}
