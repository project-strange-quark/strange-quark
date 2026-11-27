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
            let children = self.table_children(owner);
            self.make_table(id, owner, children);
        } else {
            let mut tokens = Vec::new();
            let flex_or_grid = matches!(inner, Display::Flex | Display::Grid);
            if css.get_display().is_list_item()
                && let Some(marker) = self.marker(owner, &css)
            {
                tokens.push(Token::Box(marker));
            }
            self.append_pseudo(id, owner, PseudoElement::Before, &mut tokens, flex_or_grid);
            self.collect_children(owner, &css, flex_or_grid, &mut tokens, id, false);
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

    /// Collects a node's children into `parent`. Table rows, row groups and
    /// cells outside a table, with the whitespace between them, get an
    /// anonymous table: an inline one when `inline` says the children are in
    /// an inline box (CSS 2.1 § 17.2.1).
    fn collect_children(
        &mut self,
        owner: NodeId,
        css: &Css,
        flex_grid: bool,
        tokens: &mut Vec<Token>,
        parent: Id,
        inline: bool,
    ) {
        // Boxes follow the flat tree: a shadow host renders its shadow tree,
        // and a slot its assigned nodes or else its own children.
        let children = self.document.flat_children(owner).to_vec();
        let mut index = 0;
        while index < children.len() {
            if flex_grid || !self.is_table_part(children[index]) {
                self.collect(
                    children[index],
                    owner,
                    css,
                    flex_grid,
                    tokens,
                    parent,
                    inline,
                );
                index += 1;
                continue;
            }
            let mut end = index + 1;
            let mut scan = end;
            while scan < children.len() {
                if self.is_table_part(children[scan]) {
                    scan += 1;
                    end = scan;
                } else if self.is_space(children[scan]) {
                    scan += 1;
                } else {
                    break;
                }
            }
            let run = children[index..end]
                .iter()
                .copied()
                .filter(|&child| !self.is_space(child))
                .collect();
            self.anonymous_table(owner, css, run, tokens, parent, inline);
            index = end;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn collect(
        &mut self,
        owner: NodeId,
        style_owner: NodeId,
        css: &Css,
        flex_grid: bool,
        tokens: &mut Vec<Token>,
        parent: Id,
        inline: bool,
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
            self.collect_children(owner, &style, flex_grid, tokens, parent, inline);
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
            self.collect_children(owner, &style, flex_grid, tokens, parent, true);
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

    /// The anonymous table around a run of table parts with no table.
    fn anonymous_table(
        &mut self,
        owner: NodeId,
        css: &Css,
        run: Vec<NodeId>,
        tokens: &mut Vec<Token>,
        parent: Id,
        inline: bool,
    ) {
        use style::values::specified::box_::Display as CssDisplay;

        let display = if inline {
            CssDisplay::InlineTable
        } else {
            CssDisplay::Table
        };
        let table_css = self.cascade.anonymous_style(css, display);
        let style = Rc::get_mut(&mut self.tree.calcs)
            .expect("styles are projected before layout")
            .style(&table_css);
        let table = self.tree.add(
            owner,
            FragmentKind::Box,
            Some(table_css),
            style,
            Content::Container,
        );
        self.tree[table].anonymous = true;
        self.make_table(table, owner, run);
        if inline {
            tokens.push(Token::Box(table));
        } else {
            self.flush_parent_inline(parent, tokens);
            self.tree[parent].children.push(table);
        }
    }

    /// A table part's children, with `display: contents` elements replaced by
    /// their own children.
    fn table_children(&self, owner: NodeId) -> Vec<NodeId> {
        let mut children = Vec::new();
        for &child in self.document.children(owner).unwrap_or_default() {
            match self.document.computed_style(child) {
                Some(css) if css.get_display().is_none() => {}
                Some(css) if css.get_display().inside() == DisplayInside::Contents => {
                    children.extend(self.table_children(child));
                }
                Some(_) => children.push(child),
                None if self.document.is_text(child) => children.push(child),
                None => {}
            }
        }
        children
    }

    fn table_display(&self, node: NodeId) -> Option<DisplayInside> {
        self.document
            .computed_style(node)
            .map(|css| *css.get_display())
            .filter(|display| !display.is_none())
            .map(|display| display.inside())
    }

    fn is_table_part(&self, node: NodeId) -> bool {
        matches!(
            self.table_display(node),
            Some(
                DisplayInside::TableRowGroup
                    | DisplayInside::TableHeaderGroup
                    | DisplayInside::TableFooterGroup
                    | DisplayInside::TableRow
                    | DisplayInside::TableCell
            )
        )
    }

    fn is_caption(&self, node: NodeId) -> bool {
        self.document.computed_style(node).is_some_and(|css| {
            !css.get_display().is_none()
                && css.get_display().outside() == DisplayOutside::TableCaption
        })
    }

    /// Whether a node is a text node of collapsible whitespace only.
    fn is_space(&self, node: NodeId) -> bool {
        self.document.is_text(node)
            && self.document.text(node).is_some_and(|units| {
                units
                    .iter()
                    .all(|&unit| matches!(unit, 0x20 | 0x09 | 0x0a | 0x0c | 0x0d))
            })
    }

    /// Builds a table's rows and cells from its children. Content that is
    /// not a table part goes into anonymous cells, and cells outside a row
    /// into anonymous rows.
    fn make_table(&mut self, table: Id, owner: NodeId, children: Vec<NodeId>) {
        use style::computed_values::{
            border_collapse::T as BorderCollapse, table_layout::T as TableLayout,
        };

        let css = self.tree[table].css.clone().expect("table style");
        let collapse = *css.get_border_collapse() == BorderCollapse::Collapse;
        let spacing = if collapse {
            taffy::Size::ZERO
        } else {
            let spacing = &css.get_border_spacing().0;
            taffy::Size {
                width: spacing.width.0.px(),
                height: spacing.height.0.px(),
            }
        };
        let fixed = *css.get_table_layout() == TableLayout::Fixed;
        if collapse {
            // A table whose borders collapse has no padding.
            self.tree[table].style.padding = taffy::Rect::<taffy::LengthPercentage>::ZERO;
        }

        // The first header group comes first and the first footer group last.
        let header = children
            .iter()
            .copied()
            .find(|&node| self.table_display(node) == Some(DisplayInside::TableHeaderGroup));
        let footer = children
            .iter()
            .copied()
            .find(|&node| self.table_display(node) == Some(DisplayInside::TableFooterGroup));
        let ordered: Vec<NodeId> = header
            .into_iter()
            .chain(
                children
                    .iter()
                    .copied()
                    .filter(|&node| Some(node) != header && Some(node) != footer),
            )
            .chain(footer)
            .collect();
        let mut grid = TableGrid::default();
        for node in ordered {
            match self.table_display(node) {
                Some(
                    DisplayInside::TableRowGroup
                    | DisplayInside::TableHeaderGroup
                    | DisplayInside::TableFooterGroup,
                ) => {
                    self.end_anonymous_cell(table, &mut grid);
                    grid.end_group();
                    let group = grid.parts.len();
                    grid.parts.push(crate::table::Part {
                        owner: node,
                        rows: grid.rows.len()..grid.rows.len(),
                    });
                    let group_css = self.document.computed_style(node).expect("styled group");
                    for child in self.table_children(node) {
                        self.table_item(table, &mut grid, child, node, &group_css, Some(group));
                    }
                    self.end_anonymous_cell(table, &mut grid);
                    grid.end_group();
                }
                _ => self.table_item(table, &mut grid, node, owner, &css, None),
            }
        }
        self.end_anonymous_cell(table, &mut grid);
        grid.end_group();
        self.tree[table].content = Content::Table(crate::table::Table {
            spacing,
            fixed,
            rows: grid.rows,
            cells: grid.cells,
            columns: grid.columns,
            parts: grid.parts,
            tracks: Default::default(),
        });
    }

    /// Places a child of a table or row group: a row as it is, a cell in an
    /// anonymous row, and other content in an anonymous cell in that row.
    fn table_item(
        &mut self,
        table: Id,
        grid: &mut TableGrid,
        node: NodeId,
        owner: NodeId,
        css: &Css,
        group: Option<usize>,
    ) {
        match self.table_display(node) {
            Some(DisplayInside::TableRow) => {
                self.end_anonymous_cell(table, grid);
                self.table_row(table, grid, node, group);
            }
            Some(DisplayInside::TableColumn | DisplayInside::TableColumnGroup) => {}
            // Captions are not laid out yet.
            _ if self.is_caption(node) => {}
            _ if self.is_space(node) && grid.loose.is_empty() => {}
            display => {
                if !grid.anonymous_row {
                    self.end_anonymous_cell(table, grid);
                    grid.start_row(None, Dimension::AUTO, group);
                    grid.anonymous_row = true;
                }
                if display == Some(DisplayInside::TableCell) {
                    self.end_anonymous_cell(table, grid);
                    self.table_cell(table, grid, node);
                } else {
                    if grid.loose.is_empty() {
                        grid.loose_style = Some((owner, css.clone()));
                    }
                    grid.loose.push(node);
                }
            }
        }
    }

    fn table_row(&mut self, table: Id, grid: &mut TableGrid, row: NodeId, group: Option<usize>) {
        let Some(css) = self.document.computed_style(row) else {
            return;
        };
        let height = Rc::get_mut(&mut self.tree.calcs)
            .expect("styles are projected before layout")
            .style(&css)
            .size
            .height;
        grid.start_row(Some(row), height, group);
        for child in self.table_children(row) {
            if self.table_display(child) == Some(DisplayInside::TableCell) {
                self.end_anonymous_cell(table, grid);
                self.table_cell(table, grid, child);
            } else if !(self.is_space(child) && grid.loose.is_empty()) {
                if grid.loose.is_empty() {
                    grid.loose_style = Some((row, css.clone()));
                }
                grid.loose.push(child);
            }
        }
        self.end_anonymous_cell(table, grid);
    }

    /// Wraps the content collected outside cells in an anonymous cell.
    fn end_anonymous_cell(&mut self, table: Id, grid: &mut TableGrid) {
        if grid.loose.is_empty() {
            return;
        }
        let nodes = std::mem::take(&mut grid.loose);
        let (owner, parent_css) = grid.loose_style.take().expect("loose content style");
        let css = self.cascade.anonymous_style(
            &parent_css,
            style::values::specified::box_::Display::TableCell,
        );
        let style = Rc::get_mut(&mut self.tree.calcs)
            .expect("styles are projected before layout")
            .style(&css);
        let cell = self.tree.add(
            owner,
            FragmentKind::Box,
            Some(css.clone()),
            style,
            Content::Container,
        );
        self.tree[cell].anonymous = true;
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < nodes.len() {
            // Table parts among the content get an anonymous table of their
            // own inside the cell.
            if self.is_table_part(nodes[index]) {
                let end = nodes[index..]
                    .iter()
                    .position(|&node| !self.is_table_part(node))
                    .map_or(nodes.len(), |offset| index + offset);
                let run = nodes[index..end].to_vec();
                self.anonymous_table(owner, &css, run, &mut tokens, cell, false);
                index = end;
            } else {
                self.collect(nodes[index], owner, &css, false, &mut tokens, cell, false);
                index += 1;
            }
        }
        self.flush_inline(cell, owner, &css, &mut tokens);
        self.place_cell(table, grid, cell, 1, 1, crate::table::Align::Baseline);
    }

    /// Places a cell in the first free column of the current row.
    fn table_cell(&mut self, table: Id, grid: &mut TableGrid, cell: NodeId) {
        let Some(css) = self.document.computed_style(cell) else {
            return;
        };
        let span = |name, max: u16| {
            self.attribute(cell, name)
                .and_then(|value| value.trim().parse::<u16>().ok())
                .map(|span| span.min(max))
        };
        let columns = usize::from(span("colspan", 1000).unwrap_or(1).max(1));
        // `rowspan="0"` spans the rest of the row group.
        let rows = match span("rowspan", 65534).unwrap_or(1) {
            0 => usize::MAX,
            rows => usize::from(rows),
        };
        let align = cell_align(&css);
        let child = self.make_box(cell, css);
        self.place_cell(table, grid, child, columns, rows, align);
    }

    /// Puts a cell box in the grid. The table resolves the cell's sizes, so
    /// its box keeps automatic ones.
    fn place_cell(
        &mut self,
        table: Id,
        grid: &mut TableGrid,
        child: Id,
        columns: usize,
        rows: usize,
        align: crate::table::Align,
    ) {
        let row = grid.rows.len() - 1;
        let mut column = grid.column;
        while grid.occupied.get(column).is_some_and(|&until| until > row) {
            column += 1;
        }
        let end = column + columns;
        if grid.occupied.len() < end {
            grid.occupied.resize(end, 0);
        }
        for until in &mut grid.occupied[column..end] {
            *until = (*until).max(row.saturating_add(rows));
        }
        grid.column = end;
        grid.columns = grid.columns.max(end);
        let style = &mut self.tree[child].style;
        grid.cells.push(crate::table::Cell {
            row,
            column,
            rows,
            columns,
            align,
            size: style.size,
            min_size: style.min_size,
            max_width: style.max_size.width,
        });
        style.size = taffy::Size::AUTO;
        style.min_size = taffy::Size::AUTO;
        style.max_size = taffy::Size::AUTO;
        style.aspect_ratio = None;
        // Table cells have no margins.
        style.margin = taffy::Rect::<taffy::LengthPercentageAuto>::ZERO;
        self.tree[table].children.push(child);
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

/// The table grid as its rows and cells are found.
#[derive(Default)]
struct TableGrid {
    rows: Vec<crate::table::Row>,
    cells: Vec<crate::table::Cell>,
    parts: Vec<crate::table::Part>,
    columns: usize,
    /// The next column to try in the current row.
    column: usize,
    /// For each column, the first row that a cell from an earlier row does
    /// not span into.
    occupied: Vec<usize>,
    /// The first cell of the current row group.
    group_cells: usize,
    /// Whether the current row is an anonymous one holding cells that have
    /// no row of their own.
    anonymous_row: bool,
    /// Content of the current row that is not in a cell, waiting for an
    /// anonymous cell, and the element and style that cell inherits from.
    loose: Vec<NodeId>,
    loose_style: Option<(NodeId, Css)>,
}

impl TableGrid {
    fn start_row(&mut self, owner: Option<NodeId>, height: Dimension, group: Option<usize>) {
        let index = self.rows.len();
        self.rows.push(crate::table::Row { height });
        if let Some(owner) = owner {
            self.parts.push(crate::table::Part {
                owner,
                rows: index..index + 1,
            });
        }
        if let Some(group) = group {
            self.parts[group].rows.end = index + 1;
        }
        self.column = 0;
        self.anonymous_row = false;
    }

    /// Cells span rows only within their row group.
    fn end_group(&mut self) {
        let end = self.rows.len();
        for cell in &mut self.cells[self.group_cells..] {
            cell.rows = cell.rows.min(end - cell.row);
        }
        self.group_cells = self.cells.len();
        self.occupied.clear();
        self.anonymous_row = false;
    }
}

/// A table cell's `vertical-align`, which Stylo stores as the
/// `alignment-baseline` and `baseline-shift` longhands.
fn cell_align(css: &Css) -> crate::table::Align {
    use crate::table::Align;
    use style::values::{
        computed::{AlignmentBaseline, BaselineShift},
        generics::box_::BaselineShiftKeyword,
    };

    let box_style = css.get_box();
    match box_style.get_baseline_shift() {
        BaselineShift::Keyword(BaselineShiftKeyword::Top) => Align::Top,
        BaselineShift::Keyword(BaselineShiftKeyword::Bottom) => Align::Bottom,
        BaselineShift::Keyword(BaselineShiftKeyword::Center) => Align::Middle,
        _ if box_style.slow_clone_alignment_baseline() == AlignmentBaseline::Middle => {
            Align::Middle
        }
        _ => Align::Baseline,
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
