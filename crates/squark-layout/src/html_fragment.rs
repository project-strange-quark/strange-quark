// SPDX-License-Identifier: MIT

use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
};

use html5ever::{
    Attribute, LocalName, Namespace, ParseOpts, QualName,
    tendril::{StrTendril, TendrilSink},
    tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink, create_element},
};
use squark_mirror::Document;
use squark_types::{
    NodeId,
    engine::{EngineError, ParsedDocument, ParsedName, ParsedNode},
    fragment::MirrorRead,
};

pub(crate) fn parse(
    document: &Document,
    context: NodeId,
    html: &[u16],
    measure_timings: bool,
) -> Result<Vec<ParsedNode>, EngineError> {
    let start = measure_timings.then(std::time::Instant::now);
    let context_node = document
        .node(context)
        .ok_or(EngineError::MissingNode(context))?;
    let name = context_node
        .name
        .ok_or(EngineError::Unsupported("non-element HTML parsing context"))?;
    let valid_input = String::from_utf16(html).ok();
    let valid_attrs = context_node
        .attrs
        .iter()
        .all(|attr| char::decode_utf16(attr.value.iter().copied()).all(|ch| ch.is_ok()));
    let mut utf16 = if valid_input.is_some() && valid_attrs {
        Utf16::default()
    } else {
        Utf16::new(html, context_node.attrs.iter().map(|attr| attr.value))
    };
    let input = valid_input.unwrap_or_else(|| utf16.encode(html));
    let attrs = context_node
        .attrs
        .into_iter()
        .filter_map(|attr| {
            let raw = String::from_utf16_lossy(document.atom(attr.name)?);
            let (ns, qualified) = raw.split_once('\0').unwrap_or(("", &raw));
            let (prefix, local) = if ns.is_empty() {
                (None, qualified)
            } else {
                qualified
                    .split_once(':')
                    .map_or((None, qualified), |(p, l)| (Some(p.into()), l))
            };
            Some(Attribute {
                name: QualName::new(prefix, ns.into(), local.into()),
                value: utf16.encode(attr.value).into(),
            })
        })
        .collect();
    let atom = |id| String::from_utf16_lossy(document.atom(id).unwrap_or_default());
    let name = QualName::new(
        name.prefix.map(|id| atom(id).into()),
        name.namespace.map(atom).unwrap_or_default().into(),
        atom(name.local).into(),
    );
    let sink = Sink::new();
    let context_element = create_element(&sink, name, attrs);
    let mut ancestor = Some(context);
    let mut form = None;
    while let Some(id) = ancestor.take() {
        if document.html_element(id) == Some("form") {
            form = Some(create_element(
                &sink,
                QualName::new(None, html5ever::ns!(html), html5ever::local_name!("form")),
                vec![],
            ));
            break;
        }
        ancestor = document.parent(id);
    }
    let mut opts = ParseOpts::default();
    opts.tree_builder.quirks_mode = match document.quirks() {
        style::context::QuirksMode::NoQuirks => QuirksMode::NoQuirks,
        style::context::QuirksMode::LimitedQuirks => QuirksMode::LimitedQuirks,
        style::context::QuirksMode::Quirks => QuirksMode::Quirks,
    };
    let prepared = measure_timings.then(std::time::Instant::now);
    let (mut nodes, _) =
        html5ever::driver::parse_fragment_for_element(sink, opts, context_element, true, form)
            .one(input);
    let parsed = measure_timings.then(std::time::Instant::now);
    let root = nodes[0].children[0];
    let node_count = nodes.len();
    let result = export_children(&mut nodes, root, &utf16);
    if let (Some(start), Some(prepared), Some(parsed)) = (start, prepared, parsed) {
        eprintln!(
            "squark-html-fragment setup_us={} html5ever_us={} export_us={} units={} nodes={}",
            (prepared - start).as_micros(),
            (parsed - prepared).as_micros(),
            parsed.elapsed().as_micros(),
            html.len(),
            node_count,
        );
    }
    Ok(result)
}

pub(crate) fn parse_document(
    html: &[u16],
    measure_timings: bool,
) -> Result<ParsedDocument, EngineError> {
    let start = measure_timings.then(std::time::Instant::now);
    let valid_input = String::from_utf16(html).ok();
    let mut utf16 = if valid_input.is_some() {
        Utf16::default()
    } else {
        Utf16::new(html, std::iter::empty())
    };
    let input = valid_input.unwrap_or_else(|| utf16.encode(html));
    let mut opts = ParseOpts::default();
    opts.tree_builder.scripting_enabled = false;
    let prepared = measure_timings.then(std::time::Instant::now);
    let (mut nodes, quirks) = html5ever::parse_document(Sink::new(), opts).one(input);
    let parsed = measure_timings.then(std::time::Instant::now);
    let node_count = nodes.len();
    let result = export_children(&mut nodes, 0, &utf16);
    if let (Some(start), Some(prepared), Some(parsed)) = (start, prepared, parsed) {
        eprintln!(
            "squark-html-document setup_us={} html5ever_us={} export_us={} units={} nodes={}",
            (prepared - start).as_micros(),
            (parsed - prepared).as_micros(),
            parsed.elapsed().as_micros(),
            html.len(),
            node_count,
        );
    }
    let quirks_mode = match quirks {
        QuirksMode::NoQuirks => squark_types::pack::QuirksMode::Standards,
        QuirksMode::LimitedQuirks => squark_types::pack::QuirksMode::LimitedQuirks,
        QuirksMode::Quirks => squark_types::pack::QuirksMode::Quirks,
    };
    Ok(ParsedDocument {
        quirks_mode,
        nodes: result,
    })
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
                Data::Element {
                    name,
                    attrs,
                    template,
                    ..
                } => Some(ParsedNode::Element {
                    name: parsed_name(name),
                    attrs: attrs
                        .into_iter()
                        .map(|a| (parsed_name(a.name), utf16.decode(&a.value)))
                        .collect(),
                    children: export_children(nodes, template.unwrap_or(child), utf16),
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

/// html5ever consumes scalar UTF-8. Reserve private-use scalars absent from
/// the input (including numeric character references) for lone UTF-16 units.
#[derive(Default)]
struct Utf16 {
    reserved: HashSet<char>,
    encoded: HashMap<u16, char>,
    decoded: HashMap<char, u16>,
    next: u32,
}

impl Utf16 {
    fn new<'a>(html: &'a [u16], attrs: impl Iterator<Item = &'a [u16]>) -> Self {
        let mut codec = Self {
            next: 0xf0000,
            ..Self::default()
        };
        for units in std::iter::once(html).chain(attrs) {
            codec.reserve(&String::from_utf16_lossy(units));
        }
        codec
    }

    fn reserve(&mut self, input: &str) {
        let private = |c: &char| ('\u{f0000}'..='\u{10fffd}').contains(c);
        self.reserved.extend(input.chars().filter(private));
        for reference in input.split("&#").skip(1) {
            let (digits, radix) = reference
                .strip_prefix(['x', 'X'])
                .map_or((reference, 10), |s| (s, 16));
            let end = digits
                .bytes()
                .take_while(|b| {
                    if radix == 16 {
                        b.is_ascii_hexdigit()
                    } else {
                        b.is_ascii_digit()
                    }
                })
                .count();
            if let Some(c) = u32::from_str_radix(&digits[..end], radix)
                .ok()
                .and_then(char::from_u32)
                .filter(private)
            {
                self.reserved.insert(c);
            }
        }
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
        template: Option<usize>,
        integration_point: bool,
    },
}

struct Sink {
    nodes: RefCell<Vec<Node>>,
    quirks: Cell<QuirksMode>,
}

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
    fn new() -> Self {
        Self {
            nodes: RefCell::new(vec![Node::new(Data::Root)]),
            quirks: Cell::new(QuirksMode::NoQuirks),
        }
    }

    fn add(&self, data: Data) -> usize {
        let mut nodes = self.nodes.borrow_mut();
        let id = nodes.len();
        nodes.push(Node::new(data));
        id
    }

    fn detach(&self, id: usize) {
        let mut nodes = self.nodes.borrow_mut();
        if let Some(parent) = nodes[id].parent.take() {
            nodes[parent].children.retain(|child| *child != id);
        }
    }

    fn insert(&self, parent: usize, before: Option<usize>, child: NodeOrText<usize>) {
        let mut nodes = self.nodes.borrow_mut();
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
    type Output = (Vec<Node>, QuirksMode);
    type ElemName<'a> = ElementName;

    fn finish(self) -> Self::Output {
        (self.nodes.into_inner(), self.quirks.get())
    }
    fn parse_error(&self, _: Cow<'static, str>) {}
    fn get_document(&self) -> usize {
        0
    }
    fn elem_name<'a>(&'a self, target: &'a usize) -> ElementName {
        let nodes = self.nodes.borrow();
        let Data::Element { name, .. } = &nodes[*target].data else {
            unreachable!()
        };
        ElementName(name.clone())
    }
    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> usize {
        let template = flags.template.then(|| self.add(Data::Root));
        self.add(Data::Element {
            name,
            attrs,
            template,
            integration_point: flags.mathml_annotation_xml_integration_point,
        })
    }
    fn create_comment(&self, text: StrTendril) -> usize {
        self.add(Data::Comment(text))
    }
    fn create_pi(&self, target: StrTendril, data: StrTendril) -> usize {
        self.create_comment(format!("?{target} {data}?").into())
    }
    fn append(&self, parent: &usize, child: NodeOrText<usize>) {
        self.insert(*parent, None, child);
    }
    fn append_based_on_parent_node(&self, element: &usize, prev: &usize, child: NodeOrText<usize>) {
        let parent = self.nodes.borrow()[*element].parent;
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
        let Data::Element {
            template: Some(contents),
            ..
        } = self.nodes.borrow()[*target].data
        else {
            unreachable!()
        };
        contents
    }
    fn same_node(&self, x: &usize, y: &usize) -> bool {
        x == y
    }
    fn set_quirks_mode(&self, mode: QuirksMode) {
        self.quirks.set(mode);
    }
    fn append_before_sibling(&self, sibling: &usize, child: NodeOrText<usize>) {
        let parent = self.nodes.borrow()[*sibling].parent;
        if let Some(parent) = parent {
            self.insert(parent, Some(*sibling), child);
        }
    }
    fn add_attrs_if_missing(&self, target: &usize, attrs: Vec<Attribute>) {
        let mut nodes = self.nodes.borrow_mut();
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
        self.detach(*target);
    }
    fn reparent_children(&self, node: &usize, new_parent: &usize) {
        let children = std::mem::take(&mut self.nodes.borrow_mut()[*node].children);
        for child in children {
            self.append(new_parent, NodeOrText::AppendNode(child));
        }
    }
    fn is_mathml_annotation_xml_integration_point(&self, target: &usize) -> bool {
        matches!(
            self.nodes.borrow()[*target].data,
            Data::Element {
                integration_point: true,
                ..
            }
        )
    }
    fn allow_declarative_shadow_roots(&self, _: &usize) -> bool {
        false
    }
}
