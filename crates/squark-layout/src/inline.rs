// SPDX-License-Identifier: MIT
use std::{collections::HashMap, ops::Range, rc::Rc};

use parley::{AlignmentOptions, InlineBox, InlineBoxKind, Layout as TextLayout};
use squark_text::{EditText, TextBrush, TextPositions};
use squark_types::{NodeId, fragment::TextRange};
use style::values::{computed::TextTransform, generics::text::GenericTextIndent};
use taffy::prelude::TaffyAuto;
use taffy::{
    AvailableSpace, Baselines, Layout, LayoutInput, LayoutOutput, LayoutPartialTree, OofCandidates,
    ResolveOrZero, RunMode, Size,
};

use crate::{BoxTree, Content, Css, Id};

pub(crate) enum Token {
    Text {
        owner: NodeId,
        style_owner: NodeId,
        css: Css,
        units: Box<[u16]>,
    },
    Break {
        owner: NodeId,
        css: Css,
    },
    Box(Id),
    Edge {
        id: Id,
        start: bool,
    },
    Positioned(Id),
    Bidi(&'static str),
}

#[derive(Clone, Debug)]
struct Segment {
    range: Range<usize>,
    owner: NodeId,
    css: Css,
}

/// The DOM text behind one character of the layout text. The byte offset
/// is 32-bit because paragraphs keep their sources after layout for range
/// and caret queries.
#[derive(Debug)]
pub(crate) struct Source {
    pub byte: u32,
    pub range: TextRange,
}

pub(crate) struct Inline {
    text: String,
    segments: Vec<Segment>,
    sources: Vec<Source>,
    objects: Vec<(usize, Id, InlineBoxKind)>,
    single_line: bool,
    is_control: bool,
    pub(crate) layout: Option<Rc<TextLayout<TextBrush>>>,
    unbroken: Option<Rc<TextLayout<TextBrush>>>,
    pub(crate) overflow_marker: Option<OverflowMarker>,
    pub(crate) edit: Option<(EditText, bool)>,
}

pub(crate) struct OverflowMarker {
    pub width: f32,
    pub layout: TextLayout<TextBrush>,
}

#[derive(Debug, Default)]
pub(crate) struct LineCache {
    entries: HashMap<NodeId, CachedLines>,
    shapes: ShapingCache,
}

type RunStyle = parley::TextStyle<'static, 'static, TextBrush>;

#[derive(Debug, PartialEq)]
struct ShapingStyle {
    defaults: RunStyle,
    spans: Vec<(Range<usize>, RunStyle)>,
    direction: parley::BaseDirection,
}

#[derive(Debug, Default)]
struct ShapingCache(HashMap<String, ShapedText>);

#[derive(Debug)]
struct ShapedText {
    style: ShapingStyle,
    layout: Rc<TextLayout<TextBrush>>,
    used: bool,
}

impl ShapingCache {
    fn get_or_shape(
        &mut self,
        text: &str,
        style: ShapingStyle,
        build: impl FnOnce(&ShapingStyle) -> TextLayout<TextBrush>,
    ) -> Rc<TextLayout<TextBrush>> {
        if let Some(shaped) = self.0.get_mut(text)
            && shaped.style == style
        {
            shaped.used = true;
            return Rc::clone(&shaped.layout);
        }
        let layout = Rc::new(build(&style));
        // Keep small reused paragraphs through the next layout, bounded
        // independently of the number of elements in the document.
        if self.0.len() < 2048 {
            self.0.insert(
                text.to_owned(),
                ShapedText {
                    style,
                    layout: Rc::clone(&layout),
                    used: true,
                },
            );
        }
        layout
    }
}

#[derive(Debug, Default)]
struct CachedLines {
    used: bool,
    variants: Vec<LineVariant>,
}

#[derive(Debug)]
struct LineVariant {
    text: String,
    css: Css,
    segments: Vec<Segment>,
    single_line: bool,
    space: AvailableSpace,
    basis: Option<f32>,
    layout: Rc<TextLayout<TextBrush>>,
}

impl LineCache {
    pub fn finish_layout(&mut self) {
        self.entries
            .retain(|_, lines| std::mem::take(&mut lines.used));
        self.shapes
            .0
            .retain(|_, shaped| std::mem::take(&mut shaped.used));
    }

    fn layout(
        &mut self,
        owner: NodeId,
        inline: &Inline,
        css: &Css,
        space: AvailableSpace,
        basis: Option<f32>,
        build: impl FnOnce(&mut ShapingCache) -> TextLayout<TextBrush>,
    ) -> Rc<TextLayout<TextBrush>> {
        // Atomic inline boxes carry node IDs from the current Taffy tree.
        if !inline.objects.is_empty() || inline.text.is_empty() || owner == NodeId::NONE {
            return Rc::new(build(&mut self.shapes));
        }
        let lines = self.entries.entry(owner).or_default();
        lines.used = true;
        if let Some(hit) = lines.variants.iter().position(|entry| {
            entry.space == space
                && entry.basis == basis
                && entry.single_line == inline.single_line
                && style::servo_arc::Arc::ptr_eq(&entry.css, css)
                && entry.text == inline.text
                && entry.segments.len() == inline.segments.len()
                && entry.segments.iter().zip(&inline.segments).all(|(a, b)| {
                    a.range == b.range
                        && a.owner == b.owner
                        && style::servo_arc::Arc::ptr_eq(&a.css, &b.css)
                })
        }) {
            let entry = lines.variants.remove(hit);
            let layout = Rc::clone(&entry.layout);
            lines.variants.push(entry);
            return layout;
        }
        let layout = Rc::new(build(&mut self.shapes));
        // Taffy measures the same context at minimum, maximum and assigned widths.
        if lines.variants.len() == 3 {
            lines.variants.remove(0);
        }
        lines.variants.push(LineVariant {
            text: inline.text.clone(),
            css: css.clone(),
            segments: inline.segments.clone(),
            single_line: inline.single_line,
            space,
            basis,
            layout: Rc::clone(&layout),
        });
        layout
    }
}

impl Inline {
    pub(crate) fn new(tokens: Vec<Token>) -> Self {
        Self::build(tokens, false, false)
    }

    pub(crate) fn control(owner: NodeId, css: Css, units: Box<[u16]>, single_line: bool) -> Self {
        Self::build(
            vec![Token::Text {
                owner,
                style_owner: owner,
                css,
                units,
            }],
            single_line,
            true,
        )
    }

    fn build(tokens: Vec<Token>, single_line: bool, is_control: bool) -> Self {
        let mut result = Self {
            text: String::new(),
            segments: Vec::new(),
            sources: Vec::new(),
            objects: Vec::new(),
            single_line,
            is_control,
            layout: None,
            unbroken: None,
            overflow_marker: None,
            edit: None,
        };
        let mut space = None;
        let mut at_word_start = true;
        for token in tokens {
            match token {
                Token::Text {
                    owner,
                    style_owner,
                    css,
                    units,
                } => {
                    use style::computed_values::white_space_collapse::T as Collapse;
                    let collapse = if single_line {
                        Collapse::PreserveSpaces
                    } else {
                        css.get_inherited_text().white_space_collapse
                    };
                    let transform = css.get_inherited_text().text_transform;
                    let mut offset = 0;
                    let mut chars = char::decode_utf16(units.iter().copied()).peekable();
                    while let Some(ch) = chars.next() {
                        let ch = ch.unwrap_or(char::REPLACEMENT_CHARACTER);
                        let start_utf16 = offset;
                        offset += ch.len_utf16() as u32;
                        let ch = if ch == '\r' {
                            if chars.peek() == Some(&Ok('\n')) {
                                chars.next();
                                offset += 1;
                            }
                            '\n'
                        } else {
                            ch
                        };
                        let source = TextRange {
                            node: owner,
                            start_utf16,
                            end_utf16: offset,
                        };
                        let collapsible = match collapse {
                            Collapse::Collapse => matches!(ch, ' ' | '\t' | '\n'),
                            Collapse::PreserveBreaks => matches!(ch, ' ' | '\t'),
                            _ => false,
                        };
                        if collapsible {
                            if !result.text.is_empty() && !result.text.ends_with('\n') {
                                space.get_or_insert((style_owner, css.clone(), source));
                            }
                            at_word_start = true;
                            continue;
                        }
                        if ch == '\n' && collapse != Collapse::PreserveSpaces {
                            space = None;
                        } else if let Some((owner, css, source)) = space.take() {
                            result.push(" ", owner, &css, source);
                        }
                        let ch =
                            if collapse == Collapse::PreserveSpaces && matches!(ch, '\n' | '\t') {
                                ' '
                            } else {
                                ch
                            };
                        if transform.is_empty() {
                            result.push(ch.encode_utf8(&mut [0; 4]), style_owner, &css, source);
                            at_word_start = !ch.is_alphanumeric();
                            continue;
                        }
                        let transformed: String = if transform.contains(TextTransform::UPPERCASE)
                            || (at_word_start && transform.contains(TextTransform::CAPITALIZE))
                        {
                            ch.to_uppercase().collect()
                        } else if transform.contains(TextTransform::LOWERCASE) {
                            ch.to_lowercase().collect()
                        } else {
                            ch.to_string()
                        };
                        result.push(&transformed, style_owner, &css, source);
                        at_word_start = !ch.is_alphanumeric();
                    }
                }
                Token::Break { owner, css } => {
                    space = None;
                    result.push(
                        "\n",
                        owner,
                        &css,
                        TextRange {
                            node: owner,
                            start_utf16: 0,
                            end_utf16: 0,
                        },
                    );
                    at_word_start = true;
                }
                Token::Bidi(control) => result.text.push_str(control),
                Token::Positioned(id) => {
                    result
                        .objects
                        .push((result.text.len(), id, InlineBoxKind::OutOfFlow));
                }
                Token::Edge { id, start } => {
                    if start && let Some((owner, css, source)) = space.take() {
                        result.push(" ", owner, &css, source);
                    }
                    result
                        .objects
                        .push((result.text.len(), id, InlineBoxKind::InFlow));
                    result.text.push('\u{fffc}');
                }
                Token::Box(id) => {
                    if let Some((owner, css, source)) = space.take() {
                        result.push(" ", owner, &css, source);
                    }
                    // The containing block supplies the style of the object replacement character.
                    // Its source range is empty because this is an element rather than a text node.
                    let index = result.text.len();
                    result.objects.push((index, id, InlineBoxKind::InFlow));
                    result.text.push('\u{fffc}');
                    at_word_start = true;
                }
            }
        }
        result
    }

    fn push(&mut self, value: &str, owner: NodeId, css: &Css, source: TextRange) {
        let start = self.text.len();
        self.text.push_str(value);
        self.sources
            .extend(value.char_indices().map(|(offset, _)| Source {
                byte: (start + offset) as u32,
                range: source,
            }));
        if let Some(last) = self.segments.last_mut()
            && last.range.end == start
            && last.owner == owner
            && style::servo_arc::Arc::ptr_eq(&last.css, css)
        {
            last.range.end = self.text.len();
        } else {
            self.segments.push(Segment {
                range: start..self.text.len(),
                owner,
                css: css.clone(),
            });
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.objects.is_empty()
    }

    /// Hands over the layout text and its sources once fragments are built,
    /// so queries can map DOM text to the layout. Form control text maps to
    /// the control's value rather than to Text nodes, so it keeps neither.
    pub(crate) fn take_sources(&mut self) -> Option<(String, Vec<Source>)> {
        (!self.is_control && !self.sources.is_empty()).then(|| {
            (
                std::mem::take(&mut self.text),
                std::mem::take(&mut self.sources),
            )
        })
    }

    pub(crate) fn line_count(&self, layout: &TextLayout<TextBrush>) -> usize {
        let count = layout.len();
        // A final preserved segment break ends its line. Only an editing
        // control needs an additional empty line to accommodate its caret.
        if !self.is_control
            && count > 1
            && self.text.ends_with('\n')
            && layout
                .get(count - 1)
                .is_some_and(|line| line.text_range().is_empty())
        {
            count - 1
        } else {
            count
        }
    }

    pub(crate) fn take_text_positions(&mut self) -> Option<TextPositions> {
        self.edit.take().map(|(edit, placeholder)| {
            TextPositions::new(
                edit,
                self.sources
                    .iter()
                    .map(|source| (source.range.start_utf16, source.byte as usize)),
                self.text.len(),
                placeholder,
            )
        })
    }

    fn shares_shaping(&self) -> bool {
        !self.is_control
            && self.objects.is_empty()
            && !self.text.is_empty()
            && self.text.len() <= 256
    }

    pub(crate) fn style_owner(&self, brush: NodeId, container: NodeId) -> NodeId {
        if self.shares_shaping() {
            brush
                .raw()
                .checked_sub(1)
                .and_then(|index| self.segments.get(index as usize))
                .map_or(container, |segment| segment.owner)
        } else {
            brush
        }
    }

    pub(crate) fn style_at(&self, byte: usize) -> Option<&Css> {
        let index = self
            .segments
            .partition_point(|segment| segment.range.end <= byte);
        let segment = self.segments.get(index)?;
        segment.range.contains(&byte).then_some(&segment.css)
    }

    pub(crate) fn boxes(&self) -> impl Iterator<Item = Id> + '_ {
        self.objects.iter().map(|&(_, id, _)| id)
    }

    pub(crate) fn ranges(&self, bytes: Range<usize>) -> Vec<TextRange> {
        let start = self
            .sources
            .partition_point(|source| (source.byte as usize) < bytes.start);
        let end = self
            .sources
            .partition_point(|source| (source.byte as usize) < bytes.end);
        let mut ranges: Vec<TextRange> = Vec::new();
        for source in &self.sources[start..end] {
            let range = source.range;
            if let Some(last) = ranges.last_mut()
                && last.node == range.node
                && last.end_utf16 >= range.start_utf16
            {
                last.end_utf16 = last.end_utf16.max(range.end_utf16);
            } else {
                ranges.push(range);
            }
        }
        ranges
    }
}

impl BoxTree<'_> {
    pub(crate) fn inline_layout(&mut self, id: Id, input: LayoutInput) -> LayoutOutput {
        let style = self[id].style.clone();
        let calcs = Rc::clone(&self.calcs);
        let mut baselines = Baselines::NONE;
        let mut out_of_flow = OofCandidates::NONE;
        let mut output = taffy::compute_leaf_layout(
            input,
            &style,
            |handle, basis| calcs.resolve(handle, basis),
            |_, available| {
                let result = self.format_lines(id, input, available);
                baselines = result.baselines;
                out_of_flow = result.out_of_flow;
                result.size
            },
        );
        output.baselines = baselines;
        output.oof_candidates = out_of_flow;
        output
    }

    fn format_lines(
        &mut self,
        id: Id,
        input: LayoutInput,
        available: Size<AvailableSpace>,
    ) -> InlineOutput {
        let content = std::mem::replace(&mut self[id].content, Content::Container);
        let Content::Inline(mut inline) = content else {
            unreachable!("inline layout expected")
        };
        let css = self[id].css.clone().expect("inline formatting style");
        let owner = self[id].owner;
        let width = available.width.into_option();
        let basis = width.or(input.parent_size.width);
        let mut objects = Vec::with_capacity(inline.objects.len());
        for &(index, child, kind) in &inline.objects {
            if kind == InlineBoxKind::OutOfFlow {
                objects.push((
                    InlineBox {
                        id: usize::from(child) as u64,
                        kind,
                        index,
                        width: 0.0,
                        height: 0.0,
                        baseline: None,
                        vertical_align: parley::VerticalAlign::BASELINE,
                    },
                    Layout::new(),
                    OofCandidates::NONE,
                    0.0,
                ));
                continue;
            }
            let style = self[child].style.clone();
            let margin = style
                .margin
                .resolve_or_zero(basis, |calc, basis| self.calcs.resolve(calc, basis));
            if let Content::InlineEdge(left) = self[child].content {
                let resolve = |calc, basis| self.calcs.resolve(calc, basis);
                let padding = style.padding.resolve_or_zero(basis, resolve);
                let border = style.border.resolve_or_zero(basis, resolve);
                let width = if left {
                    padding.left + border.left + margin.left
                } else {
                    padding.right + border.right + margin.right
                };
                objects.push((
                    InlineBox {
                        id: usize::from(child) as u64,
                        kind,
                        index,
                        width: width.max(0.0),
                        height: 0.0,
                        baseline: Some(0.0),
                        vertical_align: parley::VerticalAlign::BASELINE,
                    },
                    Layout {
                        padding,
                        border,
                        margin,
                        ..Layout::new()
                    },
                    OofCandidates::NONE,
                    0.0,
                ));
                continue;
            }
            let available = Size {
                width: available
                    .width
                    .map_definite_value(|width| (width - margin.left - margin.right).max(0.0)),
                height: AvailableSpace::MaxContent,
            };
            let mut constraints = LayoutInput {
                run_mode: RunMode::ComputeSize,
                parent_size: Size {
                    width: basis,
                    height: None,
                },
                available_space: available,
                ..LayoutInput::HIDDEN
            };
            if style.size.width == taffy::Dimension::AUTO
                && !style.item_is_replaced
                && let AvailableSpace::Definite(width) = available.width
            {
                constraints.available_space.width = AvailableSpace::MinContent;
                let min = self.compute_child_layout(child, constraints).size.width;
                constraints.available_space.width = AvailableSpace::MaxContent;
                let max = self.compute_child_layout(child, constraints).size.width;
                constraints.known_dimensions.width = Some(width.min(max).max(min));
            }
            let visible_overflow = matches!(self[child].content, Content::Control(_))
                || (style.overflow.x == taffy::Overflow::Visible
                    && style.overflow.y == taffy::Overflow::Visible);
            // A size-only Taffy pass can omit an inline block's natural
            // baseline. Grid track sizing needs the same baseline used by
            // final layout, or it measures a taller line at the margin edge.
            constraints.run_mode = if visible_overflow
                && matches!(self[child].content, Content::Container | Content::Table(_))
            {
                RunMode::PerformLayout
            } else {
                input.run_mode
            };
            constraints.available_space = available;
            let computed = self.compute_child_layout(child, constraints);
            let baseline = self[child]
                .inline_block_baseline(computed.baselines)
                .filter(|_| visible_overflow)
                .map(|baseline| baseline + margin.top);
            let css = self[child].css.as_ref().expect("atomic inline style");
            let resolve = |calc, basis| self.calcs.resolve(calc, basis);
            let mut vertical_align = squark_text::vertical_align(css);
            let parent_size = self[id]
                .css
                .as_ref()
                .expect("inline formatting style")
                .get_font()
                .font_size
                .used_size
                .0
                .px();
            let fractional_shift = match vertical_align.shift {
                parley::BaselineShift::Sub => {
                    Some(-((parent_size / 5.0 * 64.0).trunc() / 64.0 + 1.0))
                }
                parley::BaselineShift::Super => {
                    Some((parent_size / 3.0 * 64.0).trunc() / 64.0 + 1.0)
                }
                _ => None,
            };
            let correction = fractional_shift.map_or(0.0, |shift| {
                vertical_align.shift = parley::BaselineShift::Length(shift);
                // Parley rounds alignment offsets along with the line metrics.
                // Chromium uses fractional sub/sup offsets on that baseline.
                shift.round() - shift
            });
            objects.push((
                InlineBox {
                    id: usize::from(child) as u64,
                    kind: InlineBoxKind::InFlow,
                    index,
                    width: if self[child].marker_outside {
                        0.0
                    } else {
                        (computed.size.width + margin.left + margin.right).max(0.0)
                    },
                    height: (computed.size.height + margin.top + margin.bottom).max(0.0),
                    baseline,
                    vertical_align,
                },
                Layout {
                    size: computed.size,
                    padding: style.padding.resolve_or_zero(basis, resolve),
                    border: style.border.resolve_or_zero(basis, resolve),
                    margin,
                    scrollable_overflow_rect: computed.scrollable_overflow_rect,
                    ..Layout::new()
                },
                computed.oof_candidates,
                correction,
            ));
        }
        let single_line = inline.single_line;
        let text_style = |style_owner, css| {
            let mut style = squark_text::style(style_owner, css);
            if single_line {
                style.text_wrap_mode = parley::TextWrapMode::NoWrap;
                style.white_space_collapse = parley::WhiteSpaceCollapse::Preserve;
            }
            if style_owner == owner {
                style.vertical_align = parley::VerticalAlign::BASELINE;
            }
            style
        };
        let reuse = inline.shares_shaping();
        let mut unbroken = inline.unbroken.take();
        let fonts = &mut self.fonts;
        let layout = self
            .lines
            .layout(owner, &inline, &css, available.width, basis, |shapes| {
                let mut build = || {
                    let spans = inline
                        .segments
                        .iter()
                        .enumerate()
                        .map(|(index, segment)| {
                            let mut style = text_style(segment.owner, &segment.css);
                            if reuse {
                                // A shaped layout uses ordinal brushes. Emission
                                // maps them back to this paragraph's node IDs.
                                style.brush = TextBrush(NodeId::from_raw(index as u32 + 1));
                            }
                            let text = &inline.text[segment.range.clone()];
                            (segment.range.clone(), fonts.with_web_fonts(text, style))
                        })
                        .collect();
                    let mut defaults = fonts.primary_style(text_style(owner, &css));
                    // The default covers the line strut and characters without
                    // a text segment, including atomic inline placeholders.
                    // Letter spacing applies to text runs, not the boxes.
                    defaults.letter_spacing = 0.0;
                    if reuse {
                        defaults.brush = TextBrush(NodeId::NONE);
                    }
                    let style = ShapingStyle {
                        defaults,
                        spans,
                        direction: squark_text::base_direction(
                            *css.get_direction(),
                            *css.get_unicode_bidi(),
                        ),
                    };
                    let mut shape = |style: &ShapingStyle| {
                        let mut builder = fonts.layout.style_run_builder(
                            &mut fonts.context,
                            &inline.text,
                            1.0,
                            true,
                        );
                        let default = builder.push_style(style.defaults.clone());
                        builder.set_base_direction(style.direction);
                        let mut cursor = 0;
                        for (range, style) in &style.spans {
                            if cursor < range.start {
                                builder.push_style_run(default, cursor..range.start);
                            }
                            cursor = range.end;
                            let style = builder.push_style(style.clone());
                            builder.push_style_run(style, range.clone());
                        }
                        if cursor < inline.text.len() {
                            builder.push_style_run(default, cursor..inline.text.len());
                        }
                        for (object, _, _, _) in &objects {
                            builder.push_inline_box(object.clone());
                        }
                        builder.build(&inline.text)
                    };
                    if reuse {
                        shapes.get_or_shape(&inline.text, style, shape)
                    } else {
                        Rc::new(shape(&style))
                    }
                };
                // Intrinsic widths and final layout use the same shaping.
                let mut layout = if objects.is_empty() {
                    unbroken.get_or_insert_with(build).as_ref().clone()
                } else {
                    Rc::unwrap_or_clone(build())
                };
                let text_align = css.get_inherited_text().text_align;
                layout.set_alignment(
                    squark_text::text_align(text_align),
                    AlignmentOptions {
                        last_line_alignment: squark_text::text_align_last(
                            *css.get_text_align_last(),
                        ),
                        ..Default::default()
                    },
                );
                let GenericTextIndent {
                    length,
                    hanging,
                    each_line,
                } = css.get_inherited_text().text_indent.clone();
                layout.set_text_indent(
                    length
                        .resolve(style::values::computed::CSSPixelLength::new(
                            basis.unwrap_or(0.0),
                        ))
                        .px(),
                    parley::IndentOptions { hanging, each_line },
                );
                let limit = match available.width {
                    AvailableSpace::Definite(value) => Some(value.max(0.0)),
                    AvailableSpace::MinContent => Some(layout.calculate_content_widths().min),
                    AvailableSpace::MaxContent => None,
                };
                layout.break_all_lines(limit);
                layout
            });
        inline.unbroken = unbroken;
        let line_count = inline.line_count(&layout);
        let last = line_count.checked_sub(1).and_then(|i| layout.get(i));
        let baselines = Baselines {
            first: layout.get(0).map(|line| line.metrics().baseline),
            last: last.as_ref().map(|line| line.metrics().baseline),
        };
        let mut out_of_flow = OofCandidates::NONE;
        if input.run_mode == RunMode::PerformLayout {
            let mut layouts: HashMap<_, _> = objects
                .into_iter()
                .map(|(object, layout, candidates, correction)| {
                    (object.id, (layout, candidates, correction))
                })
                .collect();
            for line in layout.lines() {
                for positioned in line.items() {
                    let parley::PositionedLayoutItem::InlineBox(item) = positioned else {
                        continue;
                    };
                    if item.kind == InlineBoxKind::OutOfFlow {
                        use taffy::{AxisStaticEdge, AxisStaticPosition, OofCandidate};
                        let child = Id::from(item.id as usize);
                        let position = self[child].style.position;
                        let side = if layout.is_rtl() {
                            AxisStaticEdge::End
                        } else {
                            AxisStaticEdge::Start
                        };
                        out_of_flow.push(OofCandidate {
                            node: child,
                            order: 0,
                            position,
                            static_position: taffy::Point {
                                x: AxisStaticPosition::from_edge(item.x, side),
                                y: AxisStaticPosition::from_edge(
                                    line.metrics().block_min_coord,
                                    AxisStaticEdge::Start,
                                ),
                            },
                        });
                        continue;
                    }
                    let child = Id::from(item.id as usize);
                    let (mut layout, mut candidates, correction) =
                        layouts.remove(&item.id).expect("inline object placed once");
                    layout.location.x = if self[child].marker_outside {
                        if self[id].style.direction.is_rtl() {
                            item.x + layout.margin.right
                        } else {
                            item.x - layout.size.width - layout.margin.right
                        }
                    } else {
                        item.x + layout.margin.left
                    };
                    layout.location.y = item.y + layout.margin.top + correction;
                    // Relative positioning moves an atomic inline after the
                    // line is laid out, as it moves a block.
                    let relative = crate::relative_offset(
                        &self[child].style,
                        Size {
                            width: basis,
                            height: None,
                        },
                        self[id].style.direction.is_rtl(),
                        &self.calcs,
                    );
                    layout.location.x += relative.x;
                    layout.location.y += relative.y;
                    candidates.translate(layout.location);
                    out_of_flow.append(&mut candidates);
                    self[child].layout = layout;
                }
            }
        }
        let mut measured_width = layout.width();
        if available.width == AvailableSpace::MaxContent
            && css.get_display().inside() == style::values::specified::box_::DisplayInside::Flex
            && measured_width > 0.0
        {
            // Round an anonymous flex item's preferred width outward. Flex
            // sizing adds and subtracts gaps, siblings and edges before the
            // line breaker sees that width again.
            measured_width = measured_width.next_up().next_up();
        }
        let size = Size {
            width: measured_width,
            height: if line_count == layout.len() {
                layout.height()
            } else {
                last.map_or(0.0, |line| line.metrics().block_max_coord)
            },
        };
        if input.run_mode == RunMode::PerformLayout {
            use style::values::specified::{box_::Overflow, text::TextOverflowSide};
            inline.overflow_marker = None;
            if *css.get_overflow_x() != Overflow::Visible
                && let Some(width) = width
                && layout
                    .lines()
                    .any(|line| line.metrics().advance - line.metrics().hanging_advance > width)
            {
                let overflow = css.get_text_overflow();
                let end = if !overflow.sides_are_logical && layout.is_rtl() {
                    &overflow.first
                } else {
                    &overflow.second
                };
                let marker = match end {
                    TextOverflowSide::Clip => "",
                    TextOverflowSide::Ellipsis => "…",
                    TextOverflowSide::String(value) => value.as_str(),
                };
                if !marker.is_empty() {
                    let fonts = &mut self.fonts;
                    let style = fonts.with_web_fonts(marker, text_style(owner, &css));
                    let mut builder =
                        fonts
                            .layout
                            .style_run_builder(&mut fonts.context, marker, 1.0, true);
                    let style = builder.push_style(style);
                    builder.push_style_run(style, ..);
                    let mut marker = builder.build(marker);
                    marker.break_all_lines(None);
                    inline.overflow_marker = Some(OverflowMarker {
                        width,
                        layout: marker,
                    });
                }
            }
            inline.layout = Some(layout);
        }
        self[id].content = Content::Inline(inline);
        InlineOutput {
            size,
            baselines,
            out_of_flow,
        }
    }
}

struct InlineOutput {
    size: Size<f32>,
    baselines: Baselines,
    out_of_flow: OofCandidates,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_only_matching_lines_from_the_previous_layout() {
        let mut cache = LineCache::default();
        let owner = NodeId::from_raw(1);
        let style = |size| {
            let mut font = style::properties::style_structs::Font::initial_values();
            font.set_line_height(style::values::computed::LineHeight::Number(
                style::values::generics::NonNegative(size),
            ));
            style::properties::ComputedValues::initial_values_with_font_override(font)
        };
        let css = style(1.0);
        let changed = style(2.0);
        let text = |value: &str, css: &Css| {
            Inline::control(owner, css.clone(), value.encode_utf16().collect(), true)
        };
        let width = AvailableSpace::Definite(60.0);
        let initial = text("Row 15", &css);
        let layout = cache.layout(owner, &initial, &css, width, None, |_| TextLayout::new());
        cache.finish_layout();

        let same = text("Row 15", &css);
        assert!(Rc::ptr_eq(
            &cache.layout(owner, &same, &css, width, None, |_| panic!(
                "expected reuse"
            )),
            &layout
        ));
        for (inline, width, basis) in [
            (text("Row 16", &css), width, None),
            (text("Row 15", &changed), width, None),
            (text("Row 15", &css), AvailableSpace::Definite(50.0), None),
            (text("Row 15", &css), width, Some(120.0)),
        ] {
            assert!(!Rc::ptr_eq(
                &cache.layout(owner, &inline, &css, width, basis, |_| TextLayout::new()),
                &layout
            ));
        }
        cache.finish_layout();
        assert!(cache.entries.contains_key(&owner));
        cache.finish_layout();
        assert!(cache.entries.is_empty());
    }
}
