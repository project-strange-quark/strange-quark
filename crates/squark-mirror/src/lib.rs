// SPDX-License-Identifier: MIT
//! Journal-fed document arena. The document thread is its only writer.

use std::borrow::Cow;
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use markup5ever::{LocalName, Namespace, ns};
use squark_types::NodeId;
use style::context::QuirksMode;
use style::data::ElementDataWrapper;
use style::properties::PropertyDeclarationBlock;
use style::servo_arc::Arc;
use style::shared_lock::{Locked, SharedRwLock};
use style::stylist::CascadeData;

mod forms;
mod replay;
pub use forms::{InputKind, TextEdit};
mod motion;
#[allow(unsafe_code)]
mod stylo;

pub use replay::{FlushReport, ReplayError};

#[derive(Debug)]
pub struct Document {
    root: NodeId,
    quirks: QuirksMode,
    is_html: bool,
    slots: BTreeMap<u32, Slot<Box<Node>>>,
    atoms: BTreeMap<u32, Atom>,
    sheets: BTreeMap<u32, Vec<Box<[u16]>>>,
    window_listeners: u8,
    focused: Option<NodeId>,
    scroll_commands: Vec<ScrollCommand>,
    image_sources: Vec<ImageSource>,
    style_lock: SharedRwLock,
    snapshots: BTreeMap<NodeId, ElementSnapshot>,
    structural_edits: BTreeMap<NodeId, StructuralEdit>,
    relocated: BTreeSet<NodeId>,
    selector_dirty: BTreeSet<NodeId>,
    restyled_elements: usize,
    motion: motion::Motion,
    /// The cascade data of each shadow root's stylesheets, which squark-style
    /// builds and Stylo reads for the elements of that shadow tree.
    shadow_styles: BTreeMap<NodeId, Arc<CascadeData>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollCommand {
    To(NodeId, squark_types::Point),
    Focus(NodeId),
}

#[derive(Debug, PartialEq, Eq)]
pub struct ImageSource {
    pub target: NodeId,
    pub generation: u32,
    pub src: Option<String>,
}

#[derive(Debug)]
struct ElementSnapshot {
    element: Element,
    inherited_disabled: bool,
}

#[derive(Debug)]
struct StructuralEdit {
    membership_changed: bool,
    following: BTreeSet<NodeId>,
    old_edges: [Option<NodeId>; 2],
    was_empty: bool,
}

#[derive(Debug)]
enum Slot<T> {
    Live(T),
    Vacant(u8),
    Retired,
}

#[derive(Debug)]
struct Node {
    dom: DomNode,
    style: ElementDataWrapper,
    selector_id: Option<style::Atom>,
    selector_classes: Vec<style::Atom>,
    inline_style: Option<Arc<Locked<PropertyDeclarationBlock>>>,
    svg_paint_hints: Option<Arc<Locked<PropertyDeclarationBlock>>>,
    has_data: Cell<bool>,
    dirty_descendants: Cell<bool>,
    handled_snapshot: Cell<bool>,
    selector_flags: Cell<usize>,
}

impl std::ops::Deref for Node {
    type Target = DomNode;
    fn deref(&self) -> &DomNode {
        &self.dom
    }
}

#[derive(Clone, Debug)]
struct DomNode {
    id: NodeId,
    kind: NodeKind,
    parent: Option<NodeId>,
    prev: Option<NodeId>,
    next: Option<NodeId>,
    children: Vec<NodeId>,
    template_content: Option<NodeId>,
    /// The shadow root this element hosts.
    shadow_root: Option<NodeId>,
    /// The slot this slottable was last assigned to. Readers check it with
    /// [`Document::assigned_slot`], since the slot's list is the source.
    assigned_slot: Option<NodeId>,
    /// A slot's assigned nodes, or a document's or shadow root's adopted
    /// stylesheets, in order.
    list: Vec<NodeId>,
    listeners: u8,
    dirty: u8,
    descendants: u8,
}

#[derive(Clone, Debug)]
enum NodeKind {
    Document,
    Doctype(Box<[u16]>),
    Fragment {
        template: Option<NodeId>,
    },
    ShadowRoot {
        host: NodeId,
    },
    /// A constructed stylesheet and its media list, if it has one.
    Sheet {
        media: Option<Box<[u16]>>,
    },
    Comment(Box<[u16]>),
    Element(Box<Element>),
    Text(Box<[u16]>),
}

#[derive(Clone, Debug)]
struct Element {
    name: LocalName,
    namespace: Namespace,
    tag_atom: u32,
    namespace_atom: u32,
    attrs: Vec<Attribute>,
    inline: Vec<InlineDeclaration>,
    state: u32,
    state_mask: u32,
    value: Option<Box<[u16]>>,
    text_edit: Option<Box<TextEdit>>,
    image_generation: u32,
    blob_image_unavailable: bool,
    canvas_size: Option<[u32; 2]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InlineDeclaration {
    name: LocalName,
    value: Box<[u16]>,
    important: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Attribute {
    atom: u32,
    namespace: Namespace,
    name: LocalName,
    value: Box<[u16]>,
    scalar: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Atom {
    kind: u32,
    value: Box<[u16]>,
}

const ALL_DIRTY: u8 = 0xf;

impl DomNode {
    fn new(id: NodeId, kind: NodeKind) -> Self {
        Self {
            id,
            kind,
            parent: None,
            prev: None,
            next: None,
            children: Vec::new(),
            template_content: None,
            shadow_root: None,
            assigned_slot: None,
            list: Vec::new(),
            listeners: 0,
            dirty: ALL_DIRTY,
            descendants: 0,
        }
    }

    fn element(&self) -> Option<&Element> {
        match &self.kind {
            NodeKind::Element(element) => Some(element),
            _ => None,
        }
    }
}

impl Node {
    fn new(dom: DomNode) -> Box<Self> {
        Box::new(Self {
            dom,
            style: Default::default(),
            inline_style: None,
            svg_paint_hints: None,
            selector_id: None,
            selector_classes: Vec::new(),
            has_data: Cell::default(),
            dirty_descendants: Cell::default(),
            handled_snapshot: Cell::default(),
            selector_flags: Cell::default(),
        })
    }
}

impl Element {
    fn attr(&self, namespace: &Namespace, name: &LocalName) -> Option<&Attribute> {
        self.attrs
            .iter()
            .find(|attr| attr.namespace == *namespace && attr.name == *name)
    }
}

impl Document {
    pub fn new(root: NodeId, quirks: QuirksMode) -> Result<Self, ReplayError> {
        Self::new_with_mode(root, quirks, true)
    }

    pub fn new_with_mode(
        root: NodeId,
        quirks: QuirksMode,
        is_html: bool,
    ) -> Result<Self, ReplayError> {
        if root.slot() == 0 {
            return Err(ReplayError::InvalidNode(root));
        }
        Ok(Self {
            root,
            quirks,
            is_html,
            slots: BTreeMap::from([(
                root.slot(),
                Slot::Live(Node::new(DomNode::new(root, NodeKind::Document))),
            )]),
            atoms: BTreeMap::new(),
            sheets: BTreeMap::new(),
            window_listeners: 0,
            focused: None,
            scroll_commands: Vec::new(),
            image_sources: Vec::new(),
            style_lock: SharedRwLock::new(),
            snapshots: BTreeMap::new(),
            structural_edits: BTreeMap::new(),
            relocated: BTreeSet::new(),
            selector_dirty: BTreeSet::new(),
            restyled_elements: 0,
            motion: Default::default(),
            shadow_styles: BTreeMap::new(),
        })
    }

    fn live_nodes_mut(&mut self) -> impl Iterator<Item = &mut Node> {
        self.slots.values_mut().filter_map(|slot| match slot {
            Slot::Live(node) => Some(node.as_mut()),
            _ => None,
        })
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        match self.slots.get(&id.slot())? {
            Slot::Live(node) if node.id == id => Some(node),
            _ => None,
        }
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn quirks(&self) -> QuirksMode {
        self.quirks
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id)?.parent
    }

    pub fn children(&self, id: NodeId) -> Option<&[NodeId]> {
        Some(&self.node(id)?.children)
    }

    pub fn text(&self, id: NodeId) -> Option<&[u16]> {
        match &self.node(id)?.kind {
            NodeKind::Text(text) | NodeKind::Comment(text) | NodeKind::Doctype(text) => Some(text),
            _ => None,
        }
    }

    pub fn template_content(&self, id: NodeId) -> Option<NodeId> {
        self.node(id)?.template_content
    }

    /// The shadow root an element hosts.
    pub fn shadow_root(&self, id: NodeId) -> Option<NodeId> {
        self.node(id)?
            .shadow_root
            .filter(|&root| self.contains(root))
    }

    /// The host of a shadow root.
    pub fn shadow_host(&self, root: NodeId) -> Option<NodeId> {
        match self.node(root)?.kind {
            NodeKind::ShadowRoot { host } => Some(host).filter(|&host| self.contains(host)),
            _ => None,
        }
    }

    /// The shadow root at the root of the tree that contains this node.
    pub fn containing_shadow_root(&self, id: NodeId) -> Option<NodeId> {
        let root = std::iter::successors(Some(id), |&id| self.parent(id)).last()?;
        matches!(self.node(root)?.kind, NodeKind::ShadowRoot { .. }).then_some(root)
    }

    /// The parent, or the host of a shadow root: the shadow-including parent.
    pub fn composed_parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).or_else(|| self.shadow_host(id))
    }

    /// Whether the node's shadow-including root is this document.
    pub fn is_connected(&self, id: NodeId) -> bool {
        std::iter::successors(Some(id), |&id| self.composed_parent(id)).any(|id| id == self.root)
    }

    /// Whether the node is an HTML `slot` element.
    pub fn is_slot(&self, id: NodeId) -> bool {
        self.html_element(id) == Some("slot")
    }

    /// The slot a slottable is assigned to: a slot in its parent's shadow
    /// tree whose assigned nodes list it.
    pub fn assigned_slot(&self, id: NodeId) -> Option<NodeId> {
        let node = self.node(id)?;
        let slot = node.assigned_slot?;
        let host = self.shadow_host(self.containing_shadow_root(slot)?)?;
        (node.parent == Some(host) && self.node(slot)?.list.contains(&id)).then_some(slot)
    }

    /// A slot's assigned nodes that are still its host's children.
    pub fn assigned_nodes(&self, slot: NodeId) -> Cow<'_, [NodeId]> {
        let Some(node) = self.node(slot).filter(|_| self.is_slot(slot)) else {
            return Cow::Borrowed(&[]);
        };
        if node
            .list
            .iter()
            .all(|&id| self.assigned_slot(id) == Some(slot))
        {
            Cow::Borrowed(&node.list)
        } else {
            Cow::Owned(
                node.list
                    .iter()
                    .copied()
                    .filter(|&id| self.assigned_slot(id) == Some(slot))
                    .collect(),
            )
        }
    }

    /// Children in the flat tree: a host's shadow root's children, a slot's
    /// assigned nodes or else its own children, and otherwise the children.
    pub fn flat_children(&self, id: NodeId) -> Cow<'_, [NodeId]> {
        if let Some(root) = self.shadow_root(id) {
            return Cow::Borrowed(self.children(root).unwrap_or_default());
        }
        if self.is_slot(id) {
            let assigned = self.assigned_nodes(id);
            if !assigned.is_empty() {
                return assigned;
            }
        }
        Cow::Borrowed(self.children(id).unwrap_or_default())
    }

    /// The parent in the flat tree: a slotted node's slot, a shadow root
    /// child's host, and otherwise the parent.
    pub fn flat_parent(&self, id: NodeId) -> Option<NodeId> {
        if let Some(slot) = self.assigned_slot(id) {
            return Some(slot);
        }
        let parent = self.parent(id)?;
        self.shadow_host(parent).or(Some(parent))
    }

    /// The parent in an event path: a slotted node's slot, a shadow root's
    /// host, and otherwise the parent.
    pub fn event_parent(&self, id: NodeId) -> Option<NodeId> {
        self.assigned_slot(id)
            .or_else(|| self.parent(id))
            .or_else(|| self.shadow_host(id))
    }

    /// A document's or shadow root's adopted constructed stylesheets.
    pub fn adopted_sheets(&self, scope: NodeId) -> &[NodeId] {
        match self.node(scope) {
            Some(node) if matches!(node.kind, NodeKind::Document | NodeKind::ShadowRoot { .. }) => {
                &node.list
            }
            _ => &[],
        }
    }

    /// A constructed stylesheet's media list, or `None` for another node.
    pub fn sheet_media(&self, sheet: NodeId) -> Option<&[u16]> {
        match &self.node(sheet)?.kind {
            NodeKind::Sheet { media } => Some(media.as_deref().unwrap_or_default()),
            _ => None,
        }
    }

    /// The cascade data built from a shadow root's stylesheets.
    pub fn shadow_styles(&self, root: NodeId) -> Option<&Arc<CascadeData>> {
        self.shadow_styles.get(&root)
    }

    /// Replaces every shadow root's cascade data. squark-style restyles the
    /// document afterwards.
    pub fn set_shadow_styles(&mut self, styles: BTreeMap<NodeId, Arc<CascadeData>>) {
        self.shadow_styles = styles;
    }

    pub fn is_text(&self, id: NodeId) -> bool {
        self.node(id)
            .is_some_and(|node| matches!(node.kind, NodeKind::Text(_)))
    }

    pub fn html_element(&self, id: NodeId) -> Option<&str> {
        let element = self.node(id)?.element()?;
        (element.namespace == ns!(html)).then_some(&element.name)
    }

    pub fn is_svg_root(&self, id: NodeId) -> bool {
        self.node(id)
            .and_then(|node| node.element())
            .is_some_and(|element| {
                element.namespace == ns!(svg) && element.name == markup5ever::local_name!("svg")
            })
    }

    /// Canvas bitmap dimensions from the resize record or initial HTML.
    pub fn canvas_bitmap_size(&self, id: NodeId) -> Option<[u32; 2]> {
        if self.html_element(id) != Some("canvas") {
            return None;
        }
        let element = self.node(id)?.element()?;
        Some(element.canvas_size.unwrap_or_else(|| {
            let dimension = |name, fallback| {
                let digits = element
                    .attr(&ns!(), &name)
                    .map(|attr| {
                        attr.scalar
                            .trim_start_matches([' ', '\t', '\n', '\r', '\x0c'])
                    })
                    .unwrap_or_default();
                let digits = digits.strip_prefix('+').unwrap_or(digits);
                let end = digits.bytes().take_while(u8::is_ascii_digit).count();
                digits[..end].parse::<i32>().map_or(fallback, |n| n as u32)
            };
            [
                dimension(markup5ever::local_name!("width"), 300),
                dimension(markup5ever::local_name!("height"), 150),
            ]
        }))
    }

    pub fn attribute(&self, id: NodeId, name: &str) -> Option<&[u16]> {
        let attr = self
            .node(id)?
            .element()?
            .attr(&ns!(), &LocalName::from(name))?;
        Some(&attr.value)
    }

    pub fn sheet_rules(&self, id: u32) -> Option<&[Box<[u16]>]> {
        self.sheets.get(&id).map(Vec::as_slice)
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.node(id).is_some()
    }

    pub fn restyled_elements(&self) -> usize {
        self.restyled_elements
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focused
    }

    pub fn take_scroll_commands(&mut self) -> Vec<ScrollCommand> {
        std::mem::take(&mut self.scroll_commands)
    }

    pub fn take_image_sources(&mut self) -> Vec<ImageSource> {
        std::mem::take(&mut self.image_sources)
    }

    pub fn image_generation(&self, target: NodeId) -> Option<u32> {
        self.node(target)?
            .element()
            .map(|element| element.image_generation)
    }

    pub fn image_src(&self, target: NodeId) -> Option<&str> {
        let element = self.node(target)?.element()?;
        if element.namespace != ns!(html)
            || element.name != markup5ever::local_name!("img")
            || element.blob_image_unavailable
        {
            return None;
        }
        Some(
            &element
                .attr(&ns!(), &markup5ever::local_name!("src"))?
                .scalar,
        )
    }

    pub fn reject_blob_image(&mut self, target: NodeId, generation: u32) {
        if let Some(Slot::Live(node)) = self.slots.get_mut(&target.slot())
            && node.id == target
            && let NodeKind::Element(element) = &mut node.dom.kind
            && element.image_generation == generation
        {
            element.blob_image_unavailable = true;
        }
    }

    /// The lock shared by Stylo's document and its stylesheets.
    pub fn style_lock(&self) -> &SharedRwLock {
        &self.style_lock
    }
}

impl squark_types::fragment::MirrorRead for Document {
    fn document(&self) -> NodeId {
        self.root()
    }

    fn node(&self, id: NodeId) -> Option<squark_types::fragment::NodeRead<'_>> {
        use squark_types::fragment::{
            AtomId, AttrRef, Dirty, Links, Name, NodeKind as ReadKind, NodeRead,
        };
        use squark_types::journal::{ElementState, ListenerFlags};

        let node = self.node(id)?;
        let kind = match node.kind {
            NodeKind::Document => ReadKind::Document,
            NodeKind::Doctype(_) => ReadKind::Doctype,
            NodeKind::Fragment { .. } | NodeKind::ShadowRoot { .. } | NodeKind::Sheet { .. } => {
                ReadKind::Fragment
            }
            NodeKind::Comment(_) => ReadKind::Comment,
            NodeKind::Element(_) => ReadKind::Element,
            NodeKind::Text(_) => ReadKind::Text,
        };
        let links = Links {
            parent: self.event_parent(id),
            first: node.children.first().copied(),
            last: node.children.last().copied(),
            prev: node.prev,
            next: node.next,
        };
        let (name, attrs, state) = if let Some(element) = node.element() {
            let name = Name {
                namespace: (element.namespace_atom != 0).then_some(AtomId(element.namespace_atom)),
                local: AtomId(element.tag_atom),
                prefix: None,
            };
            let attrs = element
                .attrs
                .iter()
                .map(|attr| AttrRef {
                    name: AtomId(attr.atom),
                    value: &attr.value,
                })
                .collect();
            (
                Some(name),
                attrs,
                ElementState::from_bits(element.state).expect("validated state"),
            )
        } else {
            (None, Vec::new(), ElementState::default())
        };
        Some(NodeRead {
            kind,
            links,
            name,
            attrs,
            state,
            text: self.text(id),
            own: Dirty(node.dirty),
            descendants: Dirty(node.descendants),
            listeners: ListenerFlags::from_bits(u32::from(node.listeners))
                .expect("validated listeners"),
        })
    }

    fn atom(&self, id: squark_types::fragment::AtomId) -> Option<&[u16]> {
        Some(&self.atoms.get(&id.0)?.value)
    }

    fn window_listeners(&self) -> squark_types::journal::ListenerFlags {
        squark_types::journal::ListenerFlags::from_bits(u32::from(self.window_listeners))
            .expect("validated window listeners")
    }
}
