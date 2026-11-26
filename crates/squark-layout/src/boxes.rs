// SPDX-License-Identifier: MIT
use std::rc::Rc;

use squark_assets::{ElementImage, ImageCatalog, IntrinsicSize, Resources, svg_intrinsic};
use squark_mirror::Document;
use squark_types::{NodeId, Size, fragment::FragmentKind};
use style::selector_parser::PseudoElement;
use style::values::{
    generics::counters::{Content as CssContent, ContentItem},
    specified::box_::{DisplayInside, DisplayOutside},
};
use taffy::prelude::{TaffyAuto, TaffyZero};
use taffy::{Dimension, Display, Style};

use crate::{
    BoxTree, Content, Css, Id, Replacement,
    inline::{Inline, Token},
};

pub(crate) fn build(
    tree: &mut BoxTree<'_>,
    document: &Document,
    cascade: &squark_style::Cascade<'_>,
    resources: Resources<'_>,
    images: &mut ImageCatalog,
    viewport: Size,
) -> Id {
    let style = Style {
        display: Display::FlowRoot,
        size: taffy::Size {
            width: Dimension::length(viewport.width),
            height: Dimension::length(viewport.height),
        },
        ..Default::default()
    };
    let root = tree.add(
        document.root(),
        FragmentKind::Viewport,
        None,
        style,
        Content::Container,
    );
    let mut builder = Builder {
        tree,
        document,
        cascade,
        resources,
        images,
    };
    for &child in document.children(document.root()).unwrap_or_default() {
        if let Some(css) = document.computed_style(child)
            && !css.get_display().is_none()
        {
            let child = builder.make_box(child, css);
            builder.tree[root].children.push(child);
        }
    }
    root
}

struct Builder<'a, 'b> {
    tree: &'a mut BoxTree<'b>,
    document: &'a Document,
    cascade: &'a squark_style::Cascade<'a>,
    resources: Resources<'a>,
    images: &'a mut ImageCatalog,
}

impl Builder<'_, '_> {
    fn make_box(&mut self, owner: NodeId, css: Css) -> Id {
        let mut style = Rc::get_mut(&mut self.tree.calcs)
            .expect("styles are projected before layout")
            .style(&css);
        if let Some(plan) =
            crate::controls::build(self.document, self.cascade, self.tree.fonts, owner, &css)
        {
            style.item_is_replaced = true;
            if style.min_size.width.is_auto()
                && matches!(
                    style.size.width.expand(),
                    taffy::style::ExpandedDimension::Percent(_)
                )
            {
                // A percentage-sized replaced control can shrink to the
                // resulting track. Its intrinsic width cannot force that
                // track wider while the percentage is being resolved.
                style.min_size.width = taffy::LengthPercentageAuto::length(0.0);
            }
            return plan.attach(self.tree, owner, css, style);
        }
        let replacement = self.replacement(owner);
        if let Some((size, replacement)) = replacement {
            style.item_is_replaced = true;

            if matches!(replacement, Replacement::Svg) {
                // SVG width and height attributes are presentation hints and
                // can supply CSS lengths or percentages. A viewBox-only SVG
                // with automatic dimensions fills a definite containing width.
                style.size = style.size.zip_map(
                    taffy::Size {
                        width: self.svg_dimension(owner, "width", size.width),
                        height: self.svg_dimension(owner, "height", size.height),
                    },
                    |css, attribute| {
                        if css == Dimension::AUTO {
                            attribute
                        } else {
                            css
                        }
                    },
                );
                if style.size == taffy::Size::AUTO && size.ratio.is_some() {
                    style.size.width = Dimension::percent(1.0);
                }
            } else if !matches!(replacement, Replacement::Canvas(_)) {
                let dimension = |name| {
                    self.attribute(owner, name)
                        .and_then(|value| value.trim().parse::<u32>().ok())
                        .map_or(Dimension::AUTO, |value| Dimension::length(value as f32))
                };
                if style.size.width == Dimension::AUTO {
                    style.size.width = dimension("width");
                }
                if style.size.height == Dimension::AUTO {
                    style.size.height = dimension("height");
                }
            }
            return self.tree.add(
                owner,
                FragmentKind::Replaced,
                Some(css),
                style,
                Content::Replaced(size, replacement),
            );
        }
        if style.size.width == Dimension::AUTO
            && self.document.html_element(owner) == Some("legend")
            && self.document.parent(owner).is_some_and(|parent| {
                self.document.html_element(parent) == Some("fieldset")
                    && self
                        .document
                        .children(parent)
                        .unwrap_or_default()
                        .iter()
                        .copied()
                        .find(|&child| {
                            self.document.html_element(child) == Some("legend")
                                && self.document.computed_style(child).is_some_and(|style| {
                                    !style.get_display().is_none()
                                        && !style.get_box().position.is_absolutely_positioned()
                                })
                        })
                        == Some(owner)
            })
        {
            style.size.width = Dimension::fit_content();
        }
        let inner = style.display;
        let id = self.tree.add(
            owner,
            FragmentKind::Box,
            Some(css.clone()),
            style,
            Content::Container,
        );
        if css.get_display().inside() == DisplayInside::Table {
            self.make_table(id, owner);
        } else {
            let mut tokens = Vec::new();
            let flex_or_grid = matches!(inner, Display::Flex | Display::Grid);
            if css.get_display().is_list_item()
                && let Some(marker) = self.marker(owner, &css)
            {
                tokens.push(Token::Box(marker));
            }
            self.append_pseudo(id, owner, PseudoElement::Before, &mut tokens, flex_or_grid);
            for &child in self.document.children(owner).unwrap_or_default() {
                self.collect(child, owner, &css, flex_or_grid, &mut tokens, id);
            }
            self.append_pseudo(id, owner, PseudoElement::After, &mut tokens, flex_or_grid);
            self.flush_inline(id, owner, &css, &mut tokens);
            if flex_or_grid {
                let mut children = std::mem::take(&mut self.tree[id].children);
                children.sort_by_key(|&child| {
                    self.tree[child]
                        .css
                        .as_ref()
                        .filter(|_| !matches!(self.tree[child].content, Content::Inline(_)))
                        .map_or(0, |style| *style.get_order())
                });
                self.tree[id].children = children;
            }
        }
        id
    }

    fn collect(
        &mut self,
        owner: NodeId,
        style_owner: NodeId,
        css: &Css,
        flex_grid: bool,
        tokens: &mut Vec<Token>,
        parent: Id,
    ) {
        if self.document.is_text(owner) {
            if let Some(units) = self.document.text(owner) {
                tokens.push(Token::Text {
                    owner,
                    style_owner,
                    css: css.clone(),
                    units: units.into(),
                });
            }
            return;
        }
        let Some(style) = self.document.computed_style(owner) else {
            return;
        };
        let display = *style.get_display();
        if display.is_none() {
            return;
        }
        if display.inside() == DisplayInside::Contents {
            for &child in self.document.children(owner).unwrap_or_default() {
                self.collect(child, owner, &style, flex_grid, tokens, parent);
            }
        } else if self.document.html_element(owner) == Some("br") && !flex_grid {
            tokens.push(Token::Break { owner, css: style });
        } else if !flex_grid
            && matches!(
                style.get_box().position,
                style::computed_values::position::T::Absolute
                    | style::computed_values::position::T::Fixed
            )
            && style.get_box().original_display.outside() == DisplayOutside::Inline
        {
            tokens.push(Token::Positioned(self.make_box(owner, style)));
        } else if flex_grid || display.outside() != DisplayOutside::Inline {
            self.flush_parent_inline(parent, tokens);
            let child = self.make_box(owner, style);
            if self.tree[parent].style.display == taffy::Display::Grid
                && matches!(self.tree[child].content, Content::Control(_))
            {
                // Form controls are atomic, but normal grid alignment stretches
                // them like other boxes. Real replaced content keeps its size.
                self.tree[child].style.item_is_replaced = false;
            }
            self.tree[parent].children.push(child);
        } else if display.inside() != DisplayInside::Flow
            || self.is_replaced(owner)
            || matches!(
                self.document.html_element(owner),
                Some("input" | "select" | "textarea")
            )
        {
            let child = self.make_box(owner, style);
            tokens.push(Token::Box(child));
        } else {
            let controls = bidi_controls(&style);
            if let Some((enter, _)) = controls {
                tokens.push(Token::Bidi(enter));
            }
            let edge_style = crate::paint::has_inline_box(&style).then(|| {
                Rc::get_mut(&mut self.tree.calcs)
                    .expect("styles are projected before layout")
                    .style(&style)
            });
            if let Some(edge_style) = &edge_style {
                self.inline_edge(owner, &style, edge_style, true, tokens);
            }
            self.append_pseudo(parent, owner, PseudoElement::Before, tokens, flex_grid);
            for &child in self.document.children(owner).unwrap_or_default() {
                self.collect(child, owner, &style, flex_grid, tokens, parent);
            }
            self.append_pseudo(parent, owner, PseudoElement::After, tokens, flex_grid);
            if let Some(edge_style) = &edge_style {
                self.inline_edge(owner, &style, edge_style, false, tokens);
            }
            if let Some((_, exit)) = controls {
                tokens.push(Token::Bidi(exit));
            }
        }
    }

    fn inline_edge(
        &mut self,
        owner: NodeId,
        css: &Css,
        style: &Style<style::Atom>,
        start: bool,
        tokens: &mut Vec<Token>,
    ) {
        let left = start != style.direction.is_rtl();
        let (padding, border, margin) = if left {
            (style.padding.left, style.border.left, style.margin.left)
        } else {
            (style.padding.right, style.border.right, style.margin.right)
        };
        if padding == taffy::LengthPercentage::ZERO
            && border == taffy::LengthPercentage::ZERO
            && (margin == taffy::LengthPercentageAuto::ZERO
                || margin == taffy::LengthPercentageAuto::AUTO)
        {
            return;
        }
        let edge = self.tree.add(
            owner,
            FragmentKind::Box,
            Some(css.clone()),
            style.clone(),
            Content::InlineEdge(left),
        );
        self.tree[edge].claims = taffy::ContainingBlockClaims::NONE;
        tokens.push(Token::Edge { id: edge, start });
    }

    fn append_pseudo(
        &mut self,
        parent: Id,
        owner: NodeId,
        pseudo: PseudoElement,
        tokens: &mut Vec<Token>,
        blockified: bool,
    ) {
        let Some(css) = self.document.pseudo_style(owner, pseudo) else {
            return;
        };
        let CssContent::Items(content) = css.get_content() else {
            return;
        };
        if css.get_display().is_none() {
            return;
        }
        let text: String = content.items[..content.alt_start]
            .iter()
            .filter_map(|item| match item {
                ContentItem::String(text) => Some(&**text),
                ContentItem::OpenQuote => Some("“"),
                ContentItem::CloseQuote => Some("”"),
                _ => None,
            })
            .collect();
        let kind = match pseudo {
            PseudoElement::Before => FragmentKind::Before,
            PseudoElement::After => FragmentKind::After,
            _ => unreachable!("generated content slot"),
        };
        let child = self.generated_box(owner, kind, &css, &text);
        if blockified || css.get_display().outside() != DisplayOutside::Inline {
            self.flush_parent_inline(parent, tokens);
            self.tree[parent].children.push(child);
        } else {
            tokens.push(Token::Box(child));
        }
    }

    fn generated_box(&mut self, owner: NodeId, kind: FragmentKind, css: &Css, text: &str) -> Id {
        let style = Rc::get_mut(&mut self.tree.calcs)
            .expect("projection precedes layout")
            .style(css);
        let id = self
            .tree
            .add(owner, kind, Some(css.clone()), style, Content::Container);
        if !text.is_empty() {
            let mut tokens = vec![Token::Text {
                owner,
                style_owner: owner,
                css: css.clone(),
                units: text.encode_utf16().collect(),
            }];
            self.flush_inline(id, owner, css, &mut tokens);
        }
        id
    }

    fn marker(&mut self, owner: NodeId, css: &Css) -> Option<Id> {
        use squark_types::fragment::MirrorRead;
        use style::counter_style::CounterStyle;
        let label = match &css.get_list_style_type().0 {
            CounterStyle::None => return None,
            CounterStyle::String(label) => label.as_str().to_owned(),
            CounterStyle::Name(name) => match name.0.as_str() {
                "disc" => "•".into(),
                "circle" => "◦".into(),
                "square" => "▪".into(),
                _ => {
                    let mirror: &dyn MirrorRead = self.document;
                    let parent = mirror.node(owner)?.links.parent?;
                    let start = self
                        .attribute(parent, "start")
                        .and_then(|n| n.parse::<i32>().ok())
                        .unwrap_or(1);
                    let mut number = start;
                    for &sibling in self.document.children(parent)? {
                        if let Some(value) = self
                            .attribute(sibling, "value")
                            .and_then(|n| n.parse::<i32>().ok())
                        {
                            number = value;
                        }
                        if sibling == owner {
                            break;
                        }
                        if self.document.html_element(sibling) == Some("li") {
                            number = number.saturating_add(1);
                        }
                    }
                    format!("{number}.")
                }
            },
            CounterStyle::Symbols { .. } => return None,
        };
        let marker = self.generated_box(owner, FragmentKind::Marker, css, &label);
        // The marker's size comes from its own glyphs, independently of the list item's box.
        self.tree[marker].style = Style {
            display: Display::Block,
            ..Default::default()
        };
        self.tree[marker].claims = taffy::ContainingBlockClaims::NONE;
        self.tree[marker].marker_outside = *css.get_list_style_position()
            == style::computed_values::list_style_position::T::Outside;
        self.tree[marker].style.margin.right =
            taffy::LengthPercentageAuto::length(css.get_font().font_size.used_size.0.px() * 0.35);
        Some(marker)
    }

    fn flush_parent_inline(&mut self, parent: Id, tokens: &mut Vec<Token>) {
        let owner = self.tree[parent].owner;
        let css = self.tree[parent]
            .css
            .as_ref()
            .expect("a styled container")
            .clone();
        self.flush_inline(parent, owner, &css, tokens);
    }

    fn flush_inline(&mut self, parent: Id, owner: NodeId, css: &Css, tokens: &mut Vec<Token>) {
        if tokens.is_empty() {
            return;
        }
        let inline = Inline::new(std::mem::take(tokens));
        if inline.is_empty() {
            return;
        }
        let children = inline.boxes().collect();
        let style = Style {
            display: Display::Block,
            direction: self.tree[parent].style.direction,
            ..Default::default()
        };
        let id = self.tree.add(
            owner,
            FragmentKind::Line,
            Some(css.clone()),
            style,
            Content::Inline(Box::new(inline)),
        );
        self.tree[id].claims = taffy::ContainingBlockClaims::NONE;
        self.tree[id].children = children;
        self.tree[parent].children.push(id);
    }

    fn make_table(&mut self, table: Id, owner: NodeId) {
        use style::computed_values::{
            border_collapse::T as BorderCollapse, table_layout::T as TableLayout,
        };

        let css = self.tree[table].css.as_ref().expect("table style");
        let spacing = if *css.get_border_collapse() == BorderCollapse::Separate {
            let spacing = &css.get_border_spacing().0;
            taffy::Size {
                width: spacing.width.0.px(),
                height: spacing.height.0.px(),
            }
        } else {
            taffy::Size::ZERO
        };
        let mut can_size_columns = *css.get_table_layout() == TableLayout::Auto;
        let style = &mut self.tree[table].style;
        style.gap = spacing.map(taffy::LengthPercentage::length);
        let mut columns = 0;

        let mut pending: Vec<_> = self
            .document
            .children(owner)
            .unwrap_or_default()
            .iter()
            .rev()
            .map(|&node| (node, None::<usize>))
            .collect();
        let mut row = 0i16;
        let mut parts: Vec<crate::table::Part> = Vec::new();
        while let Some((node, group)) = pending.pop() {
            let Some(css) = self.document.computed_style(node) else {
                continue;
            };
            if css.get_display().is_none() {
                continue;
            }
            match css.get_display().inside() {
                DisplayInside::TableRow => {
                    let start = row as usize;
                    row = row.saturating_add(1);
                    let end = row as usize;
                    if let Some(group) = group {
                        parts[group].rows.end = end;
                    }
                    parts.push(crate::table::Part {
                        owner: node,
                        rows: start..end,
                    });
                    let mut col = 1i16;
                    for &cell in self.document.children(node).unwrap_or_default() {
                        let Some(css) = self.document.computed_style(cell) else {
                            continue;
                        };
                        if css.get_display().inside() != DisplayInside::TableCell {
                            continue;
                        }
                        let span = self
                            .attribute(cell, "colspan")
                            .and_then(|s| s.parse::<u16>().ok())
                            .unwrap_or(1)
                            .clamp(1, 1000);
                        can_size_columns &= span == 1;
                        let child = self.make_box(cell, css);
                        let style = &mut self.tree[child].style;
                        style.grid_row.start = taffy::style_helpers::line(row);
                        style.grid_column.start = taffy::style_helpers::line(col);
                        style.grid_column.end = taffy::style_helpers::span(span);
                        col = col.saturating_add(span as i16);
                        self.tree[table].children.push(child);
                    }
                    columns = columns.max(usize::try_from(col - 1).unwrap_or_default());
                }
                DisplayInside::TableRowGroup
                | DisplayInside::TableHeaderGroup
                | DisplayInside::TableFooterGroup => {
                    let group = parts.len();
                    parts.push(crate::table::Part {
                        owner: node,
                        rows: row as usize..row as usize,
                    });
                    pending.extend(
                        self.document
                            .children(node)
                            .unwrap_or_default()
                            .iter()
                            .rev()
                            .map(|&node| (node, Some(group))),
                    );
                }
                _ => {}
            }
        }
        // Even a row without cells consumes a track (and its border spacing).
        self.tree[table].style.grid_template_rows =
            vec![taffy::style_helpers::auto(); row as usize];
        self.tree[table].content = Content::Table(crate::table::Table {
            spacing,
            auto_columns: (can_size_columns && columns > 0).then_some(columns),
            parts,
        });
    }

    fn svg_dimension(&self, id: NodeId, name: &str, pixels: Option<f32>) -> Dimension {
        if let Some(pixels) = pixels {
            return Dimension::length(pixels);
        }
        self.attribute(id, name)
            .and_then(|value| value.trim().strip_suffix('%')?.trim().parse::<f32>().ok())
            .filter(|percent| percent.is_finite() && *percent >= 0.0)
            .map_or(Dimension::AUTO, |percent| {
                Dimension::percent(percent / 100.0)
            })
    }

    fn attribute(&self, owner: NodeId, name: &str) -> Option<String> {
        self.document
            .attribute(owner, name)
            .map(String::from_utf16_lossy)
    }

    fn is_replaced(&self, owner: NodeId) -> bool {
        matches!(self.document.html_element(owner), Some("img" | "canvas"))
            || self.document.is_svg_root(owner)
    }

    fn replacement(&mut self, owner: NodeId) -> Option<(IntrinsicSize, Replacement)> {
        if self.document.html_element(owner) == Some("img") {
            let mut broken = self.document.attribute(owner, "src").is_some();
            if let Some(url) = self
                .document
                .image_src(owner)
                .filter(|src| !src.trim().is_empty())
                .and_then(|src| self.resources.resolve(src))
            {
                broken = self.images.element_state(&url) != Some(ElementImage::Pending);
                if let Some(image) = self.images.find(self.resources, &url) {
                    return Some((image.intrinsic, Replacement::Image(image)));
                }
            }
            let alt = self.attribute(owner, "alt").unwrap_or_default();
            let extent = if alt.is_empty() { 0.0 } else { 16.0 };
            let replacement = if broken {
                Replacement::BrokenImage
            } else {
                Replacement::EmptyImage
            };
            Some((IntrinsicSize::dimensions(extent, extent), replacement))
        } else if let Some(size) = self.document.canvas_bitmap_size(owner) {
            Some((
                IntrinsicSize::dimensions(size[0] as f32, size[1] as f32),
                Replacement::Canvas(size),
            ))
        } else if self.document.is_svg_root(owner) {
            Some((
                svg_intrinsic(
                    self.attribute(owner, "width").as_deref(),
                    self.attribute(owner, "height").as_deref(),
                    self.attribute(owner, "viewBox").as_deref(),
                ),
                Replacement::Svg,
            ))
        } else {
            None
        }
    }
}

fn bidi_controls(css: &Css) -> Option<(&'static str, &'static str)> {
    use style::computed_values::{direction::T as Direction, unicode_bidi::T as Bidi};
    let rtl = *css.get_direction() == Direction::Rtl;
    Some(match (*css.get_unicode_bidi(), rtl) {
        (Bidi::Normal, _) => return None,
        (Bidi::Embed, false) => ("\u{202a}", "\u{202c}"),
        (Bidi::Embed, true) => ("\u{202b}", "\u{202c}"),
        (Bidi::BidiOverride, false) => ("\u{202d}", "\u{202c}"),
        (Bidi::BidiOverride, true) => ("\u{202e}", "\u{202c}"),
        (Bidi::Isolate, false) => ("\u{2066}", "\u{2069}"),
        (Bidi::Isolate, true) => ("\u{2067}", "\u{2069}"),
        (Bidi::IsolateOverride, false) => ("\u{2066}\u{202d}", "\u{202c}\u{2069}"),
        (Bidi::IsolateOverride, true) => ("\u{2067}\u{202e}", "\u{202c}\u{2069}"),
        (Bidi::Plaintext, _) => ("\u{2068}", "\u{2069}"),
    })
}
