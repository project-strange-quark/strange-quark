// SPDX-License-Identifier: MIT

//! Synchronous XML parser for `DOMParser` backed by `xml5ever`.

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

use squark_types::engine::{ParsedName, ParsedNode};
use xml5ever::{
    Attribute, LocalName, Namespace, Prefix, QualName, TokenizerResult,
    buffer_queue::BufferQueue,
    interface::ElementFlags,
    local_name, namespace_prefix, ns,
    tendril::StrTendril,
    tokenizer::{ProcessResult, TagKind, Token, TokenSink, XmlTokenizer, XmlTokenizerOpts},
    tree_builder::{ElemName, NodeOrText, TreeSink, XmlTreeBuilder, XmlTreeBuilderOpts},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct XmlParseError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

pub(crate) fn parse_xml(xml: &[u16]) -> Result<Vec<ParsedNode>, XmlParseError> {
    let valid_input = String::from_utf16(xml).ok();
    let mut utf16 = if valid_input.is_some() {
        Utf16::default()
    } else {
        Utf16::new(xml)
    };
    let input = valid_input.unwrap_or_else(|| utf16.encode(xml));

    let (mut nodes, error) = run_parser(&input);
    if let Some(message) = error {
        let (line, column) = locate_error(&input);
        return Err(XmlParseError {
            message,
            line,
            column,
        });
    }
    Ok(export_children(&mut nodes, 0, &utf16))
}

fn run_parser(input: &str) -> (Vec<Node>, Option<String>) {
    let shared = Rc::new(Shared::new());
    let sink = Sink(Rc::clone(&shared));
    let builder = XmlTreeBuilder::new(sink, XmlTreeBuilderOpts::default());
    let guard = Guard {
        inner: builder,
        shared: Rc::clone(&shared),
    };
    let tokenizer = XmlTokenizer::new(guard, XmlTokenizerOpts::default());
    let buffer = BufferQueue::default();
    if !input.is_empty() {
        buffer.push_back(StrTendril::from_slice(input));
        while let TokenizerResult::Script(_) = tokenizer.feed(&buffer) {}
    }
    tokenizer.end();
    if !shared.has_error()
        && let Some((prefix, local)) = shared.open_tags.borrow().last()
    {
        shared.record_error(format!(
            "Premature end of data in tag {}",
            fmt_qname(prefix, local)
        ));
    }
    let error = shared.first_error.borrow_mut().take();
    let nodes = shared.nodes.take();
    (nodes, error)
}

/// Only called on the error path to pinpoint the exact 1-based `(line, column)`.
fn locate_error(input: &str) -> (u32, u32) {
    if input.is_empty() {
        return (1, 1);
    }
    let shared = Rc::new(Shared::new());
    let sink = Sink(Rc::clone(&shared));
    let builder = XmlTreeBuilder::new(sink, XmlTreeBuilderOpts::default());
    let guard = Guard {
        inner: builder,
        shared: Rc::clone(&shared),
    };
    let tokenizer = XmlTokenizer::new(guard, XmlTokenizerOpts::default());
    let buffer = BufferQueue::default();

    let mut line_start = 0usize;
    let mut line_no = 1u32;
    let mut failing_line: Option<(u32, usize, &str)> = None;

    for segment in input.split_inclusive('\n') {
        buffer.push_back(StrTendril::from_slice(segment));
        while let TokenizerResult::Script(_) = tokenizer.feed(&buffer) {}
        if shared.has_error() {
            failing_line = Some((line_no, line_start, segment));
            break;
        }
        line_start += segment.len();
        if segment.ends_with('\n') {
            line_no += 1;
        }
    }

    let Some((target_line, prefix_end, line_text)) = failing_line else {
        // Error happened at EOF (e.g. unclosed tag or empty document).
        let last_line = input.rsplit('\n').next().unwrap_or("");
        return (line_no, last_line.chars().count() as u32 + 1);
    };

    // Re-feed prefix in one chunk, then char-by-char on the failing line.
    let shared2 = Rc::new(Shared::new());
    let sink2 = Sink(Rc::clone(&shared2));
    let builder2 = XmlTreeBuilder::new(sink2, XmlTreeBuilderOpts::default());
    let guard2 = Guard {
        inner: builder2,
        shared: Rc::clone(&shared2),
    };
    let tokenizer2 = XmlTokenizer::new(guard2, XmlTokenizerOpts::default());
    let buffer2 = BufferQueue::default();
    if prefix_end > 0 {
        buffer2.push_back(StrTendril::from_slice(&input[..prefix_end]));
        while let TokenizerResult::Script(_) = tokenizer2.feed(&buffer2) {}
    }
    let mut col = 1u32;
    for ch in line_text.chars() {
        let mut encoded = [0u8; 4];
        buffer2.push_back(StrTendril::from_slice(ch.encode_utf8(&mut encoded)));
        while let TokenizerResult::Script(_) = tokenizer2.feed(&buffer2) {}
        if shared2.has_error() {
            return (target_line, col);
        }
        col += 1;
    }
    (target_line, col)
}

fn fmt_qname(prefix: &Option<Prefix>, local: &LocalName) -> String {
    match prefix {
        Some(prefix) => format!("{prefix}:{local}"),
        None => local.to_string(),
    }
}

struct PendingAttrs {
    xmlns: Vec<Attribute>,
    non_xmlns_count: usize,
}

struct Shared {
    nodes: RefCell<Vec<Node>>,
    first_error: RefCell<Option<String>>,
    open_tags: RefCell<Vec<(Option<Prefix>, LocalName)>>,
    pending_attrs: RefCell<Option<PendingAttrs>>,
    seen_any_content: Cell<bool>,
}

impl Shared {
    fn new() -> Self {
        Self {
            nodes: RefCell::new(vec![Node::new(Data::Root)]),
            first_error: RefCell::new(None),
            open_tags: RefCell::new(Vec::new()),
            pending_attrs: RefCell::new(None),
            seen_any_content: Cell::new(false),
        }
    }

    fn has_error(&self) -> bool {
        self.first_error.borrow().is_some()
    }

    fn record_error(&self, message: impl Into<String>) {
        let mut slot = self.first_error.borrow_mut();
        if slot.is_none() {
            *slot = Some(message.into());
        }
    }
}

struct Guard {
    inner: XmlTreeBuilder<usize, Sink>,
    shared: Rc<Shared>,
}

impl TokenSink for Guard {
    type Handle = usize;

    fn process_token(&self, token: Token) -> ProcessResult<Self::Handle> {
        if self.shared.has_error() {
            return ProcessResult::Done;
        }
        match token {
            Token::ParseError(err) => {
                self.shared.record_error(err.into_owned());
                ProcessResult::Done
            }
            Token::NullCharacter => {
                self.shared.record_error("Invalid NUL character in XML");
                ProcessResult::Done
            }
            Token::ProcessingInstruction(pi) => {
                if pi.target.eq_ignore_ascii_case("xml") {
                    if pi.target.as_ref() != "xml" || self.shared.seen_any_content.get() {
                        self.shared.record_error(
                            "XML declaration allowed only at the start of the document",
                        );
                        return ProcessResult::Done;
                    }
                    self.shared.seen_any_content.set(true);
                    return ProcessResult::Continue;
                }
                self.shared.seen_any_content.set(true);
                self.inner.process_token(Token::ProcessingInstruction(pi))
            }
            Token::Tag(tag) => {
                self.shared.seen_any_content.set(true);
                match tag.kind {
                    TagKind::ShortTag => {
                        self.shared.record_error("Unexpected short end tag '</>'");
                        ProcessResult::Done
                    }
                    TagKind::StartTag | TagKind::EmptyTag => {
                        let mut xmlns = Vec::new();
                        let mut non_xmlns_count = 0usize;
                        for attr in &tag.attrs {
                            if attr.name.prefix == Some(namespace_prefix!("xmlns"))
                                || attr.name.local == local_name!("xmlns")
                            {
                                xmlns.push(Attribute {
                                    name: QualName::new(
                                        attr.name.prefix.clone(),
                                        ns!(xmlns),
                                        attr.name.local.clone(),
                                    ),
                                    value: attr.value.clone(),
                                });
                            } else {
                                non_xmlns_count += 1;
                            }
                        }
                        // xml5ever's tokenizer prepends xmlns attributes with insert(0),
                        // reversing their source order; reverse back to restore order.
                        xmlns.reverse();
                        *self.shared.pending_attrs.borrow_mut() = Some(PendingAttrs {
                            xmlns,
                            non_xmlns_count,
                        });
                        if tag.kind == TagKind::StartTag {
                            self.shared
                                .open_tags
                                .borrow_mut()
                                .push((tag.name.prefix.clone(), tag.name.local.clone()));
                        }
                        self.inner.process_token(Token::Tag(tag))
                    }
                    TagKind::EndTag => {
                        match self.shared.open_tags.borrow_mut().pop() {
                            Some((open_prefix, open_local))
                                if open_prefix == tag.name.prefix
                                    && open_local == tag.name.local => {}
                            Some((open_prefix, open_local)) => {
                                self.shared.record_error(format!(
                                    "Opening and ending tag mismatch: {} and {}",
                                    fmt_qname(&open_prefix, &open_local),
                                    fmt_qname(&tag.name.prefix, &tag.name.local)
                                ));
                                return ProcessResult::Done;
                            }
                            None => {
                                self.shared.record_error(format!(
                                    "Unexpected closing tag </{}>",
                                    fmt_qname(&tag.name.prefix, &tag.name.local)
                                ));
                                return ProcessResult::Done;
                            }
                        }
                        self.inner.process_token(Token::Tag(tag))
                    }
                }
            }
            Token::Characters(_) | Token::Comment(_) | Token::Doctype(_) => {
                self.shared.seen_any_content.set(true);
                self.inner.process_token(token)
            }
            Token::EndOfFile => self.inner.process_token(Token::EndOfFile),
        }
    }

    fn end(&self) {
        self.inner.end();
    }
}

struct Node {
    parent: Option<usize>,
    children: Vec<usize>,
    data: Data,
}

impl Node {
    fn new(data: Data) -> Self {
        Self {
            parent: None,
            children: Vec::new(),
            data,
        }
    }
}

enum Data {
    Root,
    Text(StrTendril),
    Comment(StrTendril),
    Doctype {
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    },
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
    },
}

struct Sink(Rc<Shared>);

#[derive(Debug)]
struct ElementName(QualName);

impl ElemName for ElementName {
    fn ns(&self) -> &Namespace {
        &self.0.ns
    }
    fn local_name(&self) -> &LocalName {
        &self.0.local
    }
}

impl Sink {
    fn add(&self, data: Data) -> usize {
        let mut nodes = self.0.nodes.borrow_mut();
        let id = nodes.len();
        nodes.push(Node::new(data));
        id
    }

    fn insert(&self, parent: usize, before: Option<usize>, child: NodeOrText<usize>) {
        let mut nodes = self.0.nodes.borrow_mut();
        let child = match child {
            NodeOrText::AppendNode(id) => {
                if let Some(old_parent) = nodes[id].parent.take() {
                    nodes[old_parent].children.retain(|child| *child != id);
                }
                id
            }
            NodeOrText::AppendText(text) => {
                let siblings = &nodes[parent].children;
                let previous = match before {
                    Some(sibling) => siblings
                        .iter()
                        .position(|&id| id == sibling)
                        .and_then(|i| i.checked_sub(1))
                        .map(|i| siblings[i]),
                    None => siblings.last().copied(),
                };
                if let Some(previous) = previous
                    && let Data::Text(existing) = &mut nodes[previous].data
                {
                    existing.push_tendril(&text);
                    return;
                }
                let id = nodes.len();
                nodes.push(Node::new(Data::Text(text)));
                id
            }
        };
        nodes[child].parent = Some(parent);
        let children = &mut nodes[parent].children;
        if let Some(at) = before.and_then(|sibling| children.iter().position(|id| *id == sibling)) {
            children.insert(at, child);
        } else {
            children.push(child);
        }
    }
}

impl TreeSink for Sink {
    type Handle = usize;
    type Output = ();
    type ElemName<'a> = ElementName;

    fn finish(self) {}

    fn parse_error(&self, msg: Cow<'static, str>) {
        self.0.record_error(msg.into_owned());
    }

    fn get_document(&self) -> usize {
        0
    }

    fn elem_name<'a>(&'a self, target: &'a usize) -> ElementName {
        let nodes = self.0.nodes.borrow();
        let Data::Element { name, .. } = &nodes[*target].data else {
            unreachable!()
        };
        ElementName(name.clone())
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, _: ElementFlags) -> usize {
        let merged = if let Some(pending) = self.0.pending_attrs.borrow_mut().take() {
            if attrs.len() < pending.non_xmlns_count {
                self.0.record_error("Duplicate attribute");
            }
            if pending.xmlns.is_empty() {
                attrs
            } else {
                let mut result = Vec::with_capacity(pending.xmlns.len() + attrs.len());
                result.extend(pending.xmlns);
                result.extend(attrs);
                result
            }
        } else {
            attrs
        };
        self.add(Data::Element {
            name,
            attrs: merged,
        })
    }

    fn create_comment(&self, text: StrTendril) -> usize {
        self.add(Data::Comment(text))
    }

    fn create_pi(&self, _: StrTendril, _: StrTendril) -> usize {
        // Processing instructions (other than the stripped XML declaration) are
        // ignored on export until ProcessingInstruction nodes exist in squark-dom.
        self.add(Data::Root)
    }

    fn append(&self, parent: &usize, child: NodeOrText<usize>) {
        self.insert(*parent, None, child);
    }

    fn append_based_on_parent_node(&self, element: &usize, prev: &usize, child: NodeOrText<usize>) {
        let parent = self.0.nodes.borrow()[*element].parent;
        match parent {
            Some(parent) => self.insert(parent, Some(*element), child),
            None => self.append(prev, child),
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let doctype = self.add(Data::Doctype {
            name,
            public_id,
            system_id,
        });
        self.insert(0, None, NodeOrText::AppendNode(doctype));
    }

    fn get_template_contents(&self, target: &usize) -> usize {
        *target
    }

    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, _: xml5ever::interface::QuirksMode) {}

    fn append_before_sibling(&self, sibling: &usize, child: NodeOrText<usize>) {
        let parent = self.0.nodes.borrow()[*sibling].parent;
        if let Some(parent) = parent {
            self.insert(parent, Some(*sibling), child);
        }
    }

    fn add_attrs_if_missing(&self, target: &usize, attrs: Vec<Attribute>) {
        let mut nodes = self.0.nodes.borrow_mut();
        let Data::Element {
            attrs: existing, ..
        } = &mut nodes[*target].data
        else {
            unreachable!()
        };
        for attr in attrs {
            if !existing.iter().any(|a| a.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &usize) {
        let mut nodes = self.0.nodes.borrow_mut();
        if let Some(parent) = nodes[*target].parent.take() {
            nodes[parent].children.retain(|child| *child != *target);
        }
    }

    fn reparent_children(&self, node: &usize, new_parent: &usize) {
        let children = std::mem::take(&mut self.0.nodes.borrow_mut()[*node].children);
        for child in children {
            self.append(new_parent, NodeOrText::AppendNode(child));
        }
    }
}

fn export_children(nodes: &mut [Node], id: usize, utf16: &Utf16) -> Vec<ParsedNode> {
    std::mem::take(&mut nodes[id].children)
        .into_iter()
        .filter_map(
            |child| match std::mem::replace(&mut nodes[child].data, Data::Root) {
                Data::Text(text) => Some(ParsedNode::Text(utf16.decode(&text))),
                Data::Comment(text) => Some(ParsedNode::Comment(utf16.decode(&text))),
                Data::Doctype {
                    name,
                    public_id,
                    system_id,
                } => Some(ParsedNode::Doctype {
                    name: utf16.decode(&name),
                    public_id: utf16.decode(&public_id),
                    system_id: utf16.decode(&system_id),
                }),
                Data::Element { name, attrs } => Some(ParsedNode::Element {
                    name: parsed_name(name),
                    attrs: attrs
                        .into_iter()
                        .map(|a| (parsed_name(a.name), utf16.decode(&a.value)))
                        .collect(),
                    children: export_children(nodes, child, utf16),
                }),
                Data::Root => None,
            },
        )
        .collect()
}

fn parsed_name(name: QualName) -> ParsedName {
    ParsedName {
        namespace: name.ns.to_string(),
        local: name.local.to_string(),
        prefix: name.prefix.map(|p| p.to_string()),
    }
}

/// xml5ever consumes scalar UTF-8. Reserve private-use scalars absent from the
/// input for lone UTF-16 code units.
#[derive(Default)]
struct Utf16 {
    reserved: HashSet<char>,
    encoded: HashMap<u16, char>,
    decoded: HashMap<char, u16>,
    next: u32,
}

impl Utf16 {
    fn new(input: &[u16]) -> Self {
        let mut codec = Self {
            next: 0xf0000,
            ..Self::default()
        };
        let lossy = String::from_utf16_lossy(input);
        let private = |c: &char| ('\u{f0000}'..='\u{10fffd}').contains(c);
        codec.reserved.extend(lossy.chars().filter(private));
        codec
    }

    fn encode(&mut self, units: &[u16]) -> String {
        if self.next == 0 {
            return String::from_utf16(units).expect("validated UTF-16");
        }
        char::decode_utf16(units.iter().copied())
            .map(|c| match c {
                Ok(c) => c,
                Err(e) => {
                    let unit = e.unpaired_surrogate();
                    if let Some(&c) = self.encoded.get(&unit) {
                        return c;
                    }
                    let available = (self.next..=0x10fffd)
                        .filter(|n| n & 0xffff < 0xfffe)
                        .filter_map(char::from_u32)
                        .find(|c| !self.reserved.contains(c));
                    let Some(c) = available else {
                        return char::REPLACEMENT_CHARACTER;
                    };
                    self.next = c as u32 + 1;
                    self.encoded.insert(unit, c);
                    self.decoded.insert(c, unit);
                    c
                }
            })
            .collect()
    }

    fn decode(&self, text: &str) -> Vec<u16> {
        if self.decoded.is_empty() {
            return text.encode_utf16().collect();
        }
        let mut units = Vec::with_capacity(text.len());
        for c in text.chars() {
            match self.decoded.get(&c) {
                Some(&unit) => units.push(unit),
                None => units.extend_from_slice(c.encode_utf16(&mut [0; 2])),
            }
        }
        units
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Vec<ParsedNode>, XmlParseError> {
        let units: Vec<u16> = text.encode_utf16().collect();
        parse_xml(&units)
    }

    #[test]
    fn parses_svg_with_namespaces_and_preserves_xmlns() {
        let nodes = parse(
            r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 100 100"><rect x="1" y="2"/><use xlink:href="#a"/></svg>"##,
        )
        .expect("valid SVG parses");
        let [
            ParsedNode::Element {
                name,
                attrs,
                children,
            },
        ] = &nodes[..]
        else {
            panic!("expected single root svg element: {nodes:?}");
        };
        assert_eq!(name.namespace, "http://www.w3.org/2000/svg");
        assert_eq!(name.local, "svg");
        assert_eq!(name.prefix, None);
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0].0.local, "xmlns");
        assert_eq!(attrs[0].0.namespace, "http://www.w3.org/2000/xmlns/");
        assert_eq!(
            String::from_utf16_lossy(&attrs[0].1),
            "http://www.w3.org/2000/svg"
        );
        assert_eq!(attrs[1].0.prefix.as_deref(), Some("xmlns"));
        assert_eq!(attrs[1].0.local, "xlink");
        assert_eq!(attrs[2].0.local, "viewBox");
        assert_eq!(children.len(), 2);
        let ParsedNode::Element {
            name: rect_name, ..
        } = &children[0]
        else {
            panic!("expected rect");
        };
        assert_eq!(rect_name.namespace, "http://www.w3.org/2000/svg");
        assert_eq!(rect_name.local, "rect");
        let ParsedNode::Element {
            attrs: use_attrs, ..
        } = &children[1]
        else {
            panic!("expected use");
        };
        assert_eq!(use_attrs[0].0.namespace, "http://www.w3.org/1999/xlink");
        assert_eq!(use_attrs[0].0.prefix.as_deref(), Some("xlink"));
        assert_eq!(use_attrs[0].0.local, "href");
    }

    #[test]
    fn reports_errors_with_line_and_column() {
        let mismatched = parse("<svg>\n  <rect></svg>").unwrap_err();
        assert_eq!(mismatched.line, 2);
        assert!(mismatched.column >= 9);

        let unclosed = parse("<svg><rect/>").unwrap_err();
        assert_eq!(unclosed.line, 1);

        let empty = parse("").unwrap_err();
        assert_eq!(empty.line, 1);
        assert_eq!(empty.column, 1);

        let duplicate = parse(r#"<a x="1" x="2"/>"#).unwrap_err();
        assert_eq!(duplicate.line, 1);

        let extra = parse("<a/><b/>").unwrap_err();
        assert_eq!(extra.line, 1);

        let short = parse("<a></></a>").unwrap_err();
        assert_eq!(short.line, 1);
    }
}
