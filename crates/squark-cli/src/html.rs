// SPDX-License-Identifier: MIT
//! Compiles an HTML document with `html5ever` into `document.bin`
//! (the initial-document format in `squark_types::pack`).

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use html5ever::interface::tree_builder::{
    ElemName, ElementFlags, NodeOrText, QuirksMode as HtmlQuirksMode, TreeSink,
};
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{Attribute, LocalName, Namespace, ParseOpts, QualName, parse_document};
use squark_journal::BatchWriter;
use squark_types::NodeId;
use squark_types::journal::{AtomKind, NO_STRING, NodeType, Opcode};
use squark_types::pack::{
    BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DocumentHeader, QuirksMode,
    SCRIPT_ENTRY_SIZE, ScriptEntry, ScriptFlags, ScriptKind, encode_utf16_string_table,
};
use url::Url;

use crate::error::CliError;

const SYNTHETIC_ORIGIN: &str = "http://squark.invalid";
const XHTML_NS: &str = "http://www.w3.org/1999/xhtml";

/// Result of compiling an HTML file to `document.bin`.
#[derive(Clone, Debug)]
pub struct CompiledDocument {
    pub bytes: Vec<u8>,
    pub quirks_mode: QuirksMode,
    pub document_node_id: NodeId,
    pub node_count: u32,
    pub batch_count: u32,
    pub script_count: u32,
}

/// Compiles `html` (located at `entry_pack_path`, e.g. `"/index.html"`) into a
/// version 1 `document.bin` buffer.
pub fn compile_html_to_document_bin(
    html: &str,
    entry_pack_path: &str,
) -> Result<CompiledDocument, CliError> {
    let sink = DomBuilder::new();
    let tree = parse_document(sink, ParseOpts::default()).one(html);
    serialize_dom_tree(&tree, entry_pack_path)
}

#[derive(Debug)]
struct OwnedElemName(QualName);

impl ElemName for OwnedElemName {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }

    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

#[derive(Debug)]
enum DomNodeData {
    Document,
    Doctype {
        name: String,
    },
    Fragment,
    Comment(String),
    Text(String),
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        template_contents: Option<usize>,
        mathml_annotation_xml_integration_point: bool,
    },
}

#[derive(Debug)]
struct DomNode {
    parent: Option<usize>,
    children: Vec<usize>,
    data: DomNodeData,
}

#[derive(Debug)]
struct DomTree {
    quirks_mode: QuirksMode,
    nodes: Vec<DomNode>,
}

struct DomBuilder {
    quirks_mode: Cell<QuirksMode>,
    nodes: RefCell<Vec<DomNode>>,
}

impl DomBuilder {
    fn new() -> Self {
        let root = DomNode {
            parent: None,
            children: Vec::new(),
            data: DomNodeData::Document,
        };
        Self {
            quirks_mode: Cell::new(QuirksMode::Standards),
            nodes: RefCell::new(vec![root]),
        }
    }

    fn push_node(&self, data: DomNodeData) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        let idx = nodes.len();
        nodes.push(DomNode {
            parent: None,
            children: Vec::new(),
            data,
        });
        idx
    }

    fn detach(&self, nodes: &mut [DomNode], child: usize) {
        if let Some(old_parent) = nodes[child].parent.take()
            && let Some(pos) = nodes[old_parent].children.iter().position(|&c| c == child)
        {
            nodes[old_parent].children.remove(pos);
        }
    }
}

impl TreeSink for DomBuilder {
    type Handle = usize;
    type Output = DomTree;
    type ElemName<'a>
        = OwnedElemName
    where
        Self: 'a;

    fn finish(self) -> Self::Output {
        DomTree {
            quirks_mode: self.quirks_mode.get(),
            nodes: self.nodes.into_inner(),
        }
    }

    fn parse_error(&self, _msg: Cow<'static, str>) {}

    fn get_document(&self) -> Self::Handle {
        0
    }

    fn elem_name<'a>(&'a self, target: &'a Self::Handle) -> Self::ElemName<'a> {
        let nodes = self.nodes.borrow();
        match &nodes[*target].data {
            DomNodeData::Element { name, .. } => OwnedElemName(name.clone()),
            other => panic!("elem_name called on non-element node: {other:?}"),
        }
    }

    fn create_element(
        &self,
        name: QualName,
        attrs: Vec<Attribute>,
        flags: ElementFlags,
    ) -> Self::Handle {
        let template_contents = if flags.template {
            Some(self.push_node(DomNodeData::Fragment))
        } else {
            None
        };
        self.push_node(DomNodeData::Element {
            name,
            attrs,
            template_contents,
            mathml_annotation_xml_integration_point: flags.mathml_annotation_xml_integration_point,
        })
    }

    fn create_comment(&self, text: StrTendril) -> Self::Handle {
        self.push_node(DomNodeData::Comment(text.to_string()))
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> Self::Handle {
        self.push_node(DomNodeData::Comment(format!("{target} {data}")))
    }

    fn append(&self, parent: &Self::Handle, child: NodeOrText<Self::Handle>) {
        let mut nodes = self.nodes.borrow_mut();
        match child {
            NodeOrText::AppendNode(child_id) => {
                self.detach(&mut nodes, child_id);
                nodes[child_id].parent = Some(*parent);
                nodes[*parent].children.push(child_id);
            }
            NodeOrText::AppendText(text) => {
                if let Some(&last_child) = nodes[*parent].children.last()
                    && let DomNodeData::Text(existing) = &mut nodes[last_child].data
                {
                    existing.push_str(&text);
                    return;
                }
                let idx = nodes.len();
                nodes.push(DomNode {
                    parent: Some(*parent),
                    children: Vec::new(),
                    data: DomNodeData::Text(text.to_string()),
                });
                nodes[*parent].children.push(idx);
            }
        }
    }

    fn append_based_on_parent_node(
        &self,
        element: &Self::Handle,
        prev_element: &Self::Handle,
        child: NodeOrText<Self::Handle>,
    ) {
        let has_parent = self.nodes.borrow()[*element].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        _public_id: StrTendril,
        _system_id: StrTendril,
    ) {
        let doctype = self.push_node(DomNodeData::Doctype {
            name: name.to_string(),
        });
        let mut nodes = self.nodes.borrow_mut();
        nodes[doctype].parent = Some(0);
        nodes[0].children.push(doctype);
    }

    fn get_template_contents(&self, target: &Self::Handle) -> Self::Handle {
        let nodes = self.nodes.borrow();
        match &nodes[*target].data {
            DomNodeData::Element {
                template_contents: Some(frag),
                ..
            } => *frag,
            other => panic!("get_template_contents called on non-template: {other:?}"),
        }
    }

    fn same_node(&self, x: &Self::Handle, y: &Self::Handle) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: HtmlQuirksMode) {
        let mapped = match mode {
            HtmlQuirksMode::NoQuirks => QuirksMode::Standards,
            HtmlQuirksMode::LimitedQuirks => QuirksMode::LimitedQuirks,
            HtmlQuirksMode::Quirks => QuirksMode::Quirks,
        };
        self.quirks_mode.set(mapped);
    }

    fn append_before_sibling(&self, sibling: &Self::Handle, new_node: NodeOrText<Self::Handle>) {
        let mut nodes = self.nodes.borrow_mut();
        let Some(parent) = nodes[*sibling].parent else {
            return;
        };
        let Some(pos) = nodes[parent].children.iter().position(|&c| c == *sibling) else {
            return;
        };

        match new_node {
            NodeOrText::AppendNode(child_id) => {
                self.detach(&mut nodes, child_id);
                let pos = nodes[parent]
                    .children
                    .iter()
                    .position(|&c| c == *sibling)
                    .expect("sibling vanished during detach");
                nodes[child_id].parent = Some(parent);
                nodes[parent].children.insert(pos, child_id);
            }
            NodeOrText::AppendText(text) => {
                if pos > 0 {
                    let prev_sibling = nodes[parent].children[pos - 1];
                    if let DomNodeData::Text(existing) = &mut nodes[prev_sibling].data {
                        existing.push_str(&text);
                        return;
                    }
                }
                let idx = nodes.len();
                nodes.push(DomNode {
                    parent: Some(parent),
                    children: Vec::new(),
                    data: DomNodeData::Text(text.to_string()),
                });
                nodes[parent].children.insert(pos, idx);
            }
        }
    }

    fn add_attrs_if_missing(&self, target: &Self::Handle, attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
        if let DomNodeData::Element {
            attrs: existing, ..
        } = &mut nodes[*target].data
        {
            for attr in attrs {
                if !existing.iter().any(|a| a.name == attr.name) {
                    existing.push(attr);
                }
            }
        }
    }

    fn remove_from_parent(&self, target: &Self::Handle) {
        let mut nodes = self.nodes.borrow_mut();
        self.detach(&mut nodes, *target);
    }

    fn reparent_children(&self, node: &Self::Handle, new_parent: &Self::Handle) {
        let mut nodes = self.nodes.borrow_mut();
        let moved: Vec<usize> = std::mem::take(&mut nodes[*node].children);
        for child_id in moved {
            if let DomNodeData::Text(moving_text) = &nodes[child_id].data {
                let moving_text = moving_text.clone();
                if let Some(&last) = nodes[*new_parent].children.last()
                    && let DomNodeData::Text(existing) = &mut nodes[last].data
                {
                    existing.push_str(&moving_text);
                    nodes[child_id].parent = None;
                    continue;
                }
            }
            nodes[child_id].parent = Some(*new_parent);
            nodes[*new_parent].children.push(child_id);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &Self::Handle) -> bool {
        let nodes = self.nodes.borrow();
        matches!(
            &nodes[*handle].data,
            DomNodeData::Element {
                mathml_annotation_xml_integration_point: true,
                ..
            }
        )
    }

    fn allow_declarative_shadow_roots(&self, _intended_parent: &Self::Handle) -> bool {
        false
    }
}

struct Serializer<'a> {
    tree: &'a DomTree,
    writer: BatchWriter,
    next_slot: u32,
    script_strings: Vec<Vec<u16>>,
    script_string_map: HashMap<Vec<u16>, u32>,
    scripts: Vec<ScriptEntry>,
    doc_base_url: Url,
    found_base_element: bool,
}

impl<'a> Serializer<'a> {
    fn new(tree: &'a DomTree, entry_pack_path: &str) -> Result<Self, CliError> {
        let base_str = format!("{SYNTHETIC_ORIGIN}{entry_pack_path}");
        let doc_base_url = Url::parse(&base_str).map_err(|err| {
            CliError::PackValidation(format!(
                "failed to construct base URL for {entry_pack_path:?}: {err}"
            ))
        })?;
        Ok(Self {
            tree,
            writer: BatchWriter::new(),
            // Slot 1 is reserved for the Document node (`DocumentHeader::document_node_id`).
            next_slot: 2,
            script_strings: Vec::new(),
            script_string_map: HashMap::new(),
            scripts: Vec::new(),
            doc_base_url,
            found_base_element: false,
        })
    }

    fn alloc_id(&mut self) -> Result<NodeId, CliError> {
        let slot = self.next_slot;
        let id = NodeId::new(slot, 0).ok_or(CliError::SizeOverflow {
            reason: "DOM node count exceeds 24-bit NodeId slot limit",
        })?;
        self.next_slot = slot + 1;
        Ok(id)
    }

    fn intern_atom(&mut self, kind: AtomKind, text: &str) -> Result<u32, CliError> {
        let units: Vec<u16> = text.encode_utf16().collect();
        Ok(self.writer.intern(kind, &units)?)
    }

    fn intern_script_str(&mut self, s: &str) -> Result<u32, CliError> {
        let units: Vec<u16> = s.encode_utf16().collect();
        if let Some(&idx) = self.script_string_map.get(&units) {
            return Ok(idx);
        }
        let idx = u32::try_from(self.script_strings.len()).map_err(|_| CliError::SizeOverflow {
            reason: "script string table exceeds u32::MAX entries",
        })?;
        self.script_string_map.insert(units.clone(), idx);
        self.script_strings.push(units);
        Ok(idx)
    }

    fn resolve_resource_ref(&self, raw_ref: &str) -> (String, String) {
        let trimmed = raw_ref.trim();
        if let Ok(resolved) = self.doc_base_url.join(trimmed) {
            if resolved.origin() == self.doc_base_url.origin()
                && resolved.scheme() == "http"
                && resolved.host_str() == Some("squark.invalid")
            {
                let path = resolved.path().to_owned();
                return (path.clone(), path);
            }
            let full = resolved.to_string();
            return (full.clone(), full);
        }
        let base = self.current_base_display();
        (trimmed.to_owned(), base)
    }

    fn current_base_display(&self) -> String {
        if self.doc_base_url.scheme() == "http"
            && self.doc_base_url.host_str() == Some("squark.invalid")
        {
            self.doc_base_url.path().to_owned()
        } else {
            self.doc_base_url.to_string()
        }
    }

    fn walk_children(&mut self, parent_idx: usize, parent_id: NodeId) -> Result<(), CliError> {
        for &child_idx in &self.tree.nodes[parent_idx].children {
            let child_id = self.emit_node(child_idx, None)?;
            self.writer
                .record(Opcode::Insert, 0, parent_id.raw(), child_id.raw(), 0u32)?;
        }
        Ok(())
    }

    fn emit_node(
        &mut self,
        idx: usize,
        template_owner: Option<NodeId>,
    ) -> Result<NodeId, CliError> {
        let id = self.alloc_id()?;
        let node = &self.tree.nodes[idx];
        match &node.data {
            DomNodeData::Document => {
                return Err(CliError::PackValidation(
                    "nested Document node encountered".to_owned(),
                ));
            }
            DomNodeData::Doctype { name } => {
                self.writer.record(
                    Opcode::CreateNode,
                    0,
                    id.raw(),
                    NodeType::DocumentType as u32,
                    name.as_str(),
                )?;
            }
            DomNodeData::Fragment => {
                // Template content: operand c is the owning
                // `<template>` element's NodeId, or NO_STRING (0xFFFF_FFFF) for an ordinary fragment.
                let owner = template_owner.map_or(NO_STRING, NodeId::raw);
                self.writer.record(
                    Opcode::CreateNode,
                    0,
                    id.raw(),
                    NodeType::DocumentFragment as u32,
                    owner,
                )?;
                self.walk_children(idx, id)?;
            }
            DomNodeData::Comment(text) => {
                self.writer.record(
                    Opcode::CreateNode,
                    0,
                    id.raw(),
                    NodeType::Comment as u32,
                    text.as_str(),
                )?;
            }
            DomNodeData::Text(text) => {
                self.writer
                    .record(Opcode::CreateText, 0, id.raw(), text.as_str(), 0u32)?;
            }
            DomNodeData::Element {
                name,
                attrs,
                template_contents,
                ..
            } => {
                let tag_atom = self.intern_atom(AtomKind::Tag, &name.local)?;
                let ns_str: &str = &name.ns;
                let ns_atom = if ns_str.is_empty() {
                    0
                } else {
                    self.intern_atom(AtomKind::Namespace, ns_str)?
                };
                self.writer
                    .record(Opcode::CreateElement, 0, id.raw(), tag_atom, ns_atom)?;

                for attr in attrs {
                    let attr_ns: &str = &attr.name.ns;
                    let attr_atom = if attr_ns.is_empty() {
                        self.intern_atom(AtomKind::Attribute, &attr.name.local)?
                    } else {
                        let qualified = match attr.name.prefix.as_deref() {
                            Some(p) if !p.is_empty() => {
                                format!("{attr_ns}\0{p}:{}", &*attr.name.local)
                            }
                            _ => format!("{attr_ns}\0{}", &*attr.name.local),
                        };
                        self.intern_atom(AtomKind::QualifiedAttribute, &qualified)?
                    };
                    let attr_val: &str = &attr.value;
                    self.writer
                        .record(Opcode::SetAttr, 0, id.raw(), attr_atom, attr_val)?;
                }

                // Track the first `<base href="...">` in document order.
                if !self.found_base_element
                    && ns_str == XHTML_NS
                    && &*name.local == "base"
                    && let Some(href) = find_attr(attrs, "href")
                    && let Ok(updated) = self.doc_base_url.join(href.trim())
                {
                    self.doc_base_url = updated;
                    self.found_base_element = true;
                }

                self.walk_children(idx, id)?;

                if let Some(frag_idx) = template_contents {
                    let _frag_id = self.emit_node(*frag_idx, Some(id))?;
                }

                if ns_str == XHTML_NS {
                    if &*name.local == "style" {
                        let css = collect_text_children(self.tree, idx);
                        if !css.is_empty() {
                            // Stylesheet ids: the sheet id is the
                            // owning `<style>` element's NodeId.
                            self.writer.record(
                                Opcode::SheetInsert,
                                0,
                                id.raw(),
                                0u32,
                                css.as_str(),
                            )?;
                        }
                    }
                    self.maybe_record_script_or_stylesheet(idx, id, &name.local, attrs)?;
                }
            }
        }
        Ok(id)
    }

    fn maybe_record_script_or_stylesheet(
        &mut self,
        node_idx: usize,
        node_id: NodeId,
        local_name: &str,
        attrs: &[Attribute],
    ) -> Result<(), CliError> {
        if local_name == "link" {
            let is_stylesheet = find_attr(attrs, "rel").is_some_and(|rel| {
                let mut has_stylesheet = false;
                let mut has_alternate = false;
                for token in rel.split_ascii_whitespace() {
                    if token.eq_ignore_ascii_case("stylesheet") {
                        has_stylesheet = true;
                    } else if token.eq_ignore_ascii_case("alternate") {
                        has_alternate = true;
                    }
                }
                has_stylesheet && !has_alternate
            });
            if is_stylesheet
                && let Some(href) = find_attr(attrs, "href")
                && !href.trim().is_empty()
            {
                let (payload, base_url) = self.resolve_resource_ref(href);
                let payload_str = self.intern_script_str(&payload)?;
                let base_url_str = self.intern_script_str(&base_url)?;
                self.scripts.push(ScriptEntry {
                    kind: ScriptKind::ExternalStylesheet,
                    flags: ScriptFlags::NONE,
                    node_id,
                    payload_str,
                    base_url_str,
                });
            }
        } else if local_name == "script" {
            let script_type = find_attr(attrs, "type").map(str::trim);
            let is_module = script_type.is_some_and(|t| t.eq_ignore_ascii_case("module"));
            let is_classic = match script_type {
                None | Some("") => true,
                Some(t) => is_js_mime_type(t),
            };
            if !is_module && !is_classic {
                return Ok(());
            }

            let mut flags = ScriptFlags::NONE;
            if has_attr(attrs, "defer") {
                flags = flags.union(ScriptFlags::DEFER);
            }
            if has_attr(attrs, "async") {
                flags = flags.union(ScriptFlags::ASYNC);
            }

            if let Some(src) = find_attr(attrs, "src") {
                if src.trim().is_empty() {
                    return Ok(());
                }
                let (payload, base_url) = self.resolve_resource_ref(src);
                let payload_str = self.intern_script_str(&payload)?;
                let base_url_str = self.intern_script_str(&base_url)?;
                let kind = if is_module {
                    ScriptKind::ExternalModule
                } else {
                    ScriptKind::ExternalClassic
                };
                self.scripts.push(ScriptEntry {
                    kind,
                    flags,
                    node_id,
                    payload_str,
                    base_url_str,
                });
            } else {
                let inline_src = collect_text_children(self.tree, node_idx);
                let base_url = self.current_base_display();
                let payload_str = self.intern_script_str(&inline_src)?;
                let base_url_str = self.intern_script_str(&base_url)?;
                let kind = if is_module {
                    ScriptKind::InlineModule
                } else {
                    ScriptKind::InlineClassic
                };
                self.scripts.push(ScriptEntry {
                    kind,
                    flags,
                    node_id,
                    payload_str,
                    base_url_str,
                });
            }
        }
        Ok(())
    }
}

fn collect_text_children(tree: &DomTree, node_idx: usize) -> String {
    let mut out = String::new();
    for &child_idx in &tree.nodes[node_idx].children {
        if let DomNodeData::Text(text) = &tree.nodes[child_idx].data {
            out.push_str(text);
        }
    }
    out
}

fn serialize_dom_tree(tree: &DomTree, entry_pack_path: &str) -> Result<CompiledDocument, CliError> {
    let document_node_id = NodeId::new(1, 0).expect("slot 1 is always valid");
    let mut ser = Serializer::new(tree, entry_pack_path)?;
    ser.walk_children(0, document_node_id)?;

    let node_count = ser.next_slot - 1;
    let batches = ser.writer.finish();
    let batch_count = u32::try_from(batches.len()).map_err(|_| CliError::SizeOverflow {
        reason: "batch count exceeds u32::MAX",
    })?;
    let script_count = u32::try_from(ser.scripts.len()).map_err(|_| CliError::SizeOverflow {
        reason: "script count exceeds u32::MAX",
    })?;
    let script_strings_bytes = encode_utf16_string_table(&ser.script_strings)?;

    let batch_dir_offset = DOCUMENT_HEADER_SIZE as u64;
    let batch_dir_len = (batches.len() as u64)
        .checked_mul(BATCH_DIR_ENTRY_SIZE as u64)
        .ok_or(CliError::SizeOverflow {
            reason: "batch directory overflow",
        })?;
    let mut cursor = batch_dir_offset
        .checked_add(batch_dir_len)
        .ok_or(CliError::SizeOverflow {
            reason: "document.bin offset overflow",
        })?;

    let mut dir_entries = Vec::with_capacity(batches.len());
    for batch in &batches {
        cursor = align_up_8(cursor).ok_or(CliError::SizeOverflow {
            reason: "batch alignment overflow",
        })?;
        dir_entries.push(BatchDirEntry {
            offset: cursor,
            len: batch.len() as u64,
        });
        cursor = cursor
            .checked_add(batch.len() as u64)
            .ok_or(CliError::SizeOverflow {
                reason: "batch end offset overflow",
            })?;
    }

    let script_strings_offset = align_up_8(cursor).ok_or(CliError::SizeOverflow {
        reason: "script string table alignment overflow",
    })?;
    let script_strings_len = script_strings_bytes.len() as u64;
    let script_table_offset = align_up_8(
        script_strings_offset
            .checked_add(script_strings_len)
            .ok_or(CliError::SizeOverflow {
                reason: "script string table end overflow",
            })?,
    )
    .ok_or(CliError::SizeOverflow {
        reason: "script table alignment overflow",
    })?;
    let total_len = script_table_offset
        .checked_add((ser.scripts.len() as u64) * (SCRIPT_ENTRY_SIZE as u64))
        .ok_or(CliError::SizeOverflow {
            reason: "document.bin total length overflow",
        })?;

    let header = DocumentHeader {
        quirks_mode: tree.quirks_mode,
        document_node_id,
        batch_count,
        batch_dir_offset,
        script_strings_offset,
        script_strings_len,
        script_count,
        script_table_offset,
    };

    let mut out = Vec::with_capacity(total_len as usize);
    out.extend_from_slice(&header.encode());
    for entry in &dir_entries {
        out.extend_from_slice(&entry.encode());
    }
    for (entry, batch) in dir_entries.iter().zip(batches.iter()) {
        out.resize(entry.offset as usize, 0);
        out.extend_from_slice(batch);
    }
    out.resize(script_strings_offset as usize, 0);
    out.extend_from_slice(&script_strings_bytes);
    out.resize(script_table_offset as usize, 0);
    for script in &ser.scripts {
        out.extend_from_slice(&script.encode());
    }

    Ok(CompiledDocument {
        bytes: out,
        quirks_mode: tree.quirks_mode,
        document_node_id,
        node_count,
        batch_count,
        script_count,
    })
}

fn find_attr<'a>(attrs: &'a [Attribute], local: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|a| a.name.ns.is_empty() && &*a.name.local == local)
        .map(|a| a.value.as_ref())
}

fn has_attr(attrs: &[Attribute], local: &str) -> bool {
    attrs
        .iter()
        .any(|a| a.name.ns.is_empty() && &*a.name.local == local)
}

fn is_js_mime_type(mime: &str) -> bool {
    // WHATWG MIME Sniffing Standard §4.3 JavaScript MIME type essence list.
    let essence = mime.split(';').next().unwrap_or("").trim();
    matches!(
        essence.to_ascii_lowercase().as_str(),
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

fn align_up_8(value: u64) -> Option<u64> {
    value.checked_add(7).map(|v| v & !7)
}
