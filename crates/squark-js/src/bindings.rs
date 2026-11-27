// SPDX-License-Identifier: MIT

//! Synchronous document-thread calls from the squark-dom prelude into the engine.

use std::{cell::RefCell, ptr::NonNull};

use squark_journal::BatchDecoder;
use squark_types::{
    NodeId, Point, Rect, ScrollOffset,
    engine::{
        self, Direction, DocumentEngine, EngineError, FrameInput, MeasureText, MediaState,
        ParsedName, ParsedNode, Pseudo, Query, QueryValue, SelectMode,
    },
    journal::Opcode,
    render::{FrameId, FramePainter, FrameTick},
};
use v8::{Local, PinScope, Value};

use crate::{DocumentShutdown, canvas::Staging, document::DocumentError, scroll::ScrollState};

pub(crate) struct Bridge<'engine> {
    pub shutdown: DocumentShutdown,
    pub engine: Box<dyn DocumentEngine + 'engine>,
    pub painter: Box<dyn FramePainter + 'engine>,
    pub decoder: BatchDecoder,
    pub input: FrameState,
    pub dirty: bool,
    pub staging: Staging,
    pub scroll_dirty: bool,
    pub scroll_delivery: Vec<ScrollOffset>,
}

pub(crate) struct FrameState {
    pub tick: FrameTick,
    pub media: MediaState,
    pub scroll: ScrollState,
}

impl FrameState {
    fn as_input(&self) -> FrameInput<'_> {
        FrameInput {
            tick: self.tick,
            media: self.media,
            scroll: self.scroll.as_input(),
        }
    }
}

impl Bridge<'_> {
    pub fn query(&mut self, query: Query<'_>) -> Result<QueryValue, DocumentError> {
        if let Query::ElementFromPoint(point) = query {
            self.layout()?;
            let (fragments, mirror) = self.engine.snapshot();
            let hit = self.painter.hit_test(fragments, mirror, point);
            self.shutdown.check()?;
            return Ok(QueryValue::Node(hit));
        }
        self.update_scroll()?;
        self.shutdown
            .engine(self.engine.query(&self.input.as_input(), query))
    }

    pub fn layout(&mut self) -> Result<(), DocumentError> {
        self.shutdown.check()?;
        self.shutdown
            .engine(self.engine.update_layout(&self.input.as_input()))?;
        if self.scroll_dirty {
            self.scroll_dirty = false;
            let (tree, _) = self.engine.snapshot();
            let samples = tree
                .nodes
                .values()
                .filter_map(|fragment| fragment.scroll)
                .map(|frame| ScrollOffset {
                    owner: frame.owner,
                    offset: frame.offset,
                    generation: frame.generation,
                })
                .collect::<Vec<_>>();
            self.scroll_delivery
                .extend(self.input.scroll.update(FrameId(0), 0, &samples));
        }
        Ok(())
    }

    pub fn update_scroll(&mut self) -> Result<(), DocumentError> {
        self.shutdown.check()?;
        if self.scroll_dirty {
            self.layout()?;
        }
        Ok(())
    }

    fn flush(&mut self, bytes: &[u8]) -> Result<u32, String> {
        self.shutdown.check().map_err(|error| error.to_string())?;
        let batch = self
            .decoder
            .validate(bytes)
            .map_err(|error| error.to_string())?;
        let records: Vec<_> = batch.records().collect();
        let scroll = records
            .iter()
            .any(|record| matches!(record.op, Opcode::ScrollTo | Opcode::Focus));
        let table = batch.strings();
        let strings: Vec<_> = (0..table.len())
            .map(|index| native_units(table.get(index as u32).unwrap()))
            .collect();
        let units: Vec<_> = strings.iter().map(AsRef::as_ref).collect();
        let staged = self.staging.resolve(&records, &units)?;
        let count = self
            .shutdown
            .engine(apply_batch(&mut *self.engine, &records, &units, &staged))
            .map_err(|error| error.to_string())?;
        let used = staged.iter().map(|(id, _)| *id).collect();
        batch.commit();
        self.staging.consume(used);
        self.dirty |= count != 0;
        self.scroll_dirty |= scroll;
        Ok(count)
    }
}

fn native_units(text: squark_journal::Utf16<'_>) -> std::borrow::Cow<'_, [u16]> {
    #[cfg(target_endian = "little")]
    {
        // SAFETY: Every two-byte pattern is a valid u16. align_to checks
        // alignment and preserves the lifetime of the immutable batch.
        let (prefix, units, suffix) = unsafe { text.le_units().align_to::<u16>() };
        if prefix.is_empty() && suffix.is_empty() {
            return std::borrow::Cow::Borrowed(units);
        }
    }
    std::borrow::Cow::Owned(text.units().collect())
}

struct BridgePointer(NonNull<()>);

/// # Safety
/// The cell must stay at the same address and outlive the isolate.
pub(crate) unsafe fn register(scope: &mut PinScope<'_, '_>, bridge: &RefCell<Bridge<'_>>) {
    scope.set_slot(BridgePointer(NonNull::from(bridge).cast()));
}

pub(crate) fn with_bridge<R>(
    scope: &PinScope<'_, '_>,
    f: impl for<'engine> FnOnce(&mut Bridge<'engine>) -> R,
) -> R {
    let address = scope
        .get_slot::<BridgePointer>()
        .expect("document bindings installed")
        .0;
    // SAFETY: DocumentRunner registers its boxed RefCell once, before startup.
    // The box stays at the same address through all callbacks and outlives the
    // isolate. The borrow is checked by RefCell and cannot escape the closure.
    let bridge = unsafe { address.cast::<RefCell<Bridge<'_>>>().as_ref() };
    f(&mut bridge.borrow_mut())
}

fn apply_batch(
    engine: &mut dyn DocumentEngine,
    records: &[squark_types::journal::Record],
    strings: &[&[u16]],
    staged_bytes: &[(u32, &[u8])],
) -> Result<u32, EngineError> {
    let report = engine.flush(engine::DecodedBatch {
        records,
        strings,
        staged_bytes,
    })?;
    if report.applied as usize != records.len() {
        return Err(EngineError::InvalidBatch {
            record: None,
            reason: format!(
                "engine acknowledged {} of {} records",
                report.applied,
                records.len()
            ),
        });
    }
    Ok(report.applied)
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, v8::Object>) -> Option<()> {
    for (name, value) in [
        ("os", std::env::consts::OS),
        ("arch", std::env::consts::ARCH),
    ] {
        let value = v8::String::new(scope, value)?;
        set(scope, host, name, value.into())?;
    }
    for (name, callback) in [
        ("flush", v8::Function::new(scope, flush)),
        ("query", v8::Function::new(scope, query)),
        ("scrollOffset", v8::Function::new(scope, scroll_offset)),
        ("queryNumber", v8::Function::new(scope, query_number)),
        ("parseFragment", v8::Function::new(scope, parse_fragment)),
        ("parseHtml", v8::Function::new(scope, parse_html)),
        ("parseXml", v8::Function::new(scope, parse_xml)),
        ("selectDetached", v8::Function::new(scope, select_detached)),
        ("measureText", v8::Function::new(scope, measure_text)),
        ("logError", v8::Function::new(scope, log_error)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn log_error(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    _: v8::ReturnValue<'_>,
) {
    if let Some(message) = string(scope, args.get(0)) {
        eprintln!("JavaScript error: {message}");
    }
}

fn flush(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let value = (|| {
        let buffer = Local::<v8::ArrayBuffer>::try_from(args.get(0)).ok()?;
        let length = unsigned(args.get(1))? as usize;
        let store = buffer.get_backing_store();
        if buffer.was_detached() || store.is_shared() || length > store.byte_length() {
            return None;
        }
        let data = store.data()?;
        // SAFETY: The ordinary ArrayBuffer cannot be shared with another isolate.
        // Its backing store is held for this borrow, length is bounded by it, and
        // validation and engine.flush do not execute JS or retain the slice.
        let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), length) };
        Some(with_bridge(scope, |bridge| bridge.flush(bytes)))
    })();
    match value {
        Some(Ok(count)) => result.set_uint32(count),
        Some(Err(error)) => throw(scope, &error, false),
        None => throw(scope, "invalid journal buffer or length", false),
    }
}

fn query(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let Some(request) = prepare_query(scope, &args) else {
        throw(scope, "invalid query arguments", false);
        return;
    };
    let response = with_bridge(scope, |bridge| bridge.query(request.borrow()));
    match response {
        Ok(response) => match query_value(scope, response) {
            Some(value) => result.set(value),
            None => throw(scope, "could not build query result", false),
        },
        Err(error) => throw_document(scope, error),
    }
}

fn scroll_offset(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let Some(owner) = unsigned(args.get(0)).map(NodeId::from_raw) else {
        throw(scope, "invalid scroll owner", false);
        return;
    };
    let offset = with_bridge(scope, |bridge| {
        bridge.update_scroll()?;
        Ok::<_, DocumentError>(
            bridge
                .input
                .scroll
                .offset(owner)
                .map_or_else(Point::default, |entry| entry.offset),
        )
    });
    match offset {
        Ok(offset) => {
            if let Some(value) =
                numbers(scope, [("left", offset.x.into()), ("top", offset.y.into())])
            {
                result.set(value.into());
            }
        }
        Err(error) => throw_document(scope, error),
    }
}

// Stores the decoded arguments independently of the V8 scope so layout can
// run after all reads of JS properties have finished.
enum Request {
    Plain(Query<'static>),
    Selector(NodeId, String, SelectMode),
    Style(NodeId, String, Option<Pseudo>),
}

impl Request {
    fn borrow(&self) -> Query<'_> {
        match self {
            Self::Plain(query) => *query,
            Self::Selector(scope, selector, mode) => Query::Selector {
                scope: *scope,
                selector,
                mode: *mode,
            },
            Self::Style(node, property, pseudo) => Query::ComputedStyle {
                node: *node,
                property,
                pseudo: *pseudo,
            },
        }
    }
}

fn prepare_query(
    scope: &mut PinScope<'_, '_>,
    args: &v8::FunctionCallbackArguments,
) -> Option<Request> {
    let kind = unsigned(args.get(0))?;
    let id = NodeId::from_raw(unsigned(args.get(1))?);
    let arg = args.get(2);
    let plain = match kind {
        1 => Query::BoundingRect(id),
        2 => Query::ClientRects(id),
        3 => Query::BoxMetrics(id),
        4 => {
            let arg = Local::<v8::Object>::try_from(arg).ok()?;
            let property = get(scope, arg, "property")?;
            let property = string(scope, property)?;
            let pseudo = get(scope, arg, "pseudo")?;
            let pseudo = if pseudo.is_null_or_undefined() {
                None
            } else {
                let name = string(scope, pseudo)?;
                Some(match name.trim_start_matches(':') {
                    "before" => Pseudo::Before,
                    "after" => Pseudo::After,
                    "marker" => Pseudo::Marker,
                    "placeholder" => Pseudo::Placeholder,
                    "first-letter" => Pseudo::FirstLetter,
                    "first-line" => Pseudo::FirstLine,
                    _ => return None,
                })
            };
            return Some(Request::Style(id, property, pseudo));
        }
        5 => {
            let arg = Local::<v8::Object>::try_from(arg).ok()?;
            let selector = get(scope, arg, "selector")?;
            let selector = string(scope, selector)?;
            let mode = get(scope, arg, "mode")?;
            let mode = match string(scope, mode)?.as_str() {
                "first" => SelectMode::First,
                "all" => SelectMode::All,
                "match" => SelectMode::Match,
                "closest" => SelectMode::Closest,
                _ => return None,
            };
            return Some(Request::Selector(id, selector, mode));
        }
        6 | 12 => {
            let arg = Local::<v8::Object>::try_from(arg).ok()?;
            let x = Local::<v8::Number>::try_from(get(scope, arg, "x")?)
                .ok()?
                .value() as f32;
            let y = Local::<v8::Number>::try_from(get(scope, arg, "y")?)
                .ok()?
                .value() as f32;
            let point = Point { x, y };
            if kind == 6 {
                Query::ElementFromPoint(point)
            } else if x.is_finite() && y.is_finite() {
                Query::OffsetFromPoint(id, point)
            } else {
                return None;
            }
        }
        7 => Query::SvgBBox(id),
        8 => Query::SvgComputedTextLength(id),
        9 => Query::FocusScroll(id),
        10 => Query::SheetRules(id),
        11 => Query::CaretRect(id, unsigned(arg)?),
        _ => return None,
    };
    Some(Request::Plain(plain))
}

fn query_number(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let (Some(3), Some(id), Some(field @ 1..=10)) = (
        unsigned(args.get(0)),
        unsigned(args.get(1)),
        unsigned(args.get(2)),
    ) else {
        throw(scope, "invalid numeric query arguments", false);
        return;
    };
    match with_bridge(scope, |bridge| {
        bridge.query(Query::BoxMetrics(NodeId::from_raw(id)))
    }) {
        Ok(QueryValue::BoxMetrics(metrics)) => result.set_int32(metrics.number(field).unwrap()),
        Ok(_) => throw(scope, "expected box metrics", false),
        Err(error) => throw_document(scope, error),
    }
}

const METRICS: [&str; 10] = [
    "offsetTop",
    "offsetLeft",
    "offsetWidth",
    "offsetHeight",
    "clientTop",
    "clientLeft",
    "clientWidth",
    "clientHeight",
    "scrollWidth",
    "scrollHeight",
];

fn query_value<'s>(scope: &mut PinScope<'s, '_>, response: QueryValue) -> Option<Local<'s, Value>> {
    Some(match response {
        QueryValue::Rect(value) => rect(scope, value)?.into(),
        QueryValue::Rects(values) => array(scope, values, |scope, value| {
            Some(rect(scope, value)?.into())
        })?
        .into(),
        QueryValue::String(value) => v8::String::new(scope, &value)?.into(),
        QueryValue::Strings(values) => array(scope, values, |scope, value| {
            Some(v8::String::new(scope, &value)?.into())
        })?
        .into(),
        QueryValue::Bool(value) => v8::Boolean::new(scope, value).into(),
        QueryValue::Node(value) => node_value(scope, value),
        QueryValue::Nodes(values) => array(scope, values, |scope, node| {
            Some(node_value(scope, Some(node)))
        })?
        .into(),
        QueryValue::Number(value) => v8::Number::new(scope, value).into(),
        QueryValue::Caret(Some((node, offset))) => {
            let node = v8::Integer::new_from_unsigned(scope, node.raw());
            let offset = v8::Integer::new_from_unsigned(scope, offset);
            v8::Array::new_with_elements(scope, &[node.into(), offset.into()]).into()
        }
        QueryValue::Caret(None) => v8::null(scope).into(),
        QueryValue::Void => v8::undefined(scope).into(),
        QueryValue::BoxMetrics(metrics) => {
            let object = v8::Object::new(scope);
            for (index, name) in METRICS.into_iter().enumerate() {
                let value = v8::Integer::new(scope, metrics.number(index as u32 + 1)?);
                set(scope, object, name, value.into())?;
            }
            let parent = node_value(scope, metrics.offset_parent);
            set(scope, object, "offsetParent", parent)?;
            object.into()
        }
    })
}

fn node_value<'s>(scope: &mut PinScope<'s, '_>, id: Option<NodeId>) -> Local<'s, Value> {
    match id {
        Some(id) => v8::Integer::new_from_unsigned(scope, id.raw()).into(),
        None => v8::null(scope).into(),
    }
}

pub(crate) fn rect<'s>(scope: &mut PinScope<'s, '_>, rect: Rect) -> Option<Local<'s, v8::Object>> {
    numbers(
        scope,
        [
            ("x", rect.origin.x.into()),
            ("y", rect.origin.y.into()),
            ("width", rect.size.width.into()),
            ("height", rect.size.height.into()),
        ],
    )
}

fn parse_fragment(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let (Some(html), Some(id)) = (utf16(scope, args.get(0)), unsigned(args.get(1))) else {
        throw(scope, "invalid fragment arguments", false);
        return;
    };
    let parsed = with_bridge(scope, |bridge| {
        bridge.engine.parse_fragment(NodeId::from_raw(id), &html)
    });
    match parsed {
        Ok(nodes) => match array(scope, nodes, parsed_node) {
            Some(nodes) => result.set(nodes.into()),
            None => throw(scope, "could not build fragment result", false),
        },
        Err(error) => throw_engine(scope, error),
    }
}

fn parse_html(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let Some(html) = utf16(scope, args.get(0)) else {
        throw(scope, "invalid HTML document argument", false);
        return;
    };
    let parsed = with_bridge(scope, |bridge| bridge.engine.parse_html_document(&html));
    match parsed {
        Ok(document) => {
            let object = v8::Object::new(scope);
            let quirks = v8::Integer::new_from_unsigned(scope, document.quirks_mode.to_flags());
            let Some(nodes) = array(scope, document.nodes, parsed_node) else {
                throw(scope, "could not build HTML document result", false);
                return;
            };
            if set(scope, object, "quirksMode", quirks.into()).is_none()
                || set(scope, object, "nodes", nodes.into()).is_none()
            {
                throw(scope, "could not build HTML document result", false);
                return;
            }
            result.set(object.into());
        }
        Err(error) => throw_engine(scope, error),
    }
}

fn parse_xml(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let Some(xml) = utf16(scope, args.get(0)) else {
        throw(scope, "invalid XML document argument", false);
        return;
    };
    match crate::xml::parse_xml(&xml) {
        Ok(nodes) => match array(scope, nodes, parsed_node) {
            Some(nodes) => result.set(nodes.into()),
            None => throw(scope, "could not build XML document result", false),
        },
        Err(err) => {
            let object = v8::Object::new(scope);
            let Some(message) = v8::String::new(scope, &err.message) else {
                throw(scope, "could not build XML error result", false);
                return;
            };
            let line = v8::Integer::new_from_unsigned(scope, err.line);
            let column = v8::Integer::new_from_unsigned(scope, err.column);
            if set(scope, object, "error", message.into()).is_none()
                || set(scope, object, "line", line.into()).is_none()
                || set(scope, object, "column", column.into()).is_none()
            {
                throw(scope, "could not build XML error result", false);
                return;
            }
            result.set(object.into());
        }
    }
}

fn select_detached(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let parsed = (|| {
        let batches_array = Local::<v8::Array>::try_from(args.get(0)).ok()?;
        let root = NodeId::from_raw(unsigned(args.get(1))?);
        let quirks = squark_types::pack::QuirksMode::from_flags(unsigned(args.get(2))?).ok()?;
        let is_html = args.get(3).boolean_value(scope);
        let scope_id = NodeId::from_raw(unsigned(args.get(4))?);
        let selector = string(scope, args.get(5))?;
        let mode = match string(scope, args.get(6))?.as_str() {
            "first" => SelectMode::First,
            "all" => SelectMode::All,
            "match" => SelectMode::Match,
            "closest" => SelectMode::Closest,
            _ => return None,
        };
        let mut owned_batches = Vec::with_capacity(batches_array.length() as usize);
        for index in 0..batches_array.length() {
            let entry = batches_array.get_index(scope, index)?;
            let buffer = Local::<v8::ArrayBuffer>::try_from(entry).ok()?;
            let store = buffer.get_backing_store();
            if buffer.was_detached() || store.is_shared() {
                return None;
            }
            let len = store.byte_length();
            let bytes = if len == 0 {
                Vec::new()
            } else {
                let data = store.data()?;
                // SAFETY: The ArrayBuffer is unshared and live for this copy.
                unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), len) }.to_vec()
            };
            owned_batches.push(bytes);
        }
        Some((
            owned_batches,
            root,
            quirks,
            is_html,
            scope_id,
            selector,
            mode,
        ))
    })();
    let Some((owned_batches, root, quirks, is_html, scope_id, selector, mode)) = parsed else {
        throw(scope, "invalid detached selector arguments", false);
        return;
    };
    let mut decoder = match squark_journal::BatchDecoder::new(root) {
        Ok(decoder) => decoder,
        Err(error) => {
            throw(scope, &error.to_string(), false);
            return;
        }
    };
    struct OwnedBatch {
        records: Vec<squark_types::journal::Record>,
        strings: Vec<Vec<u16>>,
    }
    let mut decoded_owned = Vec::with_capacity(owned_batches.len());
    for bytes in &owned_batches {
        let batch = match decoder.validate(bytes) {
            Ok(batch) => batch,
            Err(error) => {
                throw(scope, &error.to_string(), false);
                return;
            }
        };
        let records = batch.records().collect();
        let table = batch.strings();
        let strings = (0..table.len())
            .map(|i| table.get(i as u32).unwrap().units().collect())
            .collect();
        batch.commit();
        decoded_owned.push(OwnedBatch { records, strings });
    }
    let string_slices: Vec<Vec<&[u16]>> = decoded_owned
        .iter()
        .map(|batch| batch.strings.iter().map(Vec::as_slice).collect())
        .collect();
    let engine_batches: Vec<engine::DecodedBatch<'_>> = decoded_owned
        .iter()
        .zip(&string_slices)
        .map(|(batch, strings)| engine::DecodedBatch {
            records: &batch.records,
            strings,
            staged_bytes: &[],
        })
        .collect();
    let response = with_bridge(scope, |bridge| {
        bridge.engine.select_detached(
            root,
            quirks,
            is_html,
            &engine_batches,
            scope_id,
            &selector,
            mode,
        )
    });
    match response {
        Ok(response) => match query_value(scope, response) {
            Some(value) => result.set(value),
            None => throw(scope, "could not build selector result", false),
        },
        Err(error) => throw_engine(scope, error),
    }
}

fn parsed_node<'s>(scope: &mut PinScope<'s, '_>, node: ParsedNode) -> Option<Local<'s, Value>> {
    let object = v8::Object::new(scope);
    let kind = match node {
        ParsedNode::Element {
            name,
            attrs,
            children,
        } => {
            parsed_name(scope, object, &name)?;
            let attributes = array(scope, attrs, |scope, (name, value)| {
                let attribute = v8::Object::new(scope);
                parsed_name(scope, attribute, &name)?;
                let value = two_byte(scope, &value)?;
                set(scope, attribute, "value", value.into())?;
                Some(attribute.into())
            })?;
            set(scope, object, "attrs", attributes.into())?;
            let children = array(scope, children, parsed_node)?;
            set(scope, object, "children", children.into())?;
            "element"
        }
        ParsedNode::Text(ref value) | ParsedNode::Comment(ref value) => {
            let value = two_byte(scope, value)?;
            set(scope, object, "value", value.into())?;
            if matches!(node, ParsedNode::Text(_)) {
                "text"
            } else {
                "comment"
            }
        }
        ParsedNode::Doctype {
            ref name,
            ref public_id,
            ref system_id,
        } => {
            let name = two_byte(scope, name)?;
            let public_id = two_byte(scope, public_id)?;
            let system_id = two_byte(scope, system_id)?;
            set(scope, object, "name", name.into())?;
            set(scope, object, "publicId", public_id.into())?;
            set(scope, object, "systemId", system_id.into())?;
            "doctype"
        }
    };
    let kind = v8::String::new(scope, kind)?;
    set(scope, object, "kind", kind.into())?;
    Some(object.into())
}

fn parsed_name(
    scope: &mut PinScope<'_, '_>,
    object: Local<'_, v8::Object>,
    name: &ParsedName,
) -> Option<()> {
    for (key, value) in [
        (
            "namespace",
            Some(name.namespace.as_str()).filter(|value| !value.is_empty()),
        ),
        ("localName", Some(name.local.as_str())),
        ("prefix", name.prefix.as_deref()),
    ] {
        let value = match value {
            Some(value) => v8::String::new(scope, value)?.into(),
            None => v8::null(scope).into(),
        };
        set(scope, object, key, value)?;
    }
    Some(())
}

fn measure_text(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let (Some(font), Some(text)) = (string(scope, args.get(0)), utf16(scope, args.get(1))) else {
        throw(scope, "invalid text measurement arguments", false);
        return;
    };
    let metrics = with_bridge(scope, |bridge| {
        bridge.engine.measure_text(MeasureText {
            font_css: &font,
            text: &text,
            direction: Direction::Ltr,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            font_environment: None,
        })
    });
    match metrics {
        Ok(m) => {
            let object = numbers(
                scope,
                [
                    ("width", m.width),
                    ("actualBoundingBoxLeft", m.actual_left),
                    ("actualBoundingBoxRight", m.actual_right),
                    ("actualBoundingBoxAscent", m.actual_ascent),
                    ("actualBoundingBoxDescent", m.actual_descent),
                    ("fontBoundingBoxAscent", m.font_ascent),
                    ("fontBoundingBoxDescent", m.font_descent),
                    ("emHeightAscent", m.em_ascent),
                    ("emHeightDescent", m.em_descent),
                    ("alphabeticBaseline", m.alphabetic_baseline),
                    ("hangingBaseline", m.hanging_baseline),
                    ("ideographicBaseline", m.ideographic_baseline),
                ],
            );
            if let Some(object) = object {
                result.set(object.into());
            } else {
                throw(scope, "could not build text metrics", false);
            }
        }
        Err(error) => throw_engine(scope, error),
    }
}

pub(crate) fn two_byte<'s>(
    scope: &mut PinScope<'s, '_>,
    value: &[u16],
) -> Option<Local<'s, v8::String>> {
    v8::String::new_from_two_byte(scope, value, v8::NewStringType::Normal)
}

pub(crate) fn utf16(scope: &mut PinScope<'_, '_>, value: Local<'_, Value>) -> Option<Vec<u16>> {
    let value = Local::<v8::String>::try_from(value).ok()?;
    let mut units = vec![0; value.length()];
    value.write_v2(scope, 0, &mut units, v8::WriteFlags::empty());
    Some(units)
}

pub(crate) fn string(scope: &mut PinScope<'_, '_>, value: Local<'_, Value>) -> Option<String> {
    Some(
        Local::<v8::String>::try_from(value)
            .ok()?
            .to_rust_string_lossy(scope),
    )
}

pub(crate) fn unsigned(value: Local<'_, Value>) -> Option<u32> {
    Some(Local::<v8::Uint32>::try_from(value).ok()?.value())
}

fn numbers<'s>(
    scope: &mut PinScope<'s, '_>,
    entries: impl IntoIterator<Item = (&'static str, f64)>,
) -> Option<Local<'s, v8::Object>> {
    let object = v8::Object::new(scope);
    for (key, value) in entries {
        let value = v8::Number::new(scope, value);
        set(scope, object, key, value.into())?;
    }
    Some(object)
}

pub(crate) fn array<'s, T>(
    scope: &mut PinScope<'s, '_>,
    data: Vec<T>,
    mut encode: impl FnMut(&mut PinScope<'s, '_>, T) -> Option<Local<'s, Value>>,
) -> Option<Local<'s, v8::Array>> {
    let values = data
        .into_iter()
        .map(|item| encode(scope, item))
        .collect::<Option<Vec<_>>>()?;
    Some(v8::Array::new_with_elements(scope, &values))
}

pub(crate) fn get<'s>(
    scope: &mut PinScope<'s, '_>,
    object: Local<'_, v8::Object>,
    key: &str,
) -> Option<Local<'s, Value>> {
    let key = v8::String::new(scope, key)?;
    object.get(scope, key.into())
}

pub(crate) fn set(
    scope: &mut PinScope<'_, '_>,
    object: Local<'_, v8::Object>,
    key: &str,
    value: Local<'_, Value>,
) -> Option<()> {
    let key = v8::String::new(scope, key)?;
    (object.create_data_property(scope, key.into(), value) == Some(true)).then_some(())
}

fn throw_engine(scope: &mut PinScope<'_, '_>, error: EngineError) {
    throw(
        scope,
        &error.to_string(),
        error == EngineError::InvalidSelector,
    );
}

fn throw_document(scope: &mut PinScope<'_, '_>, error: DocumentError) {
    match error {
        DocumentError::Engine(error) => throw_engine(scope, error),
        error => throw(scope, &error.to_string(), false),
    }
}

pub(crate) fn throw(scope: &mut PinScope<'_, '_>, error: &str, syntax: bool) {
    if scope.is_execution_terminating() {
        return;
    }
    if let Some(message) = v8::String::new(scope, error) {
        let exception = if syntax {
            v8::Exception::syntax_error(scope, message)
        } else {
            v8::Exception::type_error(scope, message)
        };
        scope.throw_exception(exception);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_utf16_units_preserve_aligned_and_unaligned_strings() {
        #[repr(align(2))]
        struct Aligned([u8; 13]);
        let mut bytes = Aligned([0; 13]);
        for start in [0, 1] {
            let batch = &mut bytes.0[start..start + 12];
            batch.copy_from_slice(&[0, 0, 0, 0, 2, 0, 0, 0, 0x41, 0, 0, 0xd8]);
            let table = squark_journal::StringTable::new(batch, 1, 2).unwrap();
            let units = super::native_units(table.get(0).unwrap());
            assert_eq!(units.as_ref(), &[0x41, 0xd800]);
            assert_eq!(
                matches!(units, std::borrow::Cow::Borrowed(_)),
                cfg!(target_endian = "little") && start == 0,
            );
        }
    }
}
