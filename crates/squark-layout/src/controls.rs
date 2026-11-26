// SPDX-License-Identifier: MIT

use squark_mirror::{Document, InputKind};
use squark_style::Cascade;
use squark_text::{ControlMetrics, EditText, Fonts};
use squark_types::{
    NodeId,
    engine::{Direction, MeasureText},
    journal::ElementState as State,
};
use style::{
    selector_parser::PseudoElement,
    servo_arc::Arc,
    values::computed::{Appearance, LineHeight},
};
use taffy::{
    AvailableSpace, Baselines, Layout, LayoutInput, LayoutOutput, LayoutPartialTree, ResolveOrZero,
    RunMode, Size,
};

use crate::{BoxTree, Content, Css, Id, inline::Inline};

mod theme;
pub(crate) use theme::{box_chrome, broken_image, control_art, edit_fill};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    Checkbox,
    Radio,
    InputButton,
    Field(InputKind),
    Multiline,
    Menu,
    List,
}

#[derive(Debug)]
pub(crate) struct Control {
    pub kind: Kind,
    pub native: bool,
    intrinsic: Size<f32>,
    metrics: Option<ControlMetrics>,
    rows: Vec<Row>,
}

struct Label {
    owner: NodeId,
    style: Css,
    text: Box<[u16]>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Row {
    pub selected: bool,
    pub indent: f32,
}

pub(crate) struct Plan {
    control: Control,
    labels: Vec<Label>,
    edit: Option<(EditText, bool)>,
}

fn control_size(document: &Document, owner: NodeId, attr: &str, default: u32) -> u32 {
    document
        .attribute(owner, attr)
        .and_then(|s| String::from_utf16_lossy(s).trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

fn select_kind(document: &Document, owner: NodeId) -> Kind {
    if control_size(document, owner, "size", 1) > 1
        || document.attribute(owner, "multiple").is_some()
    {
        Kind::List
    } else {
        Kind::Menu
    }
}

pub(crate) fn menu_style(document: &Document, owner: NodeId, mut css: Css) -> Css {
    // Native dropdowns use their platform line height. List boxes and selects
    // styled with appearance:none use the authored line height.
    if *css.get_line_height() != LineHeight::Normal
        && *css.get_appearance() == Appearance::Auto
        && document.html_element(owner) == Some("select")
        && select_kind(document, owner) == Kind::Menu
    {
        Arc::make_mut(&mut css)
            .mutate_font()
            .set_line_height(LineHeight::Normal);
    }
    css
}

pub(crate) fn build(
    document: &Document,
    cascade: &Cascade<'_>,
    fonts: &mut Fonts,
    owner: NodeId,
    css: &Css,
) -> Option<Plan> {
    let native = *css.get_appearance() != Appearance::None;
    let number = |attr, default| control_size(document, owner, attr, default);
    let kind = match document.html_element(owner)? {
        "input" => match document.input_kind(owner)? {
            InputKind::Other => return None,
            InputKind::Checkbox => Kind::Checkbox,
            InputKind::Radio => Kind::Radio,
            InputKind::Button | InputKind::Submit | InputKind::Reset => Kind::InputButton,
            field => Kind::Field(field),
        },
        "select" => select_kind(document, owner),
        "textarea" => Kind::Multiline,
        _ => return None,
    };
    let mut control = Control {
        kind,
        native,
        intrinsic: Size::ZERO,
        metrics: None,
        rows: Vec::new(),
    };
    let mut labels = Vec::new();
    let mut edit = None;
    if matches!(kind, Kind::Checkbox | Kind::Radio) {
        if native {
            control.intrinsic = Size {
                width: 13.0,
                height: 13.0,
            };
        }
        return Some(Plan {
            control,
            labels,
            edit,
        });
    }

    let menu_css = (kind == Kind::Menu).then(|| menu_style(document, owner, css.clone()));
    let css = menu_css.as_ref().unwrap_or(css);
    let metrics = fonts.control_metrics(css);
    control.metrics = Some(metrics);
    control.intrinsic.height = metrics.height;

    let mut label = |owner, style, text, selected, indent| {
        labels.push(Label { owner, style, text });
        control.rows.push(Row { selected, indent });
    };
    match kind {
        Kind::InputButton => {
            let value = document
                .attribute(owner, "value")
                .map(<[u16]>::to_vec)
                .unwrap_or_else(|| match document.input_kind(owner) {
                    Some(InputKind::Submit) => "Submit".encode_utf16().collect(),
                    Some(InputKind::Reset) => "Reset".encode_utf16().collect(),
                    _ => Vec::new(),
                });
            control.intrinsic.width = advance(fonts, css, &value);
            label(owner, css.clone(), value.into(), false, 0.0);
        }
        Kind::Field(field) => {
            let columns = if field == InputKind::Number {
                20
            } else {
                number("size", 20)
            };
            control.intrinsic.width =
                (metrics.average * (columns - 1) as f32 + metrics.maximum).ceil();
            let value = document
                .live_value(owner)
                .or_else(|| document.attribute(owner, "value"))
                .unwrap_or_default();
            let value = field_value(value, field);
            let (state, display) =
                editing_text(document, owner, &value, field == InputKind::Password);
            let placeholder =
                display.is_empty() && document.attribute(owner, "placeholder").is_some();
            let (text, style) = text_or_placeholder(document, cascade, owner, css, display);
            edit = Some((state, placeholder));
            label(owner, style, text, false, 0.0);
        }
        Kind::Multiline => {
            use style::values::specified::box_::Overflow;
            let scrollbar = if matches!(css.get_overflow_y(), Overflow::Auto | Overflow::Scroll) {
                15.0
            } else {
                0.0
            };
            control.intrinsic.width =
                (metrics.average * number("cols", 20) as f32).ceil() + scrollbar;
            control.intrinsic.height *= number("rows", 2) as f32;
            let value = document
                .live_value(owner)
                .map(<[u16]>::to_vec)
                .unwrap_or_else(|| text_content(document, owner));
            let (state, display) = editing_text(document, owner, &value, false);
            let placeholder =
                display.is_empty() && document.attribute(owner, "placeholder").is_some();
            let (text, style) = text_or_placeholder(document, cascade, owner, css, display);
            edit = Some((state, placeholder));
            label(owner, style, text, false, 0.0);
        }
        Kind::Menu | Kind::List => {
            let mut options = Vec::new();
            collect_options(document, owner, false, &mut options);
            let max_width = options
                .iter()
                .filter(|option| kind == Kind::List || !option.group)
                .map(|option| {
                    advance(fonts, css, &option.label)
                        + if kind == Kind::List {
                            option.indent
                        } else {
                            0.0
                        }
                })
                .fold(0.0, f32::max);
            if kind == Kind::Menu {
                control.intrinsic.width = max_width.ceil() + if native { 20.0 } else { 0.0 };
                control.intrinsic.height += if native { 2.0 } else { 0.0 };
                let selected = options.iter().rfind(|option| option.selected).or_else(|| {
                    // Startup packs carry attributes. A live select write can
                    // intentionally clear selectedIndex and must stay empty.
                    document
                        .live_value(owner)
                        .is_none()
                        .then(|| options.iter().find(|o| !o.disabled && !o.group))
                        .flatten()
                });
                if let Some(option) = selected {
                    label(
                        owner,
                        css.clone(),
                        option.label.clone(),
                        false,
                        if native { 4.0 } else { 0.0 },
                    );
                }
            } else {
                let rows = number(
                    "size",
                    if document.attribute(owner, "multiple").is_some() {
                        4
                    } else {
                        1
                    },
                );
                control.intrinsic.width = max_width + 4.0;
                control.intrinsic.height = (metrics.height + 2.0) * rows as f32;
                for option in options {
                    let style = document
                        .computed_style(option.owner)
                        .unwrap_or_else(|| css.clone());
                    label(
                        option.owner,
                        style,
                        option.label,
                        option.selected,
                        2.0 + option.indent,
                    );
                }
            }
        }
        Kind::Checkbox | Kind::Radio => unreachable!(),
    }
    Some(Plan {
        control,
        labels,
        edit,
    })
}

fn editing_text(
    document: &Document,
    owner: NodeId,
    value: &[u16],
    password: bool,
) -> (EditText, Vec<u16>) {
    let state = document.text_edit(owner);
    let preedit = state.filter(|_| document.focused() == Some(owner));
    EditText::project(
        value,
        state.map_or(0..0, |state| state.start..state.end),
        state.is_some_and(|state| state.backward),
        preedit.map_or(&[], |state| &state.preedit),
        preedit.map_or(0, |state| state.cursor),
        password,
    )
}

fn field_value(units: &[u16], kind: InputKind) -> Vec<u16> {
    let value: Vec<u16> = units
        .iter()
        .copied()
        .filter(|unit| !matches!(unit, 10 | 13))
        .collect();
    match kind {
        InputKind::Email | InputKind::Url => {
            let start = value
                .iter()
                .position(|unit| !html_space(*unit))
                .unwrap_or(value.len());
            let end = value
                .iter()
                .rposition(|unit| !html_space(*unit))
                .map_or(start, |i| i + 1);
            value[start..end].to_vec()
        }
        InputKind::Number => {
            let text = String::from_utf16_lossy(&value);
            if text.starts_with('+') || !text.parse::<f64>().is_ok_and(f64::is_finite) {
                Vec::new()
            } else {
                value
            }
        }
        _ => value,
    }
}

fn html_space(unit: u16) -> bool {
    matches!(unit, 9 | 10 | 12 | 13 | 32)
}

fn text_or_placeholder(
    document: &Document,
    cascade: &Cascade<'_>,
    owner: NodeId,
    css: &Css,
    value: Vec<u16>,
) -> (Box<[u16]>, Css) {
    if value.is_empty()
        && let Some(hint) = document
            .attribute(owner, "placeholder")
            .filter(|hint| !hint.is_empty())
    {
        let text = hint
            .iter()
            .copied()
            .filter(|u| !matches!(u, 10 | 13))
            .collect();
        let style = cascade
            .computed_style(document, owner, Some(&PseudoElement::Placeholder))
            .unwrap_or_else(|| css.clone());
        return (text, style);
    }
    (value.into(), css.clone())
}

fn advance(fonts: &mut Fonts, style: &Css, text: &[u16]) -> f32 {
    let style_values = squark_text::style(NodeId::NONE, style);
    let dir = if *style.get_direction() == style::computed_values::direction::T::Rtl {
        Direction::Rtl
    } else {
        Direction::Ltr
    };
    fonts
        .measure(
            MeasureText {
                font_css: "",
                text,
                direction: dir,
                letter_spacing: style_values.letter_spacing,
                word_spacing: style_values.word_spacing,
                font_environment: None,
            },
            style,
        )
        .width as f32
}

struct OptionLabel {
    owner: NodeId,
    label: Box<[u16]>,
    selected: bool,
    disabled: bool,
    group: bool,
    indent: f32,
}

fn collect_options(
    document: &Document,
    parent: NodeId,
    in_group: bool,
    labels: &mut Vec<OptionLabel>,
) {
    for &owner in document.children(parent).unwrap_or_default() {
        let has_group = document.html_element(owner) == Some("optgroup");
        match document.html_element(owner) {
            Some("option" | "optgroup") => {
                let default = if has_group {
                    Vec::new()
                } else {
                    text_content(document, owner)
                };
                let label = document.attribute(owner, "label").unwrap_or(&default);
                let mut units = Vec::new();
                let mut separator = false;
                for &unit in label {
                    if html_space(unit) {
                        separator = !units.is_empty();
                    } else {
                        if std::mem::take(&mut separator) {
                            units.push(32);
                        }
                        units.push(unit);
                    }
                }
                labels.push(OptionLabel {
                    owner,
                    label: units.into(),
                    selected: document.control_state(owner).bits() & State::SELECTED.bits() != 0,
                    disabled: document.control_state(owner).bits() & State::DISABLED.bits() != 0,
                    group: has_group,
                    indent: if in_group { 18.0 } else { 0.0 },
                });
                if has_group {
                    collect_options(document, owner, true, labels);
                }
            }
            Some("select") => {}
            _ => collect_options(document, owner, in_group, labels),
        }
    }
}

fn text_content(document: &Document, owner: NodeId) -> Vec<u16> {
    let mut text = Vec::new();
    let mut pending = vec![owner];
    while let Some(node) = pending.pop() {
        if document.is_text(node) {
            text.extend_from_slice(document.text(node).unwrap_or_default());
        } else if document.html_element(node) != Some("script") {
            pending.extend(
                document
                    .children(node)
                    .unwrap_or_default()
                    .iter()
                    .rev()
                    .copied(),
            );
        }
    }
    text
}

impl Plan {
    pub(crate) fn attach(
        self,
        tree: &mut BoxTree<'_>,
        owner: NodeId,
        css: Css,
        style: taffy::Style<style::Atom>,
    ) -> Id {
        use squark_types::fragment::FragmentKind;
        let single_line = self.control.kind != Kind::Multiline;
        let id = tree.add(
            owner,
            FragmentKind::Replaced,
            Some(css),
            style,
            Content::Control(Box::new(self.control)),
        );
        let mut edit = self.edit;
        for label in self.labels {
            let mut inline =
                Inline::control(label.owner, label.style.clone(), label.text, single_line);
            inline.edit = edit.take();
            let child = tree.add(
                label.owner,
                FragmentKind::Line,
                Some(label.style),
                taffy::Style {
                    display: taffy::Display::Block,
                    direction: tree[id].style.direction,
                    ..Default::default()
                },
                Content::Inline(Box::new(inline)),
            );
            tree[child].claims = taffy::ContainingBlockClaims::NONE;
            tree[id].children.push(child);
        }
        id
    }
}

pub(crate) fn compute(tree: &mut BoxTree<'_>, id: Id, input: LayoutInput) -> LayoutOutput {
    let Content::Control(control) = &tree[id].content else {
        unreachable!()
    };
    let (kind, native, intrinsic, metrics) = (
        control.kind,
        control.native,
        control.intrinsic,
        control.metrics,
    );
    let calcs = tree.calcs.clone();
    let resolve = |handle, basis| calcs.resolve(handle, basis);
    let style = &tree[id].style;
    let mut output = taffy::compute_leaf_layout(input, style, resolve, |_, _| intrinsic);
    let Some(metrics) = metrics else {
        return output;
    };
    let padding = style
        .padding
        .resolve_or_zero(input.parent_size.width, resolve);
    let border = style
        .border
        .resolve_or_zero(input.parent_size.width, resolve);
    let left = border.left + padding.left;
    let top = border.top + padding.top;
    let width = (output.size.width - left - border.right - padding.right).max(0.0);
    let height = (output.size.height - top - border.bottom - padding.bottom).max(0.0);
    let rtl = style.direction.is_rtl();
    let vertical_offset = if kind == Kind::Multiline || kind == Kind::List {
        0.0
    } else {
        ((height - metrics.height) / 2.0).floor()
    };
    let baseline = top + vertical_offset + metrics.ascent;
    if kind != Kind::Multiline && kind != Kind::List {
        output.baselines = Baselines {
            first: Some(baseline),
            last: Some(baseline),
        };
    }
    if input.run_mode != RunMode::PerformLayout {
        return output;
    }
    let labels = tree[id].children.clone();
    let rows = control.rows.clone();
    let row_step = metrics.height + 2.0;
    for (line, (child, row)) in labels.into_iter().zip(rows).enumerate() {
        let reserve = match kind {
            Kind::Menu if native => 20.0,
            Kind::Field(InputKind::Number) if native => 15.0,
            _ => row.indent,
        };
        let line_width = (width - reserve).max(0.0);
        if line_width == 0.0 {
            continue;
        }
        let measured = tree.compute_child_layout(
            child,
            LayoutInput {
                run_mode: RunMode::PerformLayout,
                known_dimensions: Size {
                    width: Some(line_width),
                    height: None,
                },
                parent_size: Size {
                    width: Some(line_width),
                    height: None,
                },
                available_space: Size {
                    width: AvailableSpace::Definite(line_width),
                    height: AvailableSpace::MaxContent,
                },
                ..LayoutInput::HIDDEN
            },
        );
        let x = left
            + if rtl {
                reserve - row.indent
            } else {
                row.indent
            };
        let y = if kind == Kind::Multiline {
            top
        } else {
            baseline + line as f32 * row_step - measured.baselines.first.unwrap_or_default()
        };
        tree[child].layout = Layout {
            size: measured.size,
            location: taffy::Point { x, y },
            ..Layout::new()
        };
    }
    if kind == Kind::List {
        let overflow = output.scrollable_overflow_rect;
        let mut rect = overflow.rect();
        rect.bottom = rect
            .bottom
            .max(top - border.top + tree[id].children.len() as f32 * row_step);
        output.scrollable_overflow_rect =
            taffy::ScrollableOverflow::new(rect, overflow.is_reversed());
    }
    output
}

impl Control {
    pub(crate) fn selected_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row.selected.then_some(i))
    }

    pub(crate) fn row_height(&self) -> f32 {
        self.metrics.map_or(0.0, |metrics| metrics.height + 2.0)
    }
}
