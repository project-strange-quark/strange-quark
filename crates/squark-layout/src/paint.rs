// SPDX-License-Identifier: MIT
pub(crate) mod effects;
pub(crate) use effects::animated_value;
mod shapes;

use std::sync::Arc;

use squark_types::Size;

use squark_assets::{ImageCatalog, Resources};
use squark_types::fragment::{
    ArtValues, BackgroundClip, BackgroundImage, BackgroundLayer, BorderSide, BorderStyle,
    BoxPaintValues, ColorSpace, Containment, Cursor, DisplayKind, Edges, GeometryBox, Gradient,
    GradientGeometry, GradientStop, HueInterpolation, ImageRendering, ImageSize, InteractionValues,
    LengthPercentage, ObjectFit, Outline, Overflow, OverscrollBehavior, PaintColor, PaintValues,
    PointerEvents, Position2D, PositionKind, RadialExtent, Repeat, StackingValues, StopPosition,
    Visibility,
};
use style::{
    color::{
        AbsoluteColor, ColorSpace as CssSpace,
        mix::{ColorInterpolationMethod, HueInterpolationMethod},
    },
    properties::ComputedValues,
    values::{
        computed::background::BackgroundSize,
        computed::image::{Gradient as CssGradient, Image, LineDirection},
        computed::{AngleOrPercentage, CSSPixelLength, Color, LengthPercentage as CssLength},
        generics::{
            image::{Circle, Ellipse, EndingShape, GradientItem, ShapeExtent},
            length::LengthPercentageOrAuto,
        },
        specified::{
            background::{BackgroundClip as Clip, BackgroundOrigin, BackgroundRepeatKeyword},
            box_::{DisplayInside, DisplayOutside, Overflow as CssOverflow},
        },
    },
};

pub(crate) fn project(
    css: &ComputedValues,
    resources: Resources<'_>,
    images: &mut ImageCatalog,
    border_size: Size,
    padding_size: Size,
    content_size: Size,
) -> PaintValues {
    let background = css.get_background();
    let current = css.get_color();
    let mut backgrounds = Vec::new();
    for (index, image) in background.background_image.0.iter().enumerate() {
        let image = match image {
            Image::Url(url) => url
                .url()
                .and_then(|url| images.find(resources, url))
                .map(|image| BackgroundImage::Image(image.key)),
            Image::Gradient(gradient) => Some(BackgroundImage::Gradient(Arc::new(
                project_gradient(gradient, current),
            ))),
            _ => None,
        };
        let Some(image) = image else { continue };
        let origin =
            match background.background_origin.0[index % background.background_origin.0.len()] {
                BackgroundOrigin::BorderBox => GeometryBox::Border,
                BackgroundOrigin::PaddingBox => GeometryBox::Padding,
                BackgroundOrigin::ContentBox => GeometryBox::Content,
            };
        let clip = clip(background.background_clip.0[index % background.background_clip.0.len()]);
        let size = match &background.background_size.0[index % background.background_size.0.len()] {
            BackgroundSize::Contain => ImageSize::Contain,
            BackgroundSize::Cover => ImageSize::Cover,
            BackgroundSize::ExplicitSize { width, height } => {
                let value = |length: &LengthPercentageOrAuto<
                    style::values::computed::NonNegativeLengthPercentage,
                >| match length {
                    LengthPercentageOrAuto::Auto => None,
                    LengthPercentageOrAuto::LengthPercentage(px) => Some(length_percentage(&px.0)),
                };
                ImageSize::Explicit([value(width), value(height)])
            }
        };
        let x = &background.background_position_x.0;
        let y = &background.background_position_y.0;
        let repeat = &background.background_repeat.0[index % background.background_repeat.0.len()];
        backgrounds.push(BackgroundLayer {
            image,
            origin,
            clip,
            size,
            position: Position2D {
                x: length_percentage(&x[index % x.len()]),
                y: length_percentage(&y[index % y.len()]),
            },
            repeat: [repetition(repeat.0), repetition(repeat.1)],
        });
    }
    let bottom_layer =
        background.background_image.0.len().saturating_sub(1) % background.background_clip.0.len();
    let display = *css.get_display();
    let is_inline = display.outside() == DisplayOutside::Inline;
    let display = if display.is_none() {
        DisplayKind::None
    } else {
        match (display.inside(), is_inline, display.is_list_item()) {
            (DisplayInside::Contents, _, _) => DisplayKind::Contents,
            (DisplayInside::Flex, true, _) => DisplayKind::InlineFlex,
            (DisplayInside::Flex, false, _) => DisplayKind::Flex,
            (DisplayInside::Grid, true, _) => DisplayKind::InlineGrid,
            (DisplayInside::Grid, false, _) => DisplayKind::Grid,
            (DisplayInside::Table, _, _) => DisplayKind::Table,
            (DisplayInside::FlowRoot, true, _) => DisplayKind::InlineBlock,
            (_, _, true) => DisplayKind::ListItem,
            (_, true, _) => DisplayKind::Inline,
            _ => DisplayKind::Block,
        }
    };
    use style::computed_values::{float::T as Float, isolation::T as Isolation};
    use style::computed_values::{position::T as Position, visibility::T as CssVisibility};
    use style::values::computed::{Contain, ZIndex};
    let box_style = css.get_box();
    let border = css.get_border();
    let outline = css.get_outline();
    let contain = box_style.contain;
    let position = css.get_position();
    let border_side = |style, shade: &Color| BorderSide {
        style: border_style(style),
        color: color(&shade.resolve_to_absolute(current)),
    };
    PaintValues {
        image_rendering: match css.get_image_rendering() {
            style::values::specified::ImageRendering::Auto => ImageRendering::Auto,
            style::values::specified::ImageRendering::Pixelated => ImageRendering::Pixelated,
            style::values::specified::ImageRendering::CrispEdges => ImageRendering::CrispEdges,
        },
        stacking: StackingValues {
            position: match css.get_box().position {
                Position::Static => PositionKind::Static,
                Position::Relative => PositionKind::Relative,
                Position::Absolute => PositionKind::Absolute,
                Position::Fixed => PositionKind::Fixed,
                Position::Sticky => PositionKind::Sticky,
            },
            display,
            visibility: match css.get_visibility() {
                CssVisibility::Visible => Visibility::Visible,
                CssVisibility::Hidden => Visibility::Hidden,
                CssVisibility::Collapse => Visibility::Collapse,
            },
            z_index: match position.z_index {
                ZIndex::Auto => None,
                ZIndex::Integer(index) => Some(index),
            },
            isolation: box_style.isolation == Isolation::Isolate,
            floating: box_style.float != Float::None,
            containment: Containment {
                size: contain.contains(Contain::SIZE),
                inline_size: contain.contains(Contain::INLINE_SIZE),
                layout: contain.contains(Contain::LAYOUT),
                style: contain.contains(Contain::STYLE),
                paint: contain.contains(Contain::PAINT),
            },
            ..Default::default()
        },
        box_paint: BoxPaintValues {
            background_clip: clip(background.background_clip.0[bottom_layer]),
            backgrounds: backgrounds.into_boxed_slice(),
            overflow: [
                overflow(*css.get_overflow_x()),
                overflow(*css.get_overflow_y()),
            ],
            border: Edges {
                top: border_side(border.border_top_style, &border.border_top_color),
                right: border_side(border.border_right_style, &border.border_right_color),
                bottom: border_side(border.border_bottom_style, &border.border_bottom_color),
                left: border_side(border.border_left_style, &border.border_left_color),
            },
            radii: [
                radius(&border.border_top_left_radius),
                radius(&border.border_top_right_radius),
                radius(&border.border_bottom_right_radius),
                radius(&border.border_bottom_left_radius),
            ],
            outline: Outline {
                width: outline.outline_width.0.to_f32_px(),
                offset: outline.outline_offset.to_f32_px(),
                side: border_side(
                    match outline.outline_style {
                        style::values::specified::outline::OutlineStyle::Auto => {
                            style::values::specified::border::BorderStyle::Solid
                        }
                        style::values::specified::outline::OutlineStyle::BorderStyle(style) => {
                            style
                        }
                    },
                    &outline.outline_color,
                ),
            },
            overscroll: [
                overscroll(box_style.overscroll_behavior_x),
                overscroll(box_style.overscroll_behavior_y),
            ],
            ..Default::default()
        },
        art: ArtValues {
            color: color(current),
            background_color: color(&background.background_color.resolve_to_absolute(current)),
            box_shadows: effects::box_shadows(css),
            object_fit: object_fit(position.object_fit),
            object_position: position2d(&position.object_position),
            ..Default::default()
        },
        properties: effects::project(css, border_size, padding_size, content_size),
        interaction: interaction(css),
    }
}

fn clip(value: Clip) -> BackgroundClip {
    match value {
        Clip::ContentBox => BackgroundClip::Content,
        Clip::PaddingBox => BackgroundClip::Padding,
        Clip::Text => BackgroundClip::Text,
        _ => BackgroundClip::Border,
    }
}

fn repetition(value: BackgroundRepeatKeyword) -> Repeat {
    match value {
        BackgroundRepeatKeyword::Repeat => Repeat::Repeat,
        BackgroundRepeatKeyword::Space => Repeat::Space,
        BackgroundRepeatKeyword::Round => Repeat::Round,
        BackgroundRepeatKeyword::NoRepeat => Repeat::NoRepeat,
    }
}

fn overflow(value: CssOverflow) -> Overflow {
    match value {
        CssOverflow::Visible => Overflow::Visible,
        CssOverflow::Clip => Overflow::Clip,
        CssOverflow::Hidden => Overflow::Hidden,
        CssOverflow::Scroll => Overflow::Scroll,
        CssOverflow::Auto => Overflow::Auto,
    }
}

pub(crate) fn color(value: &AbsoluteColor) -> PaintColor {
    PaintColor {
        space: color_space(value.color_space),
        components: [value.components.0, value.components.1, value.components.2],
        alpha: value.alpha,
    }
}

fn color_space(space: CssSpace) -> ColorSpace {
    match space {
        CssSpace::Srgb => ColorSpace::Srgb,
        CssSpace::SrgbLinear => ColorSpace::SrgbLinear,
        CssSpace::DisplayP3 => ColorSpace::DisplayP3,
        CssSpace::DisplayP3Linear => ColorSpace::DisplayP3Linear,
        CssSpace::A98Rgb => ColorSpace::A98Rgb,
        CssSpace::ProphotoRgb => ColorSpace::ProphotoRgb,
        CssSpace::Rec2020 => ColorSpace::Rec2020,
        CssSpace::XyzD50 => ColorSpace::XyzD50,
        CssSpace::XyzD65 => ColorSpace::XyzD65,
        CssSpace::Hsl => ColorSpace::Hsl,
        CssSpace::Hwb => ColorSpace::Hwb,
        CssSpace::Lab => ColorSpace::Lab,
        CssSpace::Lch => ColorSpace::Lch,
        CssSpace::Oklab => ColorSpace::Oklab,
        CssSpace::Oklch => ColorSpace::Oklch,
    }
}

fn length_percentage(value: &CssLength) -> LengthPercentage {
    let px = value.resolve(CSSPixelLength::new(0.0)).px();
    let fraction = (value.resolve(CSSPixelLength::new(100.0)).px() - px) / 100.0;
    LengthPercentage { px, fraction }
}

fn project_gradient(gradient: &CssGradient, current: &AbsoluteColor) -> Gradient {
    fn stops<T>(
        items: &[GradientItem<Color, T>],
        current: &AbsoluteColor,
        position: impl Fn(&T) -> StopPosition,
    ) -> Box<[GradientStop]> {
        items
            .iter()
            .map(|item| match item {
                GradientItem::SimpleColorStop(stop) => GradientStop::Color {
                    position: None,
                    color: color(&stop.resolve_to_absolute(current)),
                },
                GradientItem::ComplexColorStop {
                    color: stop,
                    position: offset,
                } => GradientStop::Color {
                    position: Some(position(offset)),
                    color: color(&stop.resolve_to_absolute(current)),
                },
                GradientItem::InterpolationHint(offset) => GradientStop::Hint(position(offset)),
            })
            .collect()
    }

    let length = |value: &CssLength| StopPosition::Length(length_percentage(value));
    let (geometry, items, method, repeating) = match gradient {
        CssGradient::Linear {
            direction,
            items,
            color_interpolation_method,
            flags,
            ..
        } => {
            use style::values::specified::position::{
                HorizontalPositionKeyword as X, VerticalPositionKeyword as Y,
            };
            let x = |side| match side {
                X::Left => -1,
                X::Right => 1,
            };
            let y = |side| match side {
                Y::Top => -1,
                Y::Bottom => 1,
            };
            let geometry = match *direction {
                LineDirection::Angle(value) => GradientGeometry::Linear {
                    angle_radians: value.radians(),
                },
                LineDirection::Horizontal(value) => GradientGeometry::LinearTo {
                    horizontal: x(value),
                    vertical: 0,
                },
                LineDirection::Vertical(value) => GradientGeometry::LinearTo {
                    horizontal: 0,
                    vertical: y(value),
                },
                LineDirection::Corner(a, b) => GradientGeometry::LinearTo {
                    horizontal: x(a),
                    vertical: y(b),
                },
            };
            (
                geometry,
                stops(items, current, length),
                color_interpolation_method,
                flags.bits() & 1 != 0,
            )
        }
        CssGradient::Radial {
            shape,
            position,
            items,
            color_interpolation_method,
            flags,
            ..
        } => {
            let extent = |extent| match extent {
                ShapeExtent::ClosestSide | ShapeExtent::Contain => RadialExtent::ClosestSide,
                ShapeExtent::ClosestCorner => RadialExtent::ClosestCorner,
                ShapeExtent::FarthestSide => RadialExtent::FarthestSide,
                ShapeExtent::FarthestCorner | ShapeExtent::Cover => RadialExtent::FarthestCorner,
            };
            let radius = |value| LengthPercentage {
                px: value,
                fraction: 0.0,
            };
            let (extent, circle) = match shape {
                EndingShape::Circle(Circle::Radius(value)) => {
                    let value = radius(value.0.px());
                    (
                        RadialExtent::Explicit(Position2D { x: value, y: value }),
                        true,
                    )
                }
                EndingShape::Circle(Circle::Extent(value)) => (extent(*value), true),
                EndingShape::Ellipse(Ellipse::Radii(x, y)) => (
                    RadialExtent::Explicit(Position2D {
                        x: length_percentage(&x.0),
                        y: length_percentage(&y.0),
                    }),
                    false,
                ),
                EndingShape::Ellipse(Ellipse::Extent(value)) => (extent(*value), false),
            };
            let center = Position2D {
                x: length_percentage(&position.horizontal),
                y: length_percentage(&position.vertical),
            };
            (
                GradientGeometry::Radial {
                    center,
                    extent,
                    circle,
                },
                stops(items, current, length),
                color_interpolation_method,
                flags.bits() & 1 != 0,
            )
        }
        CssGradient::Conic {
            angle,
            position,
            items,
            color_interpolation_method,
            flags,
        } => {
            let center = Position2D {
                x: length_percentage(&position.horizontal),
                y: length_percentage(&position.vertical),
            };
            let angular = |value: &AngleOrPercentage| {
                StopPosition::AngleRadians(match value {
                    AngleOrPercentage::Angle(angle) => angle.radians(),
                    AngleOrPercentage::Percentage(percent) => percent.0 * std::f32::consts::TAU,
                })
            };
            (
                GradientGeometry::Conic {
                    center,
                    from_radians: angle.radians(),
                },
                stops(items, current, angular),
                color_interpolation_method,
                flags.bits() & 1 != 0,
            )
        }
    };
    let ColorInterpolationMethod { space, hue } = *method;
    Gradient {
        geometry,
        stops: items,
        space: color_space(space),
        repeating,
        hue: match hue {
            HueInterpolationMethod::Shorter | HueInterpolationMethod::Specified => {
                HueInterpolation::Shorter
            }
            HueInterpolationMethod::Longer => HueInterpolation::Longer,
            HueInterpolationMethod::Increasing => HueInterpolation::Increasing,
            HueInterpolationMethod::Decreasing => HueInterpolation::Decreasing,
        },
    }
}

pub(crate) fn text(css: &ComputedValues) -> PaintValues {
    use style::computed_values::visibility::T as CssVisibility;
    PaintValues {
        stacking: StackingValues {
            visibility: match css.get_visibility() {
                CssVisibility::Visible => Visibility::Visible,
                CssVisibility::Hidden => Visibility::Hidden,
                CssVisibility::Collapse => Visibility::Collapse,
            },
            ..Default::default()
        },
        art: ArtValues {
            color: color(css.get_color()),
            text_shadows: effects::text_shadows(css),
            ..Default::default()
        },
        interaction: interaction(css),
        ..Default::default()
    }
}

fn interaction(css: &ComputedValues) -> InteractionValues {
    use style::values::specified::ui::{CursorKind, PointerEvents as CssPointerEvents};
    InteractionValues {
        pointer_events: match css.get_pointer_events() {
            CssPointerEvents::None => PointerEvents::None,
            CssPointerEvents::Auto
            | CssPointerEvents::Visiblepainted
            | CssPointerEvents::Visiblefill
            | CssPointerEvents::Visiblestroke
            | CssPointerEvents::Visible
            | CssPointerEvents::Painted
            | CssPointerEvents::Fill
            | CssPointerEvents::Stroke
            | CssPointerEvents::All => PointerEvents::Auto,
        },
        cursor: match css.get_cursor().keyword {
            CursorKind::None => Cursor::None,
            CursorKind::Default => Cursor::Default,
            CursorKind::Pointer => Cursor::Pointer,
            CursorKind::ContextMenu => Cursor::ContextMenu,
            CursorKind::Help => Cursor::Help,
            CursorKind::Progress => Cursor::Progress,
            CursorKind::Wait => Cursor::Wait,
            CursorKind::Cell => Cursor::Cell,
            CursorKind::Crosshair => Cursor::Crosshair,
            CursorKind::Text => Cursor::Text,
            CursorKind::VerticalText => Cursor::VerticalText,
            CursorKind::Alias => Cursor::Alias,
            CursorKind::Copy => Cursor::Copy,
            CursorKind::Move => Cursor::Move,
            CursorKind::NoDrop => Cursor::NoDrop,
            CursorKind::NotAllowed => Cursor::NotAllowed,
            CursorKind::Grab => Cursor::Grab,
            CursorKind::Grabbing => Cursor::Grabbing,
            CursorKind::EResize => Cursor::EResize,
            CursorKind::NResize => Cursor::NResize,
            CursorKind::NeResize => Cursor::NeResize,
            CursorKind::NwResize => Cursor::NwResize,
            CursorKind::SResize => Cursor::SResize,
            CursorKind::SeResize => Cursor::SeResize,
            CursorKind::SwResize => Cursor::SwResize,
            CursorKind::WResize => Cursor::WResize,
            CursorKind::EwResize => Cursor::EwResize,
            CursorKind::NsResize => Cursor::NsResize,
            CursorKind::NeswResize => Cursor::NeswResize,
            CursorKind::NwseResize => Cursor::NwseResize,
            CursorKind::ColResize => Cursor::ColResize,
            CursorKind::RowResize => Cursor::RowResize,
            CursorKind::AllScroll => Cursor::AllScroll,
            CursorKind::ZoomIn => Cursor::ZoomIn,
            CursorKind::ZoomOut => Cursor::ZoomOut,
            CursorKind::Auto => Cursor::Auto,
        },
    }
}

fn position2d(value: &style::values::computed::Position) -> Position2D {
    Position2D {
        x: length_percentage(&value.horizontal),
        y: length_percentage(&value.vertical),
    }
}

fn radius(value: &style::values::computed::border::BorderCornerRadius) -> Position2D {
    let value = &value.0;
    Position2D {
        x: length_percentage(&value.width.0),
        y: length_percentage(&value.height.0),
    }
}

fn border_style(value: style::values::specified::border::BorderStyle) -> BorderStyle {
    use style::values::specified::border::BorderStyle as Css;
    match value {
        Css::None => BorderStyle::None,
        Css::Hidden => BorderStyle::Hidden,
        Css::Solid => BorderStyle::Solid,
        Css::Dotted => BorderStyle::Dotted,
        Css::Dashed => BorderStyle::Dashed,
        Css::Double => BorderStyle::Double,
        Css::Groove => BorderStyle::Groove,
        Css::Ridge => BorderStyle::Ridge,
        Css::Inset => BorderStyle::Inset,
        Css::Outset => BorderStyle::Outset,
    }
}

fn overscroll(value: style::computed_values::overscroll_behavior_x::T) -> OverscrollBehavior {
    use style::computed_values::overscroll_behavior_x::T as Css;
    match value {
        Css::Auto | Css::Chain => OverscrollBehavior::Auto,
        Css::Contain => OverscrollBehavior::Contain,
        Css::None => OverscrollBehavior::None,
    }
}

fn object_fit(value: style::values::specified::position::ObjectFit) -> ObjectFit {
    use style::values::specified::position::ObjectFit as Css;
    match value {
        Css::Fill => ObjectFit::Fill,
        Css::Contain => ObjectFit::Contain,
        Css::Cover => ObjectFit::Cover,
        Css::None => ObjectFit::None,
        Css::ScaleDown => ObjectFit::ScaleDown,
    }
}

pub(crate) fn has_inline_box(css: &ComputedValues) -> bool {
    use style::Zero;
    let border = css.get_border();
    let pad = css.get_padding();
    let margin = css.get_margin();
    let background = css.get_background();
    let color = background
        .background_color
        .resolve_to_absolute(css.get_color());
    color.alpha != 0.0
        || background
            .background_image
            .0
            .iter()
            .any(|image| !matches!(image, Image::None))
        || !border.border_top_style.none_or_hidden()
        || !border.border_right_style.none_or_hidden()
        || !border.border_bottom_style.none_or_hidden()
        || !border.border_left_style.none_or_hidden()
        || !pad.padding_top.0.is_zero()
        || !pad.padding_right.0.is_zero()
        || !pad.padding_bottom.0.is_zero()
        || !pad.padding_left.0.is_zero()
        || !margin.margin_left.is_zero()
        || !margin.margin_right.is_zero()
        || !css.get_effects().box_shadow.0.is_empty()
}
