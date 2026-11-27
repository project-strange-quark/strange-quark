// SPDX-License-Identifier: MIT
//! Builds CSS boxes from the styled mirror, then emits unscrolled fragments.

mod boxes;
mod canvas;
mod controls;
mod engine;
mod font_loads;
mod html_fragment;
mod image_loads;
mod query;
mod resources;
mod scroll;
mod svg;
mod svg_text;
pub use engine::StaticDocument;
pub use font_loads::FontCompletion;
pub use squark_types::engine::ImageEvent;
mod css;
mod decorations;
mod editing;
mod fragments;
mod inline;
mod inline_paint;
mod motion;
mod observers;
mod paint;
mod table;
mod taffy_tree;
mod text_flow;

use std::{
    ops::{Index, IndexMut},
    rc::Rc,
};

use squark_assets::{Image, ImageCatalog, IntrinsicSize};
use squark_mirror::Document;
use squark_style::Cascade;
use squark_text::{DocumentFont, Fonts, WebFont};
use squark_types::{
    NodeId, Size,
    fragment::{FragmentKind, FragmentTree},
};
use style::{properties::ComputedValues, servo_arc::Arc};
use taffy::{AvailableSpace, Baselines, Cache, ContainingBlockClaims, DetailedLayoutInfo, Layout};

use css::Calcs;
use inline::Inline;

type Id = taffy::NodeId;
type Css = Arc<ComputedValues>;

#[derive(Debug, Default)]
pub struct LayoutEngine {
    fonts: Fonts,
    document_fonts: Vec<DocumentFont>,
    lines: inline::LineCache,
    images: ImageCatalog,
    ids: fragments::FragmentIds,
    margins: fragments::Margins,
    editing: editing::Fields,
    table_parts: table::Parts,
    anonymous: std::collections::BTreeSet<squark_types::fragment::FragmentId>,
    flows: text_flow::Flows,
    measure_timings: bool,
}

impl LayoutEngine {
    /// Lay out a mirror after the cascade has been updated for this viewport.
    pub fn layout(
        &mut self,
        document: &Document,
        cascade: &Cascade<'_>,
        viewport: Size,
    ) -> FragmentTree {
        let start = self.measure_timings.then(std::time::Instant::now);
        let resources = cascade.resources();
        self.sync_fonts(document, cascade);
        let mut tree = BoxTree {
            boxes: Vec::new(),
            calcs: Rc::default(),
            fonts: &mut self.fonts,
            lines: &mut self.lines,
        };
        let root = boxes::build(
            &mut tree,
            document,
            cascade,
            resources,
            &mut self.images,
            viewport,
        );
        let boxes_done = self.measure_timings.then(std::time::Instant::now);
        taffy::compute_root_layout(
            &mut tree,
            root,
            taffy::Size {
                width: AvailableSpace::Definite(viewport.width),
                height: AvailableSpace::Definite(viewport.height),
            },
        );
        tree.lines.finish_layout();
        let taffy_done = self.measure_timings.then(std::time::Instant::now);
        let emitted = fragments::emit(
            tree,
            root,
            document,
            resources,
            &mut self.images,
            &mut self.ids,
            f64::from(cascade.device_pixel_ratio()),
        );
        if let (Some(start), Some(boxes_done), Some(taffy_done)) = (start, boxes_done, taffy_done) {
            eprintln!(
                "squark-layout boxes_us={} taffy_us={} emit_us={}",
                (boxes_done - start).as_micros(),
                (taffy_done - boxes_done).as_micros(),
                taffy_done.elapsed().as_micros()
            );
        }
        self.margins = emitted.margins;
        self.editing = emitted.editing;
        self.table_parts = emitted.table_parts;
        self.anonymous = emitted.anonymous;
        self.flows = emitted.flows;
        emitted.tree
    }

    fn edit_textarea(
        &mut self,
        document: &Document,
        cascade: &Cascade<'_>,
        snapshot: &FragmentTree,
        id: squark_types::fragment::FragmentId,
    ) -> (FragmentTree, Option<editing::Field>) {
        use taffy::{Dimension, LayoutInput, LayoutPartialTree, LengthPercentage, RunMode};

        let old = &snapshot.nodes[&id];
        let owner = old.owner;
        let css = document.computed_style(owner).expect("styled textarea");
        let mut calcs = Calcs::default();
        let mut style = calcs.style(&css);
        let len = |edges: squark_types::fragment::Edges<f32>| taffy::Rect {
            left: LengthPercentage::length(edges.left),
            right: LengthPercentage::length(edges.right),
            top: LengthPercentage::length(edges.top),
            bottom: LengthPercentage::length(edges.bottom),
        };
        style.padding = len(old.padding);
        style.border = len(old.border);
        let size = taffy::Size {
            width: old.border_box.size.width,
            height: old.border_box.size.height,
        };
        style.size = size.map(Dimension::length);
        style.box_sizing = taffy::BoxSizing::BorderBox;
        let plan = controls::build(document, cascade, &mut self.fonts, owner, &css)
            .expect("textarea plan");
        let mut tree = BoxTree {
            boxes: Vec::new(),
            calcs: Rc::new(calcs),
            fonts: &mut self.fonts,
            lines: &mut self.lines,
        };
        let root = plan.attach(&mut tree, owner, css, style);
        tree.compute_child_layout(
            root,
            LayoutInput {
                run_mode: RunMode::PerformLayout,
                known_dimensions: size.map(Some),
                available_space: size.map(AvailableSpace::Definite),
                ..LayoutInput::HIDDEN
            },
        );
        let scroll = old.scroll.expect("textarea scroll frame");
        tree[root].layout = Layout {
            size,
            location: taffy::Point {
                x: old.border_box.origin.x,
                y: old.border_box.origin.y,
            },
            padding: style_edges(old.padding),
            border: style_edges(old.border),
            scrollbar_size: taffy::Size {
                width: scroll.gutter.right,
                height: scroll.gutter.bottom,
            },
            ..Layout::new()
        };
        fragments::Emitter::new(
            tree,
            root,
            document,
            cascade.resources(),
            &mut self.images,
            &mut self.ids,
            f64::from(cascade.device_pixel_ratio()),
        )
        .emit_edited_field(root, snapshot, id)
    }

    fn sync_fonts(&mut self, document: &Document, cascade: &Cascade<'_>) {
        let faces = cascade
            .font_faces(document)
            .iter()
            .filter_map(WebFont::from_descriptors)
            .collect();
        if self
            .fonts
            .sync_web_fonts(cascade.resources(), faces, &self.document_fonts)
        {
            self.lines = inline::LineCache::default();
        }
    }

    pub fn images(&self) -> &ImageCatalog {
        &self.images
    }

    pub(crate) fn images_mut(&mut self) -> &mut ImageCatalog {
        &mut self.images
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }
}

struct BoxTree<'a> {
    boxes: Vec<BoxNode>,
    calcs: Rc<Calcs>,
    fonts: &'a mut Fonts,
    lines: &'a mut inline::LineCache,
}

struct BoxNode {
    owner: NodeId,
    kind: FragmentKind,
    css: Option<Css>,
    style: taffy::Style<style::Atom>,
    content: Content,
    children: Vec<Id>,
    hoisted: Vec<Id>,
    layout: Layout,
    cache: Cache,
    baselines: Baselines,
    details: DetailedLayoutInfo<style::Atom>,
    claims: ContainingBlockClaims,
    marker_outside: bool,
    /// How far a table cell's layout moved its in-flow children down for
    /// `vertical-align` since they were last laid out.
    content_offset: f32,
    /// An anonymous table box, which belongs to no element of its own.
    anonymous: bool,
}

enum Content {
    Container,
    Table(table::Table),
    InlineEdge(bool),
    Inline(Box<Inline>),
    Replaced(IntrinsicSize, Replacement),
    Control(Box<controls::Control>),
}

enum Replacement {
    Image(Image),
    Canvas([u32; 2]),
    BrokenImage,
    EmptyImage,
    Svg,
}

impl BoxNode {
    /// The baseline that aligns the box as an inline-block: the last line
    /// box's for a block container, which tables do not give it, and the
    /// first baseline for other boxes that have no last one.
    fn inline_block_baseline(&self, baselines: Baselines) -> Option<f32> {
        let block_container = matches!(self.content, Content::Container)
            && matches!(
                self.style.display,
                taffy::Display::Block | taffy::Display::FlowRoot
            );
        baselines
            .last
            .or(baselines.first.filter(|_| !block_container))
    }
}

impl BoxTree<'_> {
    fn add(
        &mut self,
        owner: NodeId,
        kind: FragmentKind,
        css: Option<Css>,
        style: taffy::Style<style::Atom>,
        content: Content,
    ) -> Id {
        let id = Id::from(self.boxes.len());
        let claims = if kind == FragmentKind::Viewport {
            ContainingBlockClaims::ALL
        } else {
            css.as_ref().map_or(ContainingBlockClaims::NONE, |css| {
                css::containing_block_claims(css)
            })
        };
        self.boxes.push(BoxNode {
            owner,
            kind,
            css,
            style,
            content,
            children: Vec::new(),
            hoisted: Vec::new(),
            layout: Layout::new(),
            cache: Cache::new(),
            baselines: Baselines::NONE,
            details: DetailedLayoutInfo::None,
            claims,
            marker_outside: false,
            content_offset: 0.0,
            anonymous: false,
        });
        id
    }
}

impl Index<Id> for BoxTree<'_> {
    type Output = BoxNode;
    fn index(&self, id: Id) -> &Self::Output {
        &self.boxes[usize::from(id)]
    }
}

impl IndexMut<Id> for BoxTree<'_> {
    fn index_mut(&mut self, id: Id) -> &mut Self::Output {
        &mut self.boxes[usize::from(id)]
    }
}

fn replaced_size(intrinsic: IntrinsicSize, known: taffy::Size<Option<f32>>) -> taffy::Size<f32> {
    let ratio = intrinsic
        .ratio
        .filter(|ratio| ratio.is_finite() && *ratio > 0.0);
    let (width, height) = match (known.width, known.height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (
            width,
            ratio
                .map(|ratio| width / ratio)
                .or(intrinsic.height)
                .unwrap_or(150.0),
        ),
        (None, Some(height)) => (
            ratio
                .map(|ratio| height * ratio)
                .or(intrinsic.width)
                .unwrap_or(300.0),
            height,
        ),
        (None, None) => {
            let [width, height] = intrinsic.default_object_size();
            (width, height)
        }
    };
    taffy::Size { width, height }
}

/// The offset `position: relative` gives a box (CSS 2 § 9.4.3): `left` wins
/// over `right`, or the reverse in a right-to-left containing block, and
/// `top` over `bottom`. A percentage without a basis is `auto`.
fn relative_offset(
    style: &taffy::Style<style::Atom>,
    basis: taffy::Size<Option<f32>>,
    rtl: bool,
    calcs: &Calcs,
) -> taffy::Point<f32> {
    use taffy::MaybeResolve;

    if style.position != taffy::Position::Relative {
        return taffy::Point::ZERO;
    }
    let resolve = |value: taffy::LengthPercentageAuto, basis: Option<f32>| {
        value.maybe_resolve(basis, |handle, basis| calcs.resolve(handle, basis))
    };
    let inset = style.inset;
    let left = resolve(inset.left, basis.width);
    let right = resolve(inset.right, basis.width).map(|right| -right);
    taffy::Point {
        x: if rtl { right.or(left) } else { left.or(right) }.unwrap_or(0.0),
        y: resolve(inset.top, basis.height)
            .or_else(|| resolve(inset.bottom, basis.height).map(|bottom| -bottom))
            .unwrap_or(0.0),
    }
}

fn style_edges(edges: squark_types::fragment::Edges<f32>) -> taffy::Rect<f32> {
    taffy::Rect {
        left: edges.left,
        right: edges.right,
        top: edges.top,
        bottom: edges.bottom,
    }
}
