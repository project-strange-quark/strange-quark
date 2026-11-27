// SPDX-License-Identifier: MIT
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use parley::PositionedLayoutItem;
use squark_assets::{ImageCatalog, Resources};
use squark_mirror::Document;
use squark_types::{
    NodeId, Point, Rect, Size,
    fragment::{
        DecorationKind, DecorationSpan, DecorationStyle, Edges, Fragment, FragmentContent,
        FragmentId, FragmentKind, FragmentTree, PaintColor, PaintValues, PointerEvents,
        PositionedGlyph, PositionedRun, ScrollFrame, StickyConstraints, TextRange,
    },
};
use style::values::specified::box_::{Contain, Overflow};

use crate::{
    BoxTree, Content, Id, decorations, editing,
    inline_paint::Slices,
    paint,
    text_flow::{Flow, Flows},
};

type IdKey = (NodeId, u8, u32);

#[derive(Debug, Default)]
pub(crate) struct FragmentIds {
    previous: HashMap<IdKey, FragmentId>,
    last: u64,
}

impl FragmentIds {
    fn next(&mut self, key: IdKey) -> FragmentId {
        self.previous.get(&key).copied().unwrap_or_else(|| {
            self.last = self
                .last
                .checked_add(1)
                .expect("fragment id space exhausted");
            FragmentId(self.last)
        })
    }
}

pub(crate) type Margins = BTreeMap<FragmentId, Edges<f32>>;

pub(crate) fn emit(
    tree: BoxTree<'_>,
    root: Id,
    document: &Document,
    resources: Resources<'_>,
    images: &mut ImageCatalog,
    ids: &mut FragmentIds,
    dpr: f64,
) -> Emitted {
    let mut emit = Emitter::new(tree, root, document, resources, images, ids, dpr);
    let root_style = emit.tree[root]
        .children
        .first()
        .copied()
        .and_then(|element| {
            let css = emit.tree[element].css.as_ref()?;
            if css.get_contain().intersects(
                Contain::PAINT
                    | Contain::LAYOUT
                    | Contain::STYLE
                    | Contain::SIZE
                    | Contain::INLINE_SIZE,
            ) {
                return None;
            }
            let style = paint::project(
                css,
                resources,
                emit.images,
                Size::default(),
                Size::default(),
                Size::default(),
            );
            if style.art.background_color.alpha != 0.0
                || css
                    .get_background()
                    .background_image
                    .0
                    .iter()
                    .any(|image| !matches!(image, style::values::computed::Image::None))
            {
                return Some((element, style));
            }
            if document.html_element(emit.tree[element].owner) != Some("html") {
                return None;
            }
            emit.tree[element].children.iter().find_map(|&child| {
                let owner = emit.tree[child].owner;
                if document.html_element(owner) != Some("body") {
                    return None;
                }
                let css = emit.tree[child].css.as_ref()?;
                if css.get_contain().intersects(
                    Contain::PAINT
                        | Contain::LAYOUT
                        | Contain::STYLE
                        | Contain::SIZE
                        | Contain::INLINE_SIZE,
                ) {
                    return None;
                }
                Some((
                    child,
                    paint::project(
                        css,
                        resources,
                        emit.images,
                        Size::default(),
                        Size::default(),
                        Size::default(),
                    ),
                ))
            })
        });
    let mut canvas = PaintValues::default();
    if let Some((owner, source)) = root_style {
        emit.propagation = Some(owner);
        canvas.art.background_color = source.art.background_color;
        canvas.box_paint.backgrounds = source.box_paint.backgrounds;
    }
    let root_id = emit.ids_by_box[&root];
    emit.emit_box(root, None, None, document.root(), Some(canvas));
    emit.ids.previous = emit.keys;
    Emitted {
        tree: FragmentTree {
            root: root_id,
            ime_area: None,
            nodes: emit.nodes,
        },
        margins: emit.margins,
        editing: emit.editing,
        table_parts: emit.table_parts,
        flows: emit.flows,
    }
}

pub(crate) struct Emitted {
    pub tree: FragmentTree,
    pub margins: Margins,
    pub editing: editing::Fields,
    pub table_parts: crate::table::Parts,
    pub flows: Flows,
}

struct Positions {
    origins: Vec<Point>,
    containing: Vec<Option<Id>>,
}

impl Positions {
    fn place(&mut self, tree: &BoxTree<'_>, id: Id, parent: Point, hoisted: bool) {
        let index = usize::from(id);
        if self.containing[index].is_some() && !hoisted {
            return;
        }
        let layout = tree[id].layout;
        let origin = Point {
            x: parent.x + layout.location.x,
            y: parent.y + layout.location.y,
        };
        self.origins[index] = origin;
        for &child in &tree[id].children {
            self.place(tree, child, origin, false);
        }
        for &child in &tree[id].hoisted {
            self.place(tree, child, origin, true);
        }
    }
}

pub(crate) struct Emitter<'a, 'b> {
    tree: BoxTree<'b>,
    positions: Positions,
    document: &'a Document,
    resources: Resources<'a>,
    images: &'a mut ImageCatalog,
    ids: &'a mut FragmentIds,
    keys: HashMap<IdKey, FragmentId>,
    counts: HashMap<(NodeId, u8), u32>,
    nodes: BTreeMap<FragmentId, Fragment>,
    margins: Margins,
    editing: editing::Fields,
    table_parts: crate::table::Parts,
    flows: Flows,
    ids_by_box: HashMap<Id, FragmentId>,
    propagation: Option<Id>,
    raster_scale: f64,
    empty_paint: Arc<PaintValues>,
}

impl<'a, 'b> Emitter<'a, 'b> {
    pub(crate) fn new(
        tree: BoxTree<'b>,
        root: Id,
        document: &'a Document,
        resources: Resources<'a>,
        images: &'a mut ImageCatalog,
        ids: &'a mut FragmentIds,
        dpr: f64,
    ) -> Self {
        let mut positions = Positions {
            origins: vec![Point::default(); tree.boxes.len()],
            containing: vec![None; tree.boxes.len()],
        };
        for (i, node) in tree.boxes.iter().enumerate() {
            for &child in &node.hoisted {
                positions.containing[usize::from(child)] = Some(Id::from(i));
            }
        }
        positions.place(&tree, root, Point::default(), false);
        let mut emit = Self {
            positions,
            document,
            tree,
            resources,
            images,
            ids,
            keys: HashMap::new(),
            counts: HashMap::new(),
            nodes: BTreeMap::new(),
            margins: BTreeMap::new(),
            editing: BTreeMap::new(),
            table_parts: BTreeMap::new(),
            flows: Flows::default(),
            ids_by_box: HashMap::new(),
            propagation: None,
            raster_scale: (dpr * 1024.0).round().max(1.0) / 1024.0,
            empty_paint: Arc::default(),
        };
        for i in 0..emit.tree.boxes.len() {
            let id = Id::from(i);
            if !matches!(
                emit.tree[id].content,
                Content::Inline(_) | Content::InlineEdge(_)
            ) {
                let fragment = emit.identify(emit.tree[id].owner, emit.tree[id].kind);
                emit.ids_by_box.insert(id, fragment);
            }
        }
        emit
    }

    pub(crate) fn emit_edited_field(
        mut self,
        root: Id,
        current: &FragmentTree,
        field: FragmentId,
    ) -> (FragmentTree, Option<editing::Field>) {
        let old = &current.nodes[&field];
        let id = self.emit_box(
            root,
            old.parent,
            None,
            old.owner,
            Some((*old.style).clone()),
        );
        debug_assert_eq!(id, field);
        let updated = self.nodes.get_mut(&id).unwrap();
        updated.style = old.style.clone();
        updated.containing_block = old.containing_block;
        updated.sticky = old.sticky;
        if let (Some(old_scroll), Some(new_scroll)) = (old.scroll, &mut updated.scroll) {
            new_scroll.offset = old_scroll.offset;
            new_scroll.generation = old_scroll.generation;
        }

        // All fragments below a textarea are generated from the control's value
        // and use its owner. Retire unused keys without scanning other boxes.
        let mut pending = old.children.to_vec();
        let mut old_counts = HashMap::new();
        while let Some(id) = pending.pop() {
            let fragment = &current.nodes[&id];
            pending.extend(fragment.children.iter().copied());
            *old_counts
                .entry((fragment.owner, fragment.kind as u8))
                .or_insert(0) += 1;
        }
        for ((owner, kind), end) in old_counts {
            let start = self.counts.get(&(owner, kind)).copied().unwrap_or_default();
            for n in start..end {
                self.ids.previous.remove(&(owner, kind, n));
            }
        }
        self.ids.previous.extend(self.keys);
        (
            FragmentTree {
                root: id,
                nodes: self.nodes,
                ime_area: None,
            },
            self.editing.remove(&old.owner),
        )
    }

    fn identify(&mut self, node: NodeId, kind: FragmentKind) -> FragmentId {
        let kind = kind as u8;
        let count = self.counts.entry((node, kind)).or_default();
        let key = (node, kind, *count);
        *count += 1;
        let id = self.ids.next(key);
        self.keys.insert(key, id);
        id
    }

    fn emit_box(
        &mut self,
        node: Id,
        parent: Option<FragmentId>,
        parent_content: Option<Rect>,
        scroll_owner: NodeId,
        override_style: Option<PaintValues>,
    ) -> FragmentId {
        let origin = self.positions.origins[usize::from(node)];
        let data = &self.tree[node];
        let id = self.ids_by_box[&node];
        let owner = data.owner;
        let kind = data.kind;
        let layout = data.layout;
        let baseline = data.baselines;
        let rect = Rect {
            origin,
            size: Size {
                width: layout.size.width,
                height: layout.size.height,
            },
        };
        if let (Content::Table(table), taffy::DetailedLayoutInfo::Grid(grid)) =
            (&data.content, &data.details)
        {
            crate::table::record_parts(&table.parts, grid, id, rect, &mut self.table_parts);
        }
        let border = edges(layout.border);
        let padding = edges(layout.padding);
        self.margins.insert(id, edges(layout.margin));
        let scrollport = inset(rect, border);
        let content_box = inset(scrollport, padding);
        let overflow_rect = layout.scrollable_overflow_rect.rect();
        let mut overflow = Rect::new(
            scrollport.origin.x + overflow_rect.left,
            scrollport.origin.y + overflow_rect.top,
            (overflow_rect.right - overflow_rect.left).max(scrollport.size.width),
            (overflow_rect.bottom - overflow_rect.top).max(scrollport.size.height),
        );
        let (mut overflow_x, mut overflow_y) = data
            .css
            .as_ref()
            .map_or((Overflow::Visible, Overflow::Visible), |css| {
                (*css.get_overflow_x(), *css.get_overflow_y())
            });
        if overflow_y.is_scrollable()
            && padding.bottom > 0.0
            && matches!(data.content, Content::Container | Content::Table(_))
        {
            // Taffy includes child overflow but can omit the parent's end
            // padding beyond its in-flow margin boxes.
            let bottom = data
                .children
                .iter()
                .filter_map(|&child| {
                    let child = &self.tree[child];
                    let layout = child.layout;
                    (!child.style.position.is_out_of_flow()
                        && child.style.display != taffy::Display::None)
                        .then_some(
                            origin.y
                                + layout.location.y
                                + layout.size.height
                                + layout.margin.bottom,
                        )
                })
                .fold(overflow.max_y(), |end, child_end| {
                    end.max(child_end + padding.bottom)
                });
            overflow.size.height = bottom - overflow.origin.y;
        }
        if let Content::Control(control) = &data.content
            && control.kind == crate::controls::Kind::List
            && (overflow_x, overflow_y) == (Overflow::Clip, Overflow::Clip)
        {
            (overflow_x, overflow_y) = (Overflow::Hidden, Overflow::Auto);
        }
        let editable = matches!(
            &data.content,
            Content::Control(control)
                if matches!(control.kind, crate::controls::Kind::Field(_) | crate::controls::Kind::Multiline)
        );
        let multiline = matches!(&data.content, Content::Control(control)
            if control.kind == crate::controls::Kind::Multiline);
        let scrollport = if editable {
            overflow = content_box;
            for &child in &data.children {
                if let Content::Inline(inline) = &self.tree[child].content
                    && let Some(layout) = &inline.layout
                {
                    overflow = editing::overflow(
                        layout,
                        self.positions.origins[usize::from(child)],
                        content_box,
                        !multiline,
                    );
                }
            }
            if multiline {
                overflow = Rect::new(
                    overflow.origin.x - padding.left,
                    overflow.origin.y - padding.top,
                    overflow.size.width + padding.left + padding.right,
                    overflow.size.height + padding.top + padding.bottom,
                );
                if overflow_x == Overflow::Visible {
                    overflow_x = Overflow::Auto;
                }
                if overflow_y == Overflow::Visible {
                    overflow_y = Overflow::Auto;
                }
                scrollport
            } else {
                (overflow_x, overflow_y) = (Overflow::Hidden, Overflow::Clip);
                content_box
            }
        } else {
            scrollport
        };
        let scrolling = |overflow| {
            matches!(
                overflow,
                Overflow::Auto | Overflow::Scroll | Overflow::Hidden
            )
        };
        let is_viewport = kind == FragmentKind::Viewport;
        let scroll =
            (is_viewport || overflow_x != Overflow::Visible || overflow_y != Overflow::Visible)
                .then_some(ScrollFrame {
                    owner,
                    // The document reconciles offsets after fragment construction.
                    offset: Point::default(),
                    generation: 0,
                    scrollport,
                    content: overflow,
                    gutter: Edges {
                        right: layout.scrollbar_size.width,
                        bottom: layout.scrollbar_size.height,
                        ..Default::default()
                    },
                    scroll_x: is_viewport || scrolling(overflow_x),
                    scroll_y: is_viewport || scrolling(overflow_y),
                    clip_x: is_viewport || overflow_x != Overflow::Visible,
                    clip_y: is_viewport || overflow_y != Overflow::Visible,
                });
        let sticky = (data.style.position == taffy::Position::Sticky).then(|| {
            let target = self
                .nodes
                .values()
                .find_map(|fragment| {
                    fragment
                        .scroll
                        .filter(|scroll| scroll.owner == scroll_owner)
                })
                .map_or_else(
                    || self.tree.boxes[0].layout.size,
                    |scroll| taffy::Size {
                        width: scroll.scrollport.size.width,
                        height: scroll.scrollport.size.height,
                    },
                );
            let resolve = |value: taffy::LengthPercentageAuto, basis| {
                value.resolve_to_option(basis, |handle, basis| {
                    self.tree.calcs.resolve(handle, basis)
                })
            };
            StickyConstraints {
                scroll_owner,
                containing_rect: parent_content.unwrap_or(rect),
                insets: Edges {
                    top: resolve(data.style.inset.top, target.height),
                    bottom: resolve(data.style.inset.bottom, target.height),
                    left: resolve(data.style.inset.left, target.width),
                    right: resolve(data.style.inset.right, target.width),
                },
            }
        });

        let mut style = override_style.unwrap_or_else(|| {
            data.css.as_ref().map_or_else(PaintValues::default, |css| {
                paint::project(
                    css,
                    self.resources,
                    self.images,
                    rect.size,
                    scrollport.size,
                    content_box.size,
                )
            })
        });
        style.stacking.flex_or_grid_item =
            parent
                .and_then(|id| self.nodes.get(&id))
                .is_some_and(|parent| {
                    matches!(
                        parent.style.stacking.display,
                        squark_types::fragment::DisplayKind::Flex
                            | squark_types::fragment::DisplayKind::InlineFlex
                            | squark_types::fragment::DisplayKind::Grid
                            | squark_types::fragment::DisplayKind::InlineGrid
                    )
                });
        style.box_paint.background_propagated = self.propagation == Some(node);
        let state = self.document.control_state(owner);
        if self.document.html_element(owner) == Some("button")
            && data.css.as_ref().is_some_and(|css| {
                *css.get_appearance() != style::values::computed::Appearance::None
            })
        {
            crate::controls::box_chrome(
                &mut style,
                border,
                crate::controls::Kind::InputButton,
                state,
            );
        }
        let content = match &data.content {
            Content::Control(control) => {
                if control.native {
                    crate::controls::box_chrome(&mut style, border, control.kind, state);
                }
                crate::controls::control_art(
                    control,
                    state,
                    rect,
                    content_box,
                    data.style.direction.is_rtl(),
                    data.css
                        .as_ref()
                        .is_some_and(|css| css.get_overflow_y().is_scrollable()),
                )
            }
            Content::Replaced(_, crate::Replacement::Image(image)) => {
                let natural = crate::replaced_size(image.intrinsic, taffy::Size::NONE);
                FragmentContent::Image {
                    key: image.key,
                    intrinsic_size: Size {
                        width: natural.width,
                        height: natural.height,
                    },
                }
            }
            Content::Replaced(_, crate::Replacement::Canvas(bitmap_size)) => {
                FragmentContent::Canvas {
                    id: owner,
                    bitmap_size: *bitmap_size,
                }
            }
            Content::Replaced(_, crate::Replacement::BrokenImage) => {
                crate::controls::broken_image(content_box.size, data.style.direction.is_rtl())
            }
            Content::Replaced(_, crate::Replacement::Svg) => FragmentContent::Svg(Arc::new(
                crate::svg::geometry(self.document, owner, content_box.size).unwrap_or_default(),
            )),
            _ => FragmentContent::None,
        };
        let children = data.children.clone();
        let scroll_owner = scroll
            .filter(|scroll| scroll.scroll_x || scroll.scroll_y)
            .map_or(scroll_owner, |scroll| scroll.owner);
        self.nodes.insert(
            id,
            Fragment {
                id,
                owner,
                kind,
                parent,
                containing_block: self.positions.containing[usize::from(node)]
                    .and_then(|block| self.ids_by_box.get(&block).copied())
                    .or(parent),
                children: Box::default(),
                border_box: rect,
                padding,
                border,
                first_baseline: baseline.first.map(|baseline| origin.y + baseline),
                last_baseline: baseline.last.map(|baseline| origin.y + baseline),
                scrollable_overflow: overflow,
                scroll,
                sticky,
                content,
                style: Arc::new(style),
            },
        );
        let mut fragments = Vec::new();
        for child in children {
            if matches!(self.tree[child].content, Content::Inline(_)) {
                self.emit_lines(child, id, content_box, scroll_owner, &mut fragments);
            } else {
                fragments.push(self.emit_box(
                    child,
                    Some(id),
                    Some(content_box),
                    scroll_owner,
                    None,
                ));
            }
        }
        self.nodes.get_mut(&id).expect("emitted parent").children = fragments.into_boxed_slice();
        id
    }

    fn emit_lines(
        &mut self,
        node: Id,
        parent: FragmentId,
        containing_rect: Rect,
        scroll_owner: NodeId,
        fragments: &mut Vec<FragmentId>,
    ) {
        let origin = self.positions.origins[usize::from(node)];
        let owner = self.tree[node].owner;
        let inline = std::mem::replace(&mut self.tree[node].content, Content::Container);
        let Content::Inline(mut inline) = inline else {
            unreachable!()
        };
        let Some(layout) = inline.layout.take() else {
            return;
        };
        let text_positions = inline.take_text_positions();
        let edit_style = self.tree[node].css.clone();
        let focused = self.document.focused() == Some(owner)
            && text_positions.is_some()
            && self.document.control_state(owner).bits()
                & squark_types::journal::ElementState::DISABLED.bits()
                == 0;
        let selection = text_positions
            .as_ref()
            .filter(|_| focused)
            .map_or_else(Vec::new, |positions| positions.selection(&layout));
        let preedit = text_positions
            .as_ref()
            .filter(|_| focused)
            .map_or_else(Vec::new, |positions| positions.preedit(&layout));
        let selection_style = (!selection.is_empty())
            .then(|| {
                self.document
                    .pseudo_style(owner, style::selector_parser::PseudoElement::Selection)
            })
            .flatten();
        let selection_color = selection_style.as_ref().map_or(
            PaintColor::srgb(51.0 / 255.0, 103.0 / 255.0, 209.0 / 255.0, 1.0),
            |css| {
                paint::color(
                    &css.get_background()
                        .background_color
                        .resolve_to_absolute(css.get_color()),
                )
            },
        );
        let selection_ink = (!selection.is_empty()).then(|| {
            selection_style
                .as_ref()
                .map_or(PaintColor::srgb(1.0, 1.0, 1.0, 1.0), |css| {
                    paint::color(css.get_color())
                })
        });
        if let Some(css) = &edit_style {
            for &(rect, _) in &selection {
                let rect = editing::place(origin, rect);
                fragments.push(self.edit_rectangle(owner, parent, rect, selection_color, css));
            }
        }
        let mut last_paint: Option<(&crate::Css, Arc<PaintValues>)> = None;
        let mut line_ids = Vec::new();
        for (line_index, line) in layout.lines().take(inline.line_count(&layout)).enumerate() {
            let metrics = line.metrics();
            let line_box = Rect::new(
                origin.x + metrics.inline_min_coord,
                origin.y + metrics.block_min_coord,
                metrics.inline_max_coord - metrics.inline_min_coord,
                metrics.line_height,
            );
            let line_id = self.identify(owner, FragmentKind::Line);
            let mut children = Vec::new();
            let mut glyph_cursor = HashMap::new();
            let mut slices = Slices::default();
            let parent_kind = self.nodes[&parent].kind;
            let marker = inline
                .overflow_marker
                .as_ref()
                .filter(|marker| metrics.advance - metrics.hanging_advance > marker.width);
            let aligned_start = origin.x + metrics.inline_min_coord + metrics.offset;
            let mut decoration_extent = [aligned_start, aligned_start + metrics.advance];
            if layout.is_rtl() {
                decoration_extent[0] += metrics.hanging_advance;
                if let Some(marker) = marker {
                    decoration_extent[0] =
                        decoration_extent[0].max(origin.x + marker.layout.width());
                }
            } else {
                decoration_extent[1] -= metrics.hanging_advance;
                if let Some(marker) = marker {
                    decoration_extent[1] =
                        decoration_extent[1].min(origin.x + marker.width - marker.layout.width());
                }
            }
            for item in line.items() {
                match item {
                    PositionedLayoutItem::InlineBox(item) => {
                        let child = Id::from(item.id as usize);
                        let data = &self.tree[child];
                        if let Content::InlineEdge(left) = data.content {
                            let margin = data.layout.margin;
                            let x = origin.x + item.x;
                            let x = if left { x + margin.left } else { x };
                            let width = item.width - if left { margin.left } else { margin.right };
                            slices.edge(self.document, data.owner, owner, x, width, left);
                        } else {
                            let child_rect = Rect::new(
                                origin.x + item.x,
                                origin.y + item.y,
                                item.width,
                                item.height,
                            );
                            if let Some(ancestor) = self.document.parent(data.owner) {
                                slices.text(self.document, ancestor, owner, child_rect);
                            }
                            children.push(self.emit_box(
                                child,
                                Some(line_id),
                                Some(containing_rect),
                                scroll_owner,
                                None,
                            ));
                        }
                    }
                    PositionedLayoutItem::GlyphRun(item) => {
                        let run = item.run();
                        let index = run.index();
                        let cursor = glyph_cursor.entry(index).or_insert(0);
                        let glyph_sources: Vec<_> = run
                            .visual_clusters()
                            .flat_map(|cluster| {
                                std::iter::repeat_n(cluster.text_range(), cluster.glyphs().count())
                            })
                            .collect();
                        let mut ranges = Vec::<TextRange>::new();
                        let mut glyphs = Vec::new();
                        let mut selected_glyphs = Vec::new();
                        let first_byte = glyph_sources.get(*cursor).map(|range| range.start);
                        let mut previous = None;
                        let mut cluster = 0;
                        let mut shaped_count = 0;
                        for (offset, glyph) in item.positioned_glyphs().enumerate() {
                            shaped_count += 1;
                            if let Some(bytes) = glyph_sources.get(*cursor + offset)
                                && previous.as_ref() != Some(bytes)
                            {
                                previous = Some(bytes.clone());
                                cluster = ranges.len() as u32;
                                ranges.extend(inline.ranges(bytes.clone()));
                            }
                            if marker.is_some_and(|marker| {
                                let extent = marker.layout.width();
                                if layout.is_rtl() {
                                    glyph.x < extent
                                } else {
                                    glyph.x + glyph.advance > marker.width - extent
                                }
                            }) {
                                continue;
                            }
                            if (cluster as usize) >= ranges.len() {
                                continue;
                            }
                            let selected = selection_ink.is_some()
                                && glyph_sources.get(*cursor + offset).is_some_and(|bytes| {
                                    text_positions
                                        .as_ref()
                                        .is_some_and(|p| p.selected_at_byte(bytes.start))
                                });
                            let target = if selected {
                                &mut selected_glyphs
                            } else {
                                &mut glyphs
                            };
                            target.push(PositionedGlyph {
                                glyph_id: glyph.id,
                                position: glyph_origin(
                                    self.raster_scale,
                                    origin.x + glyph.x,
                                    origin.y + glyph.y,
                                ),
                                cluster,
                            });
                        }
                        *cursor += shaped_count;
                        if ranges.is_empty() {
                            continue;
                        }
                        let style_owner = inline.style_owner(item.style().brush.0, owner);
                        let text_owner = ranges.first().map_or(style_owner, |range| range.node);
                        let font = self.tree.fonts.intern(
                            run.font(),
                            run.font_size(),
                            &run.normalized_coords()
                                .iter()
                                .map(|coord| coord.to_bits())
                                .collect::<Vec<_>>(),
                        );
                        let baseline = origin.y + item.baseline();
                        let text_box = Rect::new(
                            origin.x + item.offset(),
                            baseline - item.span_metrics().ascent,
                            item.advance(),
                            item.span_metrics().ascent + item.span_metrics().descent,
                        );
                        slices.text(self.document, style_owner, owner, text_box);
                        let css = first_byte.and_then(|byte| inline.style_at(byte));
                        let paint = match css {
                            Some(css) => {
                                if let Some((cached, paint)) = &last_paint
                                    && style::servo_arc::Arc::ptr_eq(cached, css)
                                {
                                    paint.clone()
                                } else {
                                    let paint = Arc::new(paint::text(css));
                                    last_paint = Some((css, paint.clone()));
                                    paint
                                }
                            }
                            None => self.empty_paint.clone(),
                        };
                        let mut decoration_spans = css.map_or_else(Box::default, |css| {
                            decorations::spans(
                                self.document,
                                match parent_kind {
                                    FragmentKind::Before | FragmentKind::After => Some(style_owner),
                                    FragmentKind::Marker => None,
                                    _ => self.document.parent(style_owner),
                                },
                                css,
                                &item,
                                [
                                    text_box.origin.x.max(decoration_extent[0]),
                                    text_box.max_x().min(decoration_extent[1]),
                                ],
                                baseline,
                            )
                        });
                        if !preedit.is_empty() {
                            let mut spans = decoration_spans.into_vec();
                            for &(segment, _) in preedit.iter().filter(|part| part.1 == line_index)
                            {
                                let x = [
                                    text_box.origin.x.max(origin.x + segment.origin.x),
                                    text_box.max_x().min(origin.x + segment.max_x()),
                                ];
                                if x[0] < x[1] {
                                    spans.push(DecorationSpan {
                                        x,
                                        y: baseline + 1.0,
                                        thickness: 1.0,
                                        style: DecorationStyle::Solid,
                                        color: paint.art.color,
                                        kind: DecorationKind::Underline,
                                    });
                                }
                            }
                            decoration_spans = spans.into_boxed_slice();
                        }
                        let positioned = PositionedRun {
                            font,
                            baseline,
                            bidi_level: u8::from(run.is_rtl()),
                            glyphs: glyphs.into_boxed_slice(),
                            source_ranges: ranges.into_boxed_slice(),
                            decoration_spans,
                        };
                        let selection_run = selection_ink
                            .filter(|_| !selected_glyphs.is_empty())
                            .map(|color| {
                                let mut selection_paint = (*paint).clone();
                                selection_paint.art.color = color;
                                let run = PositionedRun {
                                    glyphs: selected_glyphs.into_boxed_slice(),
                                    source_ranges: positioned.source_ranges.clone(),
                                    decoration_spans: Box::default(),
                                    ..positioned
                                };
                                (Arc::new(selection_paint), run)
                            });
                        let text_id = self.identify(text_owner, FragmentKind::Text);
                        let mut text = simple_fragment(
                            text_id,
                            text_owner,
                            FragmentKind::Text,
                            line_id,
                            text_box,
                            baseline,
                            paint,
                        );
                        text.content = FragmentContent::Text(Box::new([positioned]));
                        self.nodes.insert(text_id, text);
                        children.push(text_id);
                        if let Some((paint, run)) = selection_run {
                            let text_id = self.identify(text_owner, FragmentKind::Text);
                            let mut text = simple_fragment(
                                text_id,
                                text_owner,
                                FragmentKind::Text,
                                line_id,
                                text_box,
                                baseline,
                                paint,
                            );
                            text.content = FragmentContent::Text(Box::new([run]));
                            self.nodes.insert(text_id, text);
                            children.push(text_id);
                        }
                    }
                }
            }
            if let Some(marker) = marker {
                let baseline = origin.y + metrics.baseline;
                let x = if layout.is_rtl() {
                    origin.x
                } else {
                    origin.x + (marker.width - marker.layout.width()).max(0.0)
                };
                for part in marker.layout.lines().flat_map(|line| line.items()) {
                    let PositionedLayoutItem::GlyphRun(part) = part else {
                        continue;
                    };
                    let run = part.run();
                    let font = self.tree.fonts.intern(
                        run.font(),
                        run.font_size(),
                        &run.normalized_coords()
                            .iter()
                            .map(|coord| coord.to_bits())
                            .collect::<Vec<_>>(),
                    );
                    let glyphs = part
                        .positioned_glyphs()
                        .map(|glyph| PositionedGlyph {
                            glyph_id: glyph.id,
                            position: glyph_origin(
                                self.raster_scale,
                                x + glyph.x,
                                baseline + glyph.y - part.baseline(),
                            ),
                            cluster: 0,
                        })
                        .collect();
                    let painted = PositionedRun {
                        font,
                        baseline,
                        bidi_level: u8::from(run.is_rtl()),
                        glyphs,
                        source_ranges: Box::new([TextRange {
                            node: owner,
                            start_utf16: 0,
                            end_utf16: 0,
                        }]),
                        decoration_spans: Box::default(),
                    };
                    let paint = self.tree[node].css.as_ref().map_or_else(
                        || self.empty_paint.clone(),
                        |css| Arc::new(paint::text(css)),
                    );
                    let id = self.identify(owner, FragmentKind::Text);
                    let rect = Rect::new(
                        x + part.offset(),
                        baseline - part.span_metrics().ascent,
                        part.advance(),
                        part.span_metrics().ascent + part.span_metrics().descent,
                    );
                    let mut fragment = simple_fragment(
                        id,
                        owner,
                        FragmentKind::Text,
                        line_id,
                        rect,
                        baseline,
                        paint,
                    );
                    fragment.content = FragmentContent::Text(Box::new([painted]));
                    self.nodes.insert(id, fragment);
                    children.push(id);
                }
            }
            let mut decorated = Vec::new();
            for slice in slices.finish(containing_rect.size.width, self.resources, self.images) {
                let id = self.identify(slice.owner, FragmentKind::Box);
                let mut fragment = simple_fragment(
                    id,
                    slice.owner,
                    FragmentKind::Box,
                    line_id,
                    slice.rect,
                    origin.y + metrics.baseline,
                    Arc::new(slice.style),
                );
                fragment.padding = slice.padding;
                fragment.border = slice.border;
                self.nodes.insert(id, fragment);
                decorated.push(id);
            }
            decorated.append(&mut children);
            let mut fragment = simple_fragment(
                line_id,
                owner,
                FragmentKind::Line,
                parent,
                line_box,
                origin.y + metrics.baseline,
                self.empty_paint.clone(),
            );
            fragment.children = decorated.into_boxed_slice();
            self.nodes.insert(line_id, fragment);
            fragments.push(line_id);
            line_ids.push(line_id);
        }
        let generated = matches!(
            self.nodes[&parent].kind,
            FragmentKind::Before | FragmentKind::After | FragmentKind::Marker
        );
        if text_positions.is_none()
            && !generated
            && let Some((text, sources)) = inline.take_sources()
        {
            use style::computed_values::white_space_collapse::T as Collapse;
            let collapsible = edit_style.as_ref().is_some_and(|css| {
                matches!(
                    css.get_inherited_text().white_space_collapse,
                    Collapse::Collapse | Collapse::PreserveBreaks
                )
            });
            // Keep the paragraph for text range and caret queries. It moves
            // the layout and sources that would be dropped here.
            self.flows.push(Flow {
                fragment: parent,
                origin,
                lines: line_ids.into_boxed_slice(),
                layout,
                text,
                sources,
                collapsible,
            });
            return;
        }
        if let Some(positions) = text_positions {
            let field = editing::Field {
                fragment: parent,
                origin,
                positions,
                layout,
            };
            if focused
                && selection.is_empty()
                && let Some(css) = &edit_style
            {
                let caret_color = match &css.get_caret_color().0 {
                    style::values::generics::color::ColorOrAuto::Auto => {
                        paint::color(css.get_color())
                    }
                    style::values::generics::color::ColorOrAuto::Color(color) => {
                        paint::color(&color.resolve_to_absolute(css.get_color()))
                    }
                };
                fragments.push(self.edit_rectangle(
                    owner,
                    parent,
                    field.active_caret(),
                    caret_color,
                    css,
                ));
            }
            self.editing.insert(owner, field);
        }
    }

    fn edit_rectangle(
        &mut self,
        owner: NodeId,
        parent: FragmentId,
        rect: Rect,
        color: PaintColor,
        css: &crate::Css,
    ) -> FragmentId {
        let id = self.identify(owner, FragmentKind::Line);
        let mut values = paint::text(css);
        values.interaction.pointer_events = PointerEvents::None;
        let mut fragment = simple_fragment(
            id,
            owner,
            FragmentKind::Line,
            parent,
            rect,
            0.0,
            Arc::new(values),
        );
        fragment.content = crate::controls::edit_fill(rect, color);
        self.nodes.insert(id, fragment);
        id
    }
}

fn simple_fragment(
    id: FragmentId,
    owner: NodeId,
    kind: FragmentKind,
    parent: FragmentId,
    rect: Rect,
    baseline: f32,
    paint: Arc<PaintValues>,
) -> Fragment {
    Fragment {
        id,
        owner,
        kind,
        parent: Some(parent),
        containing_block: Some(parent),
        children: Box::default(),
        border_box: rect,
        border: Edges::default(),
        padding: Edges::default(),
        first_baseline: Some(baseline),
        last_baseline: Some(baseline),
        scrollable_overflow: rect,
        scroll: None,
        sticky: None,
        content: FragmentContent::None,
        style: paint,
    }
}

fn edges(rect: taffy::Rect<f32>) -> Edges<f32> {
    Edges {
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
        left: rect.left,
    }
}

pub(crate) fn inset(rect: Rect, edges: Edges<f32>) -> Rect {
    Rect::new(
        rect.origin.x + edges.left,
        rect.origin.y + edges.top,
        (rect.size.width - edges.left - edges.right).max(0.0),
        (rect.size.height - edges.top - edges.bottom).max(0.0),
    )
}

// Keep layout and range geometry fractional. Horizontal text masks use the
// nearest quarter device pixel on the baseline and an integer cross-axis
// pixel. Bias a converted CSS coordinate only when it falls just below the
// raster bin boundary after narrowing to f32.
fn glyph_origin(scale: f64, x: f32, y: f32) -> Point {
    let device_x = (f64::from(x) * scale * 4.0 + 0.5).floor() / 4.0;
    let device_y = (f64::from(y) * scale + 0.5).floor();
    let to_css = |device| {
        let css = (device / scale) as f32;
        if f64::from(css) * scale < device {
            css.next_up()
        } else {
            css
        }
    };
    Point {
        x: to_css(device_x),
        y: to_css(device_y),
    }
}

#[cfg(test)]
mod tests {
    use squark_types::render::{FontInstanceId, GlyphKey};

    use super::*;

    #[test]
    fn snaps_glyphs_to_device_samples_at_fractional_dpr_and_negative_positions() {
        let dpr = 1.333;
        let scale = (dpr * 1024.0_f64).round() / 1024.0;
        let glyph = glyph_origin(scale, -1.15, 12.4);
        let key = GlyphKey::for_position(FontInstanceId(1), 0, glyph, dpr);
        assert_eq!(key.phase, [2, 0]);
        assert_eq!(key.device_origin(glyph), [-2.0, 17.0]);

        let glyph = glyph_origin(1.0, 10.9, -0.55);
        let key = GlyphKey::for_position(FontInstanceId(1), 0, glyph, 1.0);
        assert_eq!(key.phase, [0, 0]);
        assert_eq!(key.device_origin(glyph), [11.0, -1.0]);
    }
}
