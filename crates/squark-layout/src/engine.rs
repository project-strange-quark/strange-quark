// SPDX-License-Identifier: MIT
//! The packed document thread entry point used by the composition binary.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

use squark_assets::Pack;
use squark_mirror::{Document, ScrollCommand};
use squark_style::{Cascade, screen};
use squark_types::{
    NodeId, Point, Rect,
    engine::{
        AnimationEvent, ColorScheme, DecodedBatch, DocumentEngine, EngineError, FaceId,
        FlushReport, FrameInput, IntersectionEntry, IntersectionOptions, MeasureText, MediaState,
        ParsedDocument, ParsedNode, PointerMedia, Query, QueryValue, ResizeBox, ResizeDelivery,
        SelectMode, TextMetrics,
    },
    fragment::{FragmentId, FragmentTree, MirrorRead},
    journal::Opcode,
    render::{FrameDemand, ResourceSender, ResourceUpdate, Viewport},
};
use style::{
    device::servo::MediaData, queries::values::PrefersColorScheme,
    servo::media_features::PointerCapabilities,
};
use url::Url;

use crate::{
    ImageEvent, LayoutEngine,
    canvas::CanvasSubmit,
    font_loads::{FontCompletion, FontLoads, FontMutation},
    image_loads::ImageLoads,
    query::{self, Geometry},
    resources::Uploads,
};

/// The runtime keeps this engine on the document thread and commits the
/// snapshot through `FramePainter` after a successful frame update.
pub struct StaticDocument<'pack> {
    document: Document,
    cascade: Cascade<'pack>,
    layout: LayoutEngine,
    uploads: Uploads,
    image_loads: ImageLoads,
    font_loads: FontLoads,
    snapshot: FragmentTree,
    boxes: query::BoxIndex,
    layout_input: Option<(Viewport, MediaState)>,
    scroll: crate::scroll::ScrollState,
    changed: bool,
    edited_textareas: BTreeSet<NodeId>,
    cascade_dirty: bool,
    reveal_caret: BTreeSet<NodeId>,
    observers: crate::observers::Observers,
    time_origin: Option<Instant>,
    measure_timings: bool,
}

impl std::fmt::Debug for StaticDocument<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StaticDocument")
            .field("document", &self.document.root())
            .field("fragments", &self.snapshot.nodes.len())
            .finish_non_exhaustive()
    }
}

impl<'pack> StaticDocument<'pack> {
    pub fn css_warnings(&self) -> &[squark_style::CssWarning] {
        self.cascade.css_warnings()
    }

    pub fn css_supports(&self, condition: &[u16]) -> bool {
        self.cascade.css_supports(condition)
    }

    pub fn from_pack(
        pack: &'pack Pack,
        document_url: Url,
        resources: ResourceSender,
    ) -> Result<Self, EngineError> {
        let (document, _, cascade) = Cascade::from_pack(pack, document_url, screen(0., 0., 1.))
            .map_err(|error| EngineError::Resource(error.to_string()))?;
        let image_loads = ImageLoads::new(pack, cascade.resources().document_url);
        let font_loads = FontLoads::new(pack, cascade.resources().document_url);
        let measure_timings =
            std::env::var_os("SQUARK_ENGINE_TIMINGS").is_some_and(|value| value == "1");
        Ok(Self {
            document,
            cascade,
            layout: LayoutEngine {
                measure_timings,
                ..LayoutEngine::default()
            },
            uploads: Uploads::new(resources),
            image_loads,
            font_loads,
            boxes: query::BoxIndex::default(),
            layout_input: None,
            scroll: Default::default(),
            changed: true,
            edited_textareas: BTreeSet::new(),
            cascade_dirty: true,
            reveal_caret: BTreeSet::new(),
            observers: Default::default(),
            time_origin: None,
            measure_timings,
            snapshot: FragmentTree {
                root: FragmentId(0),
                ime_area: None,
                nodes: BTreeMap::new(),
            },
        })
    }

    pub fn with_blob_resolver(
        mut self,
        resolver: std::sync::Arc<squark_assets::BlobResolver>,
    ) -> Self {
        self.cascade.set_blob_resolver(resolver);
        self
    }

    pub fn with_image_frame_demand(mut self, demand: FrameDemand) -> Self {
        self.font_loads.set_frame_demand(demand.clone());
        self.image_loads.set_frame_demand(demand);
        self
    }

    pub fn take_image_events(&mut self) -> Vec<ImageEvent> {
        self.image_loads.take_events(&self.document)
    }

    pub fn parse_color(&self, canvas: NodeId, text: &[u16]) -> Option<[u8; 4]> {
        self.cascade.canvas_color(&self.document, canvas, text)
    }

    pub fn validate_filter(&self, text: &[u16]) -> bool {
        self.cascade
            .canvas_filter(&self.document, None, text)
            .is_some()
            || self.cascade.canvas_filter_uses_url(&self.document, text)
    }

    pub fn canvas_filters(
        &self,
        canvas: Option<NodeId>,
        text: &[u16],
    ) -> Option<Box<[squark_types::fragment::Filter]>> {
        crate::canvas::parse_filter(&self.document, &self.cascade, canvas, text)
    }

    pub fn canvas_font_instance(
        &self,
        id: squark_types::render::FontInstanceId,
    ) -> Option<&squark_text::FontInstance> {
        self.layout.fonts.instance(id)
    }

    pub fn parse_canvas_font(&self, canvas: NodeId, text: &[u16]) -> Option<Vec<u16>> {
        self.cascade.canvas_font_css(&self.document, canvas, text)
    }

    pub fn take_font_completions(&mut self) -> Vec<FontCompletion> {
        self.collect_fonts();
        self.font_loads.drain()
    }

    fn collect_fonts(&mut self) {
        if self.font_loads.collect(self.cascade.resources()) {
            self.layout.document_fonts = self.font_loads.active_fonts();
            self.changed = true;
        }
    }

    fn resource_mutations<'a>(
        &self,
        batch: &DecodedBatch<'a>,
    ) -> Result<Vec<ResourceMutation<'a>>, EngineError> {
        let mut mutations = Vec::new();
        let mut buffers = BTreeSet::new();
        for (index, record) in batch.records.iter().enumerate() {
            let invalid = |reason: &str| EngineError::InvalidBatch {
                record: u32::try_from(index).ok(),
                reason: reason.into(),
            };
            let canvas = NodeId::from_raw(record.a);
            let mutation = match record.op {
                Opcode::CanvasSubmit => {
                    let bytes = batch
                        .staged_bytes
                        .iter()
                        .find(|(id, _)| *id == record.b)
                        .map(|(_, bytes)| *bytes)
                        .ok_or_else(|| invalid("missing canvas buffer"))?;
                    if bytes.len() != record.c as usize || !buffers.insert(record.b) {
                        return Err(invalid("invalid or duplicate canvas buffer"));
                    }
                    let submit = CanvasSubmit::parse(bytes).map_err(invalid)?;
                    ResourceMutation::CanvasSubmit(canvas, submit)
                }
                Opcode::CanvasSize => ResourceMutation::CanvasSize(canvas, [record.b, record.c]),
                Opcode::FontFace => ResourceMutation::Font(
                    FontMutation::parse(record, batch, &self.document, &self.cascade, &mut buffers)
                        .map_err(invalid)?,
                ),
                _ => continue,
            };
            mutations.push(mutation);
        }
        Ok(mutations)
    }

    fn publish_resources(
        &mut self,
        mutations: Vec<ResourceMutation<'_>>,
    ) -> Result<(), EngineError> {
        for mutation in mutations {
            match mutation {
                ResourceMutation::CanvasSize(id, size) => self
                    .uploads
                    .send(ResourceUpdate::ResizeCanvas { id, size })?,
                ResourceMutation::CanvasSubmit(id, submit) => {
                    self.restyle_before_read()?;
                    self.collect_fonts();
                    self.layout.sync_fonts(&self.document, &self.cascade);
                    let rewritten = submit
                        .rewrite(
                            id,
                            &self.document,
                            &self.cascade,
                            &mut self.layout.fonts,
                            &self.layout.images,
                            &mut self.font_loads,
                        )
                        .map_err(|error| EngineError::Resource(error.into()))?;
                    self.uploads
                        .canvas_fonts(&self.layout.fonts, rewritten.fonts)?;
                    self.uploads
                        .canvas_images(self.layout.images_mut(), rewritten.images)?;
                    self.uploads.send(ResourceUpdate::SubmitCanvas {
                        id,
                        stream: rewritten.stream,
                    })?;
                }
                ResourceMutation::Font(mutation) => {
                    self.font_loads.apply(mutation);
                    self.layout.document_fonts = self.font_loads.active_fonts();
                }
            }
        }
        Ok(())
    }

    fn start_image_sources(&mut self) {
        self.image_loads.start(
            &mut self.document,
            self.layout.images_mut(),
            self.cascade.resources(),
        );
    }
    fn geometry(&self) -> Geometry<'_> {
        Geometry {
            document: &self.document,
            tree: &self.snapshot,
            index: &self.boxes,
            margins: &self.layout.margins,
        }
    }

    fn svg_bounds(&mut self, target: NodeId) -> Option<(Rect, Rect, NodeId)> {
        let root = std::iter::successors(Some(target), |id| self.document.parent(*id))
            .filter(|&id| self.document.is_svg_root(id))
            .last()?;
        let geometry = self.geometry();
        let border = geometry.boxes(root).next().map(|f| {
            crate::fragments::inset(crate::fragments::inset(f.border_box, f.border), f.padding)
        });
        let size = border.map_or(
            squark_types::Size {
                width: 300.0,
                height: 150.0,
            },
            |r| r.size,
        );
        let (local, viewport) =
            crate::svg::bounds(&self.document, &mut self.layout.fonts, root, target, size)?;
        let origin = border.map_or(Point::default(), |r| r.origin);
        let viewport = Rect {
            origin: Point {
                x: viewport.origin.x + origin.x,
                y: viewport.origin.y + origin.y,
            },
            ..viewport
        };
        Some((local, viewport, root))
    }

    fn update_edited_textareas(&mut self, input: &FrameInput<'_>) -> Result<bool, EngineError> {
        let start = self.measure_timings.then(Instant::now);
        let edits = self
            .edited_textareas
            .iter()
            .map(|owner| {
                let field = self.layout.editing.get(owner)?;
                self.snapshot.nodes.get(&field.fragment)?.scroll?;
                Some((*owner, field.fragment))
            })
            .collect::<Option<Vec<_>>>();
        let Some(edits) = edits else {
            return Ok(false);
        };
        let mut fragments = 0;
        for (owner, id) in edits {
            let (patch, field) =
                self.layout
                    .edit_textarea(&self.document, &self.cascade, &self.snapshot, id);
            self.uploads.prepare(
                patch.nodes.values(),
                &mut self.layout,
                self.cascade.resources(),
                input.tick.viewport.scale,
            )?;
            fragments += patch.nodes.len();
            let mut pending = vec![id];
            while let Some(id) = pending.pop() {
                if let Some(old) = self.snapshot.nodes.remove(&id) {
                    pending.extend(old.children);
                }
            }
            self.snapshot.nodes.extend(patch.nodes);
            if let Some(field) = field {
                self.layout.editing.insert(owner, field);
            } else {
                self.layout.editing.remove(&owner);
            }
            self.scroll.publish(&mut self.snapshot, owner);
        }
        self.edited_textareas.clear();
        self.apply_scroll_commands(input, false);
        if let Some(start) = start {
            eprintln!(
                "squark-engine frame={} style_us=0 layout_us={} fragments={} scope=textarea",
                input.tick.sequence,
                start.elapsed().as_micros(),
                fragments
            );
        }
        Ok(true)
    }

    fn restyle_before_read(&mut self) -> Result<(), EngineError> {
        if self.cascade_dirty {
            self.cascade
                .restyle(&mut self.document)
                .map_err(|error| EngineError::Resource(error.to_string()))?;
            self.cascade_dirty = false;
        }
        Ok(())
    }

    fn field_client_rect(&self, field: &crate::editing::Field, rect: Rect) -> Rect {
        let fragment = &self.snapshot.nodes[&field.fragment];
        let scroll = fragment
            .scroll
            .map_or(Point::default(), |frame| frame.offset);
        let rect = crate::editing::place(
            Point {
                x: -scroll.x,
                y: -scroll.y,
            },
            rect,
        );
        self.geometry().client_rect(fragment, rect)
    }

    fn ime_area(&self) -> Option<Rect> {
        let field = self.layout.editing.get(&self.document.focused()?)?;
        let preedit = field.positions.preedit(&field.layout);
        Some(if preedit.is_empty() {
            self.field_client_rect(field, field.active_caret())
        } else {
            query::bounding(preedit.into_iter().map(|(rect, _)| {
                self.field_client_rect(field, crate::editing::place(field.origin, rect))
            }))
        })
    }

    fn apply_scroll_commands(&mut self, input: &FrameInput<'_>, new_layout: bool) {
        if new_layout {
            self.scroll.reindex(&self.snapshot);
        }
        self.scroll
            .apply_compositor(&mut self.snapshot, input.scroll.offsets);
        if new_layout {
            self.scroll.restore_offsets(&mut self.snapshot);
        }
        if let Some(node) = self.document.focused()
            && self.reveal_caret.contains(&node)
            && let Some(field) = self.layout.editing.get(&node)
            && let Some(fragment) = self.snapshot.nodes.get(&field.fragment)
            && let Some(scroll) = fragment.scroll
        {
            let caret = field.active_caret();
            let view = if self.document.html_element(node) == Some("textarea") {
                crate::fragments::inset(scroll.scrollport, fragment.padding)
            } else {
                scroll.scrollport
            };
            let reveal = |start, end, view_start, view_end, offset: f32| {
                if start < view_start + offset {
                    start - view_start
                } else if end > view_end + offset {
                    end - view_end
                } else {
                    offset
                }
            };
            let offset = Point {
                x: reveal(
                    caret.origin.x,
                    caret.max_x(),
                    view.origin.x,
                    view.max_x(),
                    scroll.offset.x,
                ),
                y: reveal(
                    caret.origin.y,
                    caret.max_y(),
                    view.origin.y,
                    view.max_y(),
                    scroll.offset.y,
                ),
            };
            self.scroll.scroll_to(&mut self.snapshot, node, offset);
        }
        self.reveal_caret.clear();
        for command in self.document.take_scroll_commands() {
            match command {
                ScrollCommand::To(mut owner, offset) => {
                    let element = self.document.html_element(owner);
                    if (element == Some("html")
                        && self.document.parent(owner) == Some(self.document.root()))
                        || (element == Some("body")
                            && self.document.quirks() == style::context::QuirksMode::Quirks)
                    {
                        owner = self.document.root();
                    }
                    self.scroll.scroll_to(&mut self.snapshot, owner, offset);
                }
                ScrollCommand::Focus(node) => {
                    for (owner, offset) in self.geometry().focus_scroll(node) {
                        self.scroll.scroll_to(&mut self.snapshot, owner, offset);
                    }
                }
            }
        }
        self.snapshot.ime_area = self.ime_area();
    }
}

enum ResourceMutation<'a> {
    CanvasSize(NodeId, [u32; 2]),
    CanvasSubmit(NodeId, CanvasSubmit<'a>),
    Font(FontMutation),
}

impl DocumentEngine for StaticDocument<'_> {
    fn flush(&mut self, batch: DecodedBatch<'_>) -> Result<FlushReport, EngineError> {
        let mutations = self.resource_mutations(&batch)?;
        let report =
            self.document
                .apply_decoded(batch)
                .map_err(|error| EngineError::InvalidBatch {
                    record: None,
                    reason: error.to_string(),
                })?;
        for record in batch.records {
            let owner = NodeId::from_raw(record.a);
            let edit = matches!(
                record.op,
                Opcode::SetValue | Opcode::SetSelection | Opcode::SetPreedit
            );
            let textarea = edit && self.document.html_element(owner) == Some("textarea");
            if textarea {
                self.edited_textareas.insert(owner);
            } else {
                self.changed |= record.op != Opcode::ScrollTo;
                self.cascade_dirty |= !matches!(
                    record.op,
                    Opcode::Focus | Opcode::ScrollTo | Opcode::SetSelection | Opcode::SetPreedit
                );
            }
            if edit || record.op == Opcode::Focus {
                self.reveal_caret.insert(owner);
            }
        }
        for &owner in &report.released {
            self.scroll.forget(owner);
            self.observers.forget(owner);
        }
        self.start_image_sources();
        self.publish_resources(mutations)?;
        Ok(report)
    }

    fn update_layout(&mut self, input: &FrameInput<'_>) -> Result<(), EngineError> {
        self.start_image_sources();
        self.collect_fonts();
        self.changed |= self.image_loads.complete(self.layout.images_mut());
        let origin = *self.time_origin.get_or_insert(input.tick.vsync);
        let animated = self.document.set_animation_time(
            input
                .tick
                .vsync
                .saturating_duration_since(origin)
                .as_secs_f64(),
        );
        let viewport = input.tick.viewport;
        let layout_input = (viewport, input.media);
        if !animated && !self.changed && self.layout_input == Some(layout_input) {
            if self.edited_textareas.is_empty() {
                self.apply_scroll_commands(input, false);
                return Ok(());
            }
            if self.update_edited_textareas(input)? {
                return Ok(());
            }
        }
        let media = viewport_media(&viewport, &input.media);
        let style_start = self.measure_timings.then(Instant::now);
        if animated && !self.changed && self.layout_input == Some(layout_input) {
            self.cascade.sample_animations(&mut self.document);
        } else {
            self.cascade.set_preferences(&input.media);
            self.cascade
                .update(&mut self.document, media)
                .map_err(|error| EngineError::Resource(error.to_string()))?;
        }
        self.cascade_dirty = false;
        self.font_loads
            .start_used(&self.document, self.cascade.resources())?;
        let layout_start = self.measure_timings.then(Instant::now);
        let mut snapshot = self
            .layout
            .layout(&self.document, &self.cascade, viewport.css);
        if let (Some(style_start), Some(layout_start)) = (style_start, layout_start) {
            eprintln!(
                "squark-engine frame={} style_us={} layout_us={} fragments={}",
                input.tick.sequence,
                (layout_start - style_start).as_micros(),
                layout_start.elapsed().as_micros(),
                snapshot.nodes.len()
            );
        }
        crate::motion::publish(&mut snapshot, &self.document, &self.cascade, origin);
        self.uploads.prepare(
            snapshot.nodes.values(),
            &mut self.layout,
            self.cascade.resources(),
            viewport.scale,
        )?;
        self.boxes = query::BoxIndex::build(&snapshot);
        self.snapshot = snapshot;
        self.layout_input = Some(layout_input);
        self.changed = false;
        self.edited_textareas.clear();
        self.apply_scroll_commands(input, true);
        Ok(())
    }

    fn snapshot(&self) -> (&FragmentTree, &dyn MirrorRead) {
        (&self.snapshot, &self.document)
    }

    fn resize_entries(&mut self, deeper_than: u32) -> ResizeDelivery {
        let geometry = Geometry {
            document: &self.document,
            tree: &self.snapshot,
            index: &self.boxes,
            margins: &self.layout.margins,
        };
        let scale = self
            .layout_input
            .map_or(1.0, |(viewport, _)| viewport.scale);
        self.observers.resize_entries(&geometry, scale, deeper_than)
    }

    fn intersection_entries(&mut self, now: Instant) -> Vec<IntersectionEntry> {
        let geometry = Geometry {
            document: &self.document,
            tree: &self.snapshot,
            index: &self.boxes,
            margins: &self.layout.margins,
        };
        self.observers
            .intersection_entries(&geometry, self.time_origin.unwrap_or(now), now)
    }

    fn observe_resize(&mut self, observer: u32, target: NodeId, kind: ResizeBox) {
        self.observers.observe_resize(observer, target, kind);
    }

    fn unobserve_resize(&mut self, observer: u32, target: NodeId) {
        self.observers
            .resize
            .retain(|r| r.observer != observer || r.target != target);
    }

    fn disconnect_resize(&mut self, observer: u32) {
        self.observers.resize.retain(|r| r.observer != observer);
    }

    fn observe_intersection(
        &mut self,
        observer: u32,
        target: NodeId,
        options: IntersectionOptions,
    ) {
        self.observers
            .observe_intersection(observer, target, options);
    }

    fn unobserve_intersection(&mut self, observer: u32, target: NodeId) {
        self.observers
            .intersection
            .retain(|r| r.observer != observer || r.target != target);
    }

    fn disconnect_intersection(&mut self, observer: u32) {
        self.observers
            .intersection
            .retain(|r| r.observer != observer);
    }

    fn query(
        &mut self,
        input: &FrameInput<'_>,
        query: Query<'_>,
    ) -> Result<QueryValue, EngineError> {
        let node = match query {
            Query::BoundingRect(id)
            | Query::ClientRects(id)
            | Query::BoxMetrics(id)
            | Query::SvgBBox(id)
            | Query::SvgComputedTextLength(id)
            | Query::FocusScroll(id)
            | Query::CaretRect(id, _)
            | Query::OffsetFromPoint(id, _) => id,
            Query::ComputedStyle { node, .. } => node,
            Query::Selector { scope, .. } => scope,
            // The document-thread bridge owns FramePainter, and handles this query
            // against the fresh snapshot without reversing the engine-to-paint seam.
            Query::ElementFromPoint(_) => {
                return Err(EngineError::Unsupported("hit test without a frame painter"));
            }
            Query::SheetRules(id) | Query::SheetLoaded(id) => id,
        };
        if !self.document.contains(node) {
            return Err(EngineError::MissingNode(node));
        }
        if let Query::SheetRules(id) = query {
            self.restyle_before_read()?;
            return Ok(QueryValue::Strings(
                self.cascade.sheet_rules(&self.document, id),
            ));
        }
        if let Query::SheetLoaded(id) = query {
            self.restyle_before_read()?;
            return Ok(QueryValue::Bool(self.cascade.has_sheet(id)));
        }
        if let Query::Selector { selector, mode, .. } = query {
            self.restyle_before_read()?;
            return self.document.select(
                node,
                selector,
                mode,
                &self.cascade.resources().document_url.clone().into(),
            );
        }
        self.update_layout(input)?;
        Ok(match query {
            Query::BoundingRect(_) | Query::ClientRects(_) => {
                let svg = (self.geometry().boxes(node).next().is_none()
                    && crate::svg::element_name(&self.document, node).is_some())
                .then(|| self.svg_bounds(node))
                .flatten();
                let geometry = self.geometry();
                let rects: Vec<_> = if let Some((_, rect, root)) = svg {
                    geometry
                        .boxes(root)
                        .next()
                        .map(|f| geometry.client_rect(f, rect))
                        .into_iter()
                        .collect()
                } else if let Some(&(table, rect)) = self.layout.table_parts.get(&node) {
                    vec![geometry.client_rect(&self.snapshot.nodes[&table], rect)]
                } else {
                    geometry
                        .boxes(node)
                        .map(|f| geometry.client_rect(f, f.border_box))
                        .collect()
                };
                match query {
                    Query::BoundingRect(_) => QueryValue::Rect(query::bounding(rects.into_iter())),
                    _ => QueryValue::Rects(rects),
                }
            }
            Query::BoxMetrics(_) => QueryValue::BoxMetrics(self.geometry().box_metrics(node)),
            Query::ComputedStyle {
                pseudo, property, ..
            } => QueryValue::String(query::computed_style(
                &self.document,
                &self.cascade,
                &self.geometry(),
                node,
                pseudo,
                property,
            )),
            Query::SvgBBox(_) => QueryValue::Rect(
                self.svg_bounds(node)
                    .map_or(Rect::default(), |(local, _, _)| local),
            ),
            Query::SvgComputedTextLength(_) => QueryValue::Number(
                crate::svg_text::measure(&mut self.layout.fonts, &self.document, node).length,
            ),
            Query::FocusScroll(_) => QueryValue::Void,
            Query::CaretRect(_, offset) => {
                let field = self
                    .layout
                    .editing
                    .get(&node)
                    .ok_or(EngineError::Unsupported("caret geometry for this node"))?;
                QueryValue::Rect(self.field_client_rect(field, field.caret(offset)))
            }
            Query::OffsetFromPoint(_, point) => {
                let field = self
                    .layout
                    .editing
                    .get(&node)
                    .ok_or(EngineError::Unsupported("text position for this node"))?;
                let fragment = &self.snapshot.nodes[&field.fragment];
                let point = self
                    .geometry()
                    .document_point(fragment, point)
                    .ok_or(EngineError::Unsupported("non-invertible text geometry"))?;
                let scroll = fragment
                    .scroll
                    .map_or(Point::default(), |frame| frame.offset);
                QueryValue::Number(f64::from(field.offset_at(Point {
                    x: point.x + scroll.x,
                    y: point.y + scroll.y,
                })))
            }
            Query::ElementFromPoint(_)
            | Query::Selector { .. }
            | Query::SheetRules(_)
            | Query::SheetLoaded(_) => {
                unreachable!()
            }
        })
    }

    fn css_supports(&self, condition: &[u16]) -> bool {
        StaticDocument::css_supports(self, condition)
    }

    fn take_image_events(&mut self) -> Vec<ImageEvent> {
        StaticDocument::take_image_events(self)
    }

    fn parse_rule(&self, text: &[u16]) -> Option<Vec<u16>> {
        self.cascade.parse_rule(&self.document, text)
    }

    fn has_running_animations(&self) -> bool {
        self.document.has_running_animations()
    }

    fn take_animation_events(&mut self) -> Vec<AnimationEvent> {
        self.document.take_animation_events()
    }

    fn set_media_state(&mut self, viewport: &Viewport, media: &MediaState) {
        let device_changed = self
            .cascade
            .set_media(&self.document, viewport_media(viewport, media));
        let preferences_changed = self.cascade.set_preferences(media);
        if device_changed || preferences_changed {
            self.changed = true;
            self.cascade_dirty = true;
        }
    }

    fn evaluate_media(&self, query: &[u16]) -> (Vec<u16>, bool) {
        self.cascade.evaluate_media(query)
    }

    fn parse_fragment(
        &self,
        context: NodeId,
        html: &[u16],
    ) -> Result<Vec<ParsedNode>, EngineError> {
        crate::html_fragment::parse(&self.document, context, html, self.measure_timings)
    }

    fn parse_html_document(&self, html: &[u16]) -> Result<ParsedDocument, EngineError> {
        crate::html_fragment::parse_document(html, self.measure_timings)
    }

    fn select_detached(
        &self,
        root: NodeId,
        quirks: squark_types::pack::QuirksMode,
        is_html: bool,
        batches: &[DecodedBatch<'_>],
        scope: NodeId,
        selector: &str,
        mode: SelectMode,
    ) -> Result<QueryValue, EngineError> {
        let quirks = match quirks {
            squark_types::pack::QuirksMode::Standards => style::context::QuirksMode::NoQuirks,
            squark_types::pack::QuirksMode::LimitedQuirks => {
                style::context::QuirksMode::LimitedQuirks
            }
            squark_types::pack::QuirksMode::Quirks => style::context::QuirksMode::Quirks,
        };
        let to_engine_error = |error: squark_mirror::ReplayError| EngineError::InvalidBatch {
            record: None,
            reason: error.to_string(),
        };
        let mut document =
            Document::new_with_mode(root, quirks, is_html).map_err(to_engine_error)?;
        for &batch in batches {
            document.apply_decoded(batch).map_err(to_engine_error)?;
        }
        document.refresh_selectors();
        document.select(
            scope,
            selector,
            mode,
            &self.cascade.resources().document_url.clone().into(),
        )
    }

    fn measure_text(&mut self, request: MeasureText<'_>) -> Result<TextMetrics, EngineError> {
        self.restyle_before_read()?;
        self.collect_fonts();
        self.layout.sync_fonts(&self.document, &self.cascade);
        let font = self
            .cascade
            .canvas_font(&self.document, request.font_environment, request.font_css)
            .or_else(|| {
                self.cascade.canvas_font(
                    &self.document,
                    request.font_environment,
                    "10px sans-serif",
                )
            })
            .expect("default Canvas font parses");
        self.font_loads
            .start_matching(&font, request.text, self.cascade.resources())?;
        Ok(self.layout.fonts.measure_canvas(request, &font))
    }

    fn parse_color(&self, canvas: NodeId, text: &[u16]) -> Option<[u8; 4]> {
        StaticDocument::parse_color(self, canvas, text)
    }

    fn parse_canvas_font(&self, canvas: NodeId, css: &[u16]) -> Option<Vec<u16>> {
        StaticDocument::parse_canvas_font(self, canvas, css)
    }

    fn validate_filter(&self, text: &[u16]) -> bool {
        StaticDocument::validate_filter(self, text)
    }

    fn font_load(&mut self, face: FaceId) -> Result<(), EngineError> {
        self.font_loads.start(face, self.cascade.resources())
    }

    fn take_font_load_starts(&mut self) -> Vec<FaceId> {
        self.font_loads.drain_starts()
    }

    fn take_font_completions(&mut self) -> Vec<FontCompletion> {
        StaticDocument::take_font_completions(self)
    }

    fn match_font_faces(&self, font: &[u16], text: &[u16]) -> Result<Vec<FaceId>, EngineError> {
        let font = String::from_utf16_lossy(font);
        let font = self
            .cascade
            .canvas_font(&self.document, None, &font)
            .ok_or_else(|| EngineError::Resource("invalid CSS font shorthand".into()))?;
        Ok(self.font_loads.matches(&font, text))
    }
}

fn viewport_media(viewport: &Viewport, state: &MediaState) -> MediaData {
    let mut media = screen(
        viewport.css.width,
        viewport.css.height,
        viewport.scale as f32,
    );
    media.prefers_color_scheme = match state.color_scheme {
        ColorScheme::Light => PrefersColorScheme::Light,
        ColorScheme::Dark => PrefersColorScheme::Dark,
    };
    media.primary_pointer_capabilities = match state.primary_pointer {
        PointerMedia::None => PointerCapabilities::empty(),
        PointerMedia::Coarse => PointerCapabilities::COARSE,
        PointerMedia::Fine => PointerCapabilities::FINE,
    };
    media
        .primary_pointer_capabilities
        .set(PointerCapabilities::HOVER, state.primary_hover);
    media.all_pointer_capabilities = media.primary_pointer_capabilities;
    if state.any_coarse_pointer {
        media
            .all_pointer_capabilities
            .insert(PointerCapabilities::COARSE);
    }
    if state.any_fine_pointer {
        media
            .all_pointer_capabilities
            .insert(PointerCapabilities::FINE);
    }
    if state.any_hover {
        media
            .all_pointer_capabilities
            .insert(PointerCapabilities::HOVER);
    }
    media
}
