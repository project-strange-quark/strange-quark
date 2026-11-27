// SPDX-License-Identifier: MIT
//! Conversion functions from Stylo computed style types to Taffy equivalents

/// Private module of type aliases so we can refer to stylo types with nicer names
pub(crate) mod stylo {
    pub(crate) use style::Atom;
    pub(crate) use style::properties::ComputedValues;
    pub(crate) use style::properties::generated::longhands::box_sizing::computed_value::T as BoxSizing;
    pub(crate) use style::properties::generated::longhands::direction::computed_value::T as Direction;
    pub(crate) use style::properties::longhands::aspect_ratio::computed_value::T as AspectRatio;
    pub(crate) use style::properties::longhands::position::computed_value::T as Position;
    pub(crate) use style::values::computed::CSSPixelLength;
    pub(crate) use style::values::computed::length_percentage::Unpacked as UnpackedLengthPercentage;
    pub(crate) use style::values::computed::{
        BorderSideWidth, Contain, LengthPercentage, Percentage,
    };
    pub(crate) use style::values::generics::NonNegative;
    pub(crate) use style::values::generics::length::{
        GenericLengthPercentageOrNormal, GenericMargin, GenericMaxSize, GenericSize,
    };
    pub(crate) use style::values::generics::position::{Inset as GenericInset, PreferredRatio};
    pub(crate) use style::values::specified::align::{AlignFlags, ContentDistribution};
    pub(crate) use style::values::specified::border::BorderStyle;
    pub(crate) use style::values::specified::box_::{
        Display, DisplayInside, DisplayOutside, Overflow,
    };
    pub(crate) use style::values::specified::position::GridTemplateAreas;
    pub(crate) use style::values::specified::position::NamedArea;
    pub(crate) type MarginVal = GenericMargin<LengthPercentage>;
    pub(crate) type InsetVal = GenericInset<Percentage, LengthPercentage>;
    pub(crate) type Size = GenericSize<NonNegative<LengthPercentage>>;
    pub(crate) type MaxSize = GenericMaxSize<NonNegative<LengthPercentage>>;

    pub(crate) type Gap = GenericLengthPercentageOrNormal<NonNegative<LengthPercentage>>;

    pub(crate) use style::{
        computed_values::{flex_direction::T as FlexDirection, flex_wrap::T as FlexWrap},
        values::generics::flex::GenericFlexBasis,
    };
    pub(crate) type FlexBasis = GenericFlexBasis<Size>;

    pub(crate) use style::values::computed::text::TextAlign;
    pub(crate) use style::{
        computed_values::grid_auto_flow::T as GridAutoFlow,
        values::{
            computed::{GridLine, GridTemplateComponent},
            generics::grid::{RepeatCount, TrackBreadth, TrackListValue, TrackSize},
            specified::GenericGridTemplateComponent,
        },
    };
}

use stylo::Atom;
use taffy::style_helpers::*;
use taffy::{CalcHandle, Dimension, LengthPercentage, LengthPercentageAuto};

fn box_sizing(input: stylo::BoxSizing) -> taffy::BoxSizing {
    match input {
        stylo::BoxSizing::BorderBox => taffy::BoxSizing::BorderBox,
        stylo::BoxSizing::ContentBox => taffy::BoxSizing::ContentBox,
    }
}

fn position(input: stylo::Position) -> taffy::Position {
    match input {
        stylo::Position::Static => taffy::Position::Static,
        stylo::Position::Relative => taffy::Position::Relative,
        stylo::Position::Absolute => taffy::Position::Absolute,
        stylo::Position::Fixed => taffy::Position::Fixed,
        stylo::Position::Sticky => taffy::Position::Sticky,
    }
}

fn establishes_fixed_containing_block(style: &stylo::ComputedValues) -> bool {
    use style::values::computed::{Perspective, Rotate, Scale, Translate};
    use style::values::specified::box_::{Contain, ContainerType, WillChangeBits};

    let box_style = style.get_box();
    if !box_style.transform.0.is_empty()
        || !matches!(box_style.translate, Translate::None)
        || !matches!(box_style.rotate, Rotate::None)
        || !matches!(box_style.scale, Scale::None)
        || !matches!(box_style.perspective, Perspective::None)
    {
        return true;
    }
    if box_style.will_change.bits.intersects(
        WillChangeBits::TRANSFORM
            | WillChangeBits::PERSPECTIVE
            | WillChangeBits::FIXPOS_CB_NON_SVG
            | WillChangeBits::CONTAIN,
    ) {
        return true;
    }
    if box_style
        .contain
        .intersects(Contain::LAYOUT | Contain::PAINT)
    {
        return true;
    }
    if box_style
        .container_type
        .intersects(ContainerType::SIZE | ContainerType::INLINE_SIZE)
    {
        return true;
    }

    let effects = style.get_effects();
    !effects.filter.0.is_empty() || !effects.backdrop_filter.0.is_empty()
}

fn establishes_absolute_containing_block(style: &stylo::ComputedValues) -> bool {
    use style::values::specified::box_::WillChangeBits;

    style
        .get_box()
        .will_change
        .bits
        .intersects(WillChangeBits::POSITION)
}

pub(crate) fn containing_block_claims(
    style: &stylo::ComputedValues,
) -> taffy::ContainingBlockClaims {
    let is_positioned = style.get_box().position != stylo::Position::Static;
    let fixed = establishes_fixed_containing_block(style);
    taffy::ContainingBlockClaims {
        absolute: is_positioned || fixed || establishes_absolute_containing_block(style),
        fixed,
    }
}

fn overflow(input: stylo::Overflow) -> taffy::Overflow {
    match input {
        stylo::Overflow::Visible => taffy::Overflow::Visible,
        stylo::Overflow::Clip => taffy::Overflow::Clip,
        stylo::Overflow::Hidden => taffy::Overflow::Hidden,
        stylo::Overflow::Scroll => taffy::Overflow::Scroll,
        stylo::Overflow::Auto => taffy::Overflow::Scroll,
    }
}

fn contain(input: stylo::Contain, display: stylo::Display) -> taffy::Contain {
    // Layout and paint containment do not apply to non-atomic inline-level boxes
    // (https://drafts.csswg.org/css-contain-1/#containment-layout)
    if display.outside() == stylo::DisplayOutside::Inline
        && display.inside() == stylo::DisplayInside::Flow
    {
        return taffy::Contain::NONE;
    }
    let mut result = taffy::Contain::NONE;
    if input.contains(stylo::Contain::LAYOUT) {
        result |= taffy::Contain::LAYOUT;
    }
    if input.contains(stylo::Contain::PAINT) {
        result |= taffy::Contain::PAINT;
    }
    result
}

/// The axes in which size containment sizes the box as if it were empty:
/// `[inline, block]`. Like layout and paint containment it skips non-atomic
/// inline boxes, and it skips internal table boxes too
/// (<https://drafts.csswg.org/css-contain-2/#containment-size>).
pub(crate) fn size_containment(style: &stylo::ComputedValues) -> [bool; 2] {
    let box_style = style.get_box();
    let display = box_style.display;
    let contain = box_style.contain;
    if display.outside() == stylo::DisplayOutside::InternalTable
        || (display.outside() == stylo::DisplayOutside::Inline
            && display.inside() == stylo::DisplayInside::Flow)
    {
        return [false; 2];
    }
    let size = contain.contains(stylo::Contain::SIZE);
    [size || contain.contains(stylo::Contain::INLINE_SIZE), size]
}

/// Whether paint containment clips the box's contents to its padding box,
/// as `overflow: clip` does.
pub(crate) fn paint_containment(style: &stylo::ComputedValues) -> bool {
    let box_style = style.get_box();
    contain(box_style.contain, box_style.display).contains(taffy::Contain::PAINT)
}

fn direction(input: stylo::Direction) -> taffy::Direction {
    match input {
        stylo::Direction::Ltr => taffy::Direction::Ltr,
        stylo::Direction::Rtl => taffy::Direction::Rtl,
    }
}

fn aspect_ratio(input: stylo::AspectRatio) -> Option<f32> {
    match input.ratio {
        // A degenerate ratio (either side zero) behaves as `auto`:
        // <https://drafts.csswg.org/css-sizing-4/#aspect-ratio>
        stylo::PreferredRatio::Ratio(val) if !val.is_degenerate() => Some(val.0.0 / val.1.0),
        _ => None,
    }
}

fn content_alignment(
    input: stylo::ContentDistribution,
    display: stylo::Display,
) -> taffy::AlignContent {
    let primary = input.primary();
    let mut align = match primary.value() {
        stylo::AlignFlags::NORMAL => return taffy::AlignContent::NORMAL,
        stylo::AlignFlags::AUTO => return taffy::AlignContent::NORMAL,
        stylo::AlignFlags::START => taffy::AlignContent::START,
        stylo::AlignFlags::END => taffy::AlignContent::END,
        stylo::AlignFlags::LEFT => taffy::AlignContent::START,
        stylo::AlignFlags::RIGHT => taffy::AlignContent::END,
        stylo::AlignFlags::FLEX_START => taffy::AlignContent::FLEX_START,
        stylo::AlignFlags::STRETCH => taffy::AlignContent::STRETCH,
        stylo::AlignFlags::FLEX_END => taffy::AlignContent::FLEX_END,
        stylo::AlignFlags::CENTER => taffy::AlignContent::CENTER,
        stylo::AlignFlags::SPACE_BETWEEN => taffy::AlignContent::SPACE_BETWEEN,
        stylo::AlignFlags::SPACE_AROUND => taffy::AlignContent::SPACE_AROUND,
        stylo::AlignFlags::SPACE_EVENLY => taffy::AlignContent::SPACE_EVENLY,
        // Baseline content-alignment is not supported: it falls back to start/end
        // (<https://www.w3.org/TR/css-align-3/#baseline-align-self>)
        stylo::AlignFlags::BASELINE => taffy::AlignContent::START,
        stylo::AlignFlags::LAST_BASELINE => taffy::AlignContent::END,
        // Should never be hit. But no real reason to panic here.
        _ => return taffy::AlignContent::NORMAL,
    };
    let is_block_container = matches!(
        display.inside(),
        stylo::DisplayInside::Flow
            | stylo::DisplayInside::FlowRoot
            | stylo::DisplayInside::TableCell
    );
    let safe = primary.flags().contains(stylo::AlignFlags::SAFE)
        || (is_block_container && !primary.flags().contains(stylo::AlignFlags::UNSAFE));
    if safe {
        align.safety = taffy::AlignmentSafety::Safe;
    }
    align
}

fn justify_content(
    input: stylo::ContentDistribution,
    flex_direction: stylo::FlexDirection,
    direction: stylo::Direction,
    display: stylo::Display,
) -> taffy::AlignContent {
    let is_row = matches!(
        flex_direction,
        stylo::FlexDirection::Row | stylo::FlexDirection::RowReverse
    );
    let is_rtl = matches!(direction, stylo::Direction::Rtl);
    justify_content_in(input, is_row.then_some(is_rtl), display)
}

fn justify_content_in(
    input: stylo::ContentDistribution,
    left_is_end: Option<bool>,
    display: stylo::Display,
) -> taffy::AlignContent {
    let primary = input.primary();
    let is_right = match primary.value() {
        stylo::AlignFlags::LEFT => false,
        stylo::AlignFlags::RIGHT => true,
        _ => return self::content_alignment(input, display),
    };
    let mut align = match left_is_end {
        Some(left_is_end) if is_right != left_is_end => taffy::AlignContent::END,
        _ => taffy::AlignContent::START,
    };
    if primary.flags().contains(stylo::AlignFlags::SAFE) {
        align.safety = taffy::AlignmentSafety::Safe;
    }
    align
}

fn item_alignment(input: stylo::AlignFlags, is_horiz_rtl: bool) -> Option<taffy::AlignItems> {
    let mut align = match input.value() {
        stylo::AlignFlags::AUTO => return None,
        stylo::AlignFlags::NORMAL => return Some(taffy::AlignItems::NORMAL),
        stylo::AlignFlags::STRETCH => taffy::AlignItems::STRETCH,
        stylo::AlignFlags::FLEX_START => taffy::AlignItems::FLEX_START,
        stylo::AlignFlags::FLEX_END => taffy::AlignItems::FLEX_END,
        stylo::AlignFlags::SELF_START => taffy::AlignItems::SELF_START,
        stylo::AlignFlags::SELF_END => taffy::AlignItems::SELF_END,
        stylo::AlignFlags::START => taffy::AlignItems::START,
        stylo::AlignFlags::END => taffy::AlignItems::END,
        stylo::AlignFlags::LEFT if is_horiz_rtl => taffy::AlignItems::END,
        stylo::AlignFlags::LEFT => taffy::AlignItems::START,
        stylo::AlignFlags::RIGHT if is_horiz_rtl => taffy::AlignItems::START,
        stylo::AlignFlags::RIGHT => taffy::AlignItems::END,
        stylo::AlignFlags::CENTER => taffy::AlignItems::CENTER,
        stylo::AlignFlags::BASELINE => taffy::AlignItems::BASELINE,
        // Taffy does not support last-baseline alignment, so map it to its
        // fallback alignment of `self-end` (https://www.w3.org/TR/css-align-3/#baseline-values)
        stylo::AlignFlags::LAST_BASELINE => taffy::AlignItems::END,
        // Should never be hit. But no real reason to panic here.
        _ => return None,
    };
    if input.flags().contains(stylo::AlignFlags::SAFE) {
        align.safety = taffy::AlignmentSafety::Safe;
    } else if input.flags().contains(stylo::AlignFlags::UNSAFE) {
        align.safety = taffy::AlignmentSafety::Unsafe;
    }
    Some(align)
}

fn default_item_alignment(input: stylo::AlignFlags, is_horiz_rtl: bool) -> taffy::AlignItems {
    item_alignment(input, is_horiz_rtl).unwrap_or(taffy::AlignItems::NORMAL)
}

fn text_align(input: stylo::TextAlign) -> taffy::TextAlign {
    match input {
        stylo::TextAlign::MozLeft => taffy::TextAlign::LegacyLeft,
        stylo::TextAlign::MozRight => taffy::TextAlign::LegacyRight,
        stylo::TextAlign::MozCenter => taffy::TextAlign::LegacyCenter,
        _ => taffy::TextAlign::Auto,
    }
}

fn flex_direction(input: stylo::FlexDirection) -> taffy::FlexDirection {
    match input {
        stylo::FlexDirection::Row => taffy::FlexDirection::Row,
        stylo::FlexDirection::RowReverse => taffy::FlexDirection::RowReverse,
        stylo::FlexDirection::Column => taffy::FlexDirection::Column,
        stylo::FlexDirection::ColumnReverse => taffy::FlexDirection::ColumnReverse,
    }
}

fn flex_wrap(input: stylo::FlexWrap) -> taffy::FlexWrap {
    if input.contains(stylo::FlexWrap::BALANCE) {
        if input.contains(stylo::FlexWrap::WRAP_REVERSE) {
            taffy::FlexWrap::BalanceReverse
        } else {
            taffy::FlexWrap::Balance
        }
    } else if input.contains(stylo::FlexWrap::WRAP_REVERSE) {
        taffy::FlexWrap::WrapReverse
    } else if input.contains(stylo::FlexWrap::WRAP) {
        taffy::FlexWrap::Wrap
    } else {
        taffy::FlexWrap::NoWrap
    }
}

fn grid_auto_flow(input: stylo::GridAutoFlow) -> taffy::GridAutoFlow {
    let is_row = input.contains(stylo::GridAutoFlow::ROW);
    let is_dense = input.contains(stylo::GridAutoFlow::DENSE);

    match (is_row, is_dense) {
        (true, false) => taffy::GridAutoFlow::Row,
        (true, true) => taffy::GridAutoFlow::RowDense,
        (false, false) => taffy::GridAutoFlow::Column,
        (false, true) => taffy::GridAutoFlow::ColumnDense,
    }
}

fn grid_line(input: &stylo::GridLine) -> taffy::GridPlacement<Atom> {
    if input.is_auto() {
        taffy::GridPlacement::Auto
    } else if input.is_span {
        if !input.ident.0.as_str().is_empty() {
            taffy::GridPlacement::NamedSpan(input.ident.0.clone(), saturating_u16(input.line_num))
        } else {
            taffy::GridPlacement::Span(saturating_u16(input.line_num))
        }
    } else if !input.ident.0.as_str().is_empty() {
        taffy::GridPlacement::NamedLine(input.ident.0.clone(), saturating_i16(input.line_num))
    } else if input.line_num != 0 {
        taffy::style_helpers::line(saturating_i16(input.line_num))
    } else {
        taffy::GridPlacement::Auto
    }
}

fn track_repeat(input: stylo::RepeatCount<i32>) -> taffy::RepetitionCount {
    match input {
        stylo::RepeatCount::Number(val) => taffy::RepetitionCount::Count(saturating_u16(val)),
        stylo::RepeatCount::AutoFill => taffy::RepetitionCount::AutoFill,
        stylo::RepeatCount::AutoFit => taffy::RepetitionCount::AutoFit,
    }
}

fn grid_template_area(input: &stylo::NamedArea) -> taffy::GridTemplateArea<Atom> {
    taffy::GridTemplateArea {
        name: input.name.clone(),
        row_start: saturating_u16(input.rows.start as i32),
        row_end: saturating_u16(input.rows.end as i32),
        column_start: saturating_u16(input.columns.start as i32),
        column_end: saturating_u16(input.columns.end as i32),
    }
}

fn saturating_u16(value: i32) -> u16 {
    value.clamp(0, i32::from(u16::MAX)) as u16
}

fn saturating_i16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

#[derive(Debug, Default)]
pub(crate) struct Calcs(Vec<stylo::LengthPercentage>);

impl Calcs {
    fn insert(&mut self, value: &stylo::LengthPercentage) -> CalcHandle {
        self.0.push(value.clone());
        // CompactLength reserves the low three bits. This handle is a table
        // index, and neither the tree nor the resolver dereferences it.
        CalcHandle::from_bits((self.0.len() as u64) << 3)
    }

    pub(crate) fn resolve(&self, handle: CalcHandle, basis: f32) -> f32 {
        let index = (handle.to_bits() >> 3) as usize - 1;
        self.0[index]
            .resolve(stylo::CSSPixelLength::new(basis))
            .px()
    }

    fn length(&mut self, value: &stylo::LengthPercentage) -> LengthPercentage {
        match value.unpack() {
            stylo::UnpackedLengthPercentage::Length(px) => LengthPercentage::length(px.px()),
            stylo::UnpackedLengthPercentage::Percentage(fraction) => {
                LengthPercentage::percent(fraction.0)
            }
            stylo::UnpackedLengthPercentage::Calc(_) => LengthPercentage::calc(self.insert(value)),
        }
    }

    fn fallback(&mut self, value: &stylo::Size) -> LengthPercentageAuto {
        match value {
            stylo::Size::LengthPercentage(value) => self.length(&value.0).into(),
            _ => LengthPercentageAuto::AUTO,
        }
    }

    fn max(&mut self, value: &stylo::MaxSize) -> LengthPercentageAuto {
        match value {
            stylo::MaxSize::LengthPercentage(value) => self.length(&value.0).into(),
            _ => LengthPercentageAuto::AUTO,
        }
    }

    fn dimension(&mut self, value: &stylo::Size) -> Dimension {
        match value {
            stylo::Size::LengthPercentage(value) => self.length(&value.0).into(),
            stylo::Size::Auto => Dimension::AUTO,
            stylo::Size::MinContent => Dimension::min_content(),
            stylo::Size::MaxContent => Dimension::max_content(),
            stylo::Size::FitContent => Dimension::fit_content(),
            stylo::Size::FitContentFunction(limit) => match limit.0.unpack() {
                stylo::UnpackedLengthPercentage::Length(px) => Dimension::fit_content_px(px.px()),
                stylo::UnpackedLengthPercentage::Percentage(fraction) => {
                    Dimension::fit_content_percent(fraction.0)
                }
                stylo::UnpackedLengthPercentage::Calc(_) => {
                    Dimension::fit_content_calc(self.insert(&limit.0))
                }
            },
            stylo::Size::Stretch | stylo::Size::WebkitFillAvailable => Dimension::stretch(),
            _ => Dimension::AUTO,
        }
    }

    fn margin(&mut self, value: &stylo::MarginVal) -> LengthPercentageAuto {
        match value {
            stylo::MarginVal::LengthPercentage(value) => self.length(value).into(),
            _ => LengthPercentageAuto::AUTO,
        }
    }

    fn inset(&mut self, value: &stylo::InsetVal) -> LengthPercentageAuto {
        match value {
            stylo::InsetVal::LengthPercentage(value) => self.length(value).into(),
            _ => LengthPercentageAuto::AUTO,
        }
    }

    fn gap(&mut self, value: &stylo::Gap) -> LengthPercentage {
        match value {
            stylo::Gap::LengthPercentage(value) => self.length(&value.0),
            stylo::Gap::Normal => LengthPercentage::ZERO,
        }
    }

    fn tracks(
        &mut self,
        value: &stylo::GridTemplateComponent,
    ) -> Vec<taffy::GridTemplateComponent<Atom>> {
        let stylo::GenericGridTemplateComponent::TrackList(list) = value else {
            return Vec::new();
        };
        list.values
            .iter()
            .map(|track| match track {
                stylo::TrackListValue::TrackSize(size) => {
                    taffy::GridTemplateComponent::Single(self.track(size))
                }
                stylo::TrackListValue::TrackRepeat(repeat) => {
                    taffy::GridTemplateComponent::Repeat(taffy::GridTemplateRepetition {
                        count: track_repeat(repeat.count),
                        tracks: repeat
                            .track_sizes
                            .iter()
                            .map(|size| self.track(size))
                            .collect(),
                        line_names: repeat
                            .line_names
                            .iter()
                            .map(|names| names.iter().map(|name| name.0.clone()).collect())
                            .collect(),
                    })
                }
            })
            .collect()
    }

    fn track(
        &mut self,
        value: &stylo::TrackSize<stylo::LengthPercentage>,
    ) -> taffy::TrackSizingFunction {
        use stylo::{TrackBreadth as B, TrackSize as S};
        use taffy::{MaxTrackSizingFunction as Max, MinTrackSizingFunction as Min};
        let (min, max) = match value {
            S::Breadth(breadth) => (breadth, breadth),
            S::Minmax(min, max) => (min, max),
            S::FitContent(B::Breadth(length)) => {
                return taffy::MinMax {
                    min: Min::AUTO,
                    max: Max::fit_content(self.length(length)),
                };
            }
            S::FitContent(_) => (&B::Auto, &B::Auto),
        };
        let min = match min {
            B::Breadth(length) => self.length(length).into(),
            B::MinContent => Min::MIN_CONTENT,
            B::MaxContent => Min::MAX_CONTENT,
            B::Auto | B::Flex(_) => Min::AUTO,
        };
        let max = match max {
            B::Breadth(length) => self.length(length).into(),
            B::MinContent => Max::MIN_CONTENT,
            B::MaxContent => Max::MAX_CONTENT,
            B::Auto => Max::AUTO,
            B::Flex(fraction) => Max::from_fr(fraction.0),
        };
        taffy::MinMax { min, max }
    }

    pub(crate) fn style(&mut self, css: &stylo::ComputedValues) -> taffy::Style<Atom> {
        let display = *css.get_display();
        let pos = css.get_position();
        let padding = css.get_padding();
        let margin = css.get_margin();
        let border = css.get_border();
        let position_value = position(css.get_box().position);
        let direction_value = *css.get_direction();
        let rtl = direction_value == stylo::Direction::Rtl;
        let display_value = match display.inside() {
            stylo::DisplayInside::Flex => taffy::Display::Flex,
            stylo::DisplayInside::Grid | stylo::DisplayInside::Table => taffy::Display::Grid,
            stylo::DisplayInside::FlowRoot => taffy::Display::FlowRoot,
            stylo::DisplayInside::None => taffy::Display::None,
            _ => taffy::Display::Block,
        };
        let border_size = |width: &stylo::BorderSideWidth, edge: stylo::BorderStyle| {
            LengthPercentage::length(if edge.none_or_hidden() {
                0.0
            } else {
                width.0.to_f32_px()
            })
        };
        let line_names = |component: &stylo::GridTemplateComponent| match component {
            stylo::GenericGridTemplateComponent::TrackList(list) => list
                .line_names
                .iter()
                .map(|names| names.iter().map(|name| name.0.clone()).collect())
                .collect(),
            _ => Vec::new(),
        };
        let areas = match &pos.grid_template_areas {
            stylo::GridTemplateAreas::None => None,
            stylo::GridTemplateAreas::Areas(areas) => Some(taffy::GridTemplateAreas {
                areas: areas.0.areas.iter().map(grid_template_area).collect(),
                row_count: saturating_u16(areas.0.strings.len() as i32),
                column_count: saturating_u16(areas.0.width as i32),
            }),
        };

        taffy::Style {
            display: display_value,
            position: position_value,
            direction: direction(direction_value),
            box_sizing: box_sizing(*css.get_box_sizing()),
            contain: contain(*css.get_contain(), display),
            item_is_table: display.inside() == stylo::DisplayInside::Table,
            overflow: taffy::Point {
                x: overflow(*css.get_overflow_x()),
                y: overflow(*css.get_overflow_y()),
            },
            size: taffy::Size {
                width: self.dimension(&pos.width),
                height: self.dimension(&pos.height),
            },
            min_size: taffy::Size {
                width: self.fallback(&pos.min_width),
                height: self.fallback(&pos.min_height),
            },
            max_size: taffy::Size {
                width: self.max(&pos.max_width),
                height: self.max(&pos.max_height),
            },
            aspect_ratio: aspect_ratio(pos.aspect_ratio),
            inset: taffy::Rect {
                left: self.inset(&pos.left),
                right: self.inset(&pos.right),
                top: self.inset(&pos.top),
                bottom: self.inset(&pos.bottom),
            },
            margin: taffy::Rect {
                left: self.margin(&margin.margin_left),
                right: self.margin(&margin.margin_right),
                top: self.margin(&margin.margin_top),
                bottom: self.margin(&margin.margin_bottom),
            },
            padding: taffy::Rect {
                left: self.length(&padding.padding_left.0),
                right: self.length(&padding.padding_right.0),
                top: self.length(&padding.padding_top.0),
                bottom: self.length(&padding.padding_bottom.0),
            },
            border: taffy::Rect {
                left: border_size(&border.border_left_width, border.border_left_style),
                right: border_size(&border.border_right_width, border.border_right_style),
                top: border_size(&border.border_top_width, border.border_top_style),
                bottom: border_size(&border.border_bottom_width, border.border_bottom_style),
            },
            gap: taffy::Size {
                width: self.gap(&pos.column_gap),
                height: self.gap(&pos.row_gap),
            },
            align_content: content_alignment(pos.align_content, display),
            justify_content: justify_content(
                pos.justify_content,
                pos.flex_direction,
                direction_value,
                display,
            ),
            align_items: default_item_alignment(pos.align_items.0, false),
            align_self: item_alignment(pos.align_self.0, false),
            justify_items: default_item_alignment((pos.justify_items.computed.0).0, rtl),
            justify_self: item_alignment(pos.justify_self.0, rtl),
            text_align: text_align(*css.get_text_align()),
            flex_direction: flex_direction(pos.flex_direction),
            flex_wrap: flex_wrap(pos.flex_wrap),
            flex_grow: pos.flex_grow.0,
            flex_shrink: pos.flex_shrink.0,
            flex_basis: match &pos.flex_basis {
                stylo::FlexBasis::Content => Dimension::max_content(),
                stylo::FlexBasis::Size(size) => self.dimension(size),
            },
            grid_auto_flow: grid_auto_flow(pos.grid_auto_flow),
            grid_template_rows: self.tracks(&pos.grid_template_rows),
            grid_template_columns: self.tracks(&pos.grid_template_columns),
            grid_template_row_names: line_names(&pos.grid_template_rows),
            grid_template_column_names: line_names(&pos.grid_template_columns),
            grid_template_areas: areas,
            grid_auto_rows: pos
                .grid_auto_rows
                .0
                .iter()
                .map(|size| self.track(size))
                .collect(),
            grid_auto_columns: pos
                .grid_auto_columns
                .0
                .iter()
                .map(|size| self.track(size))
                .collect(),
            grid_row: taffy::Line {
                start: grid_line(&pos.grid_row_start),
                end: grid_line(&pos.grid_row_end),
            },
            grid_column: taffy::Line {
                start: grid_line(&pos.grid_column_start),
                end: grid_line(&pos.grid_column_end),
            },
            ..Default::default()
        }
    }
}
