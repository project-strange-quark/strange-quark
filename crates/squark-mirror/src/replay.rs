// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, BTreeSet, btree_map::Entry};

use markup5ever::{LocalName, Namespace, ns};
use squark_journal::{BatchDecoder, DecodedBatch, JournalError};
use squark_types::journal::{
    FOCUS_BLUR, FOCUS_PREVENT_SCROLL, Opcode, Record, SELECTION_BACKWARD, STYLE_IMPORTANT,
};
use squark_types::{NodeId, Point};

use super::{
    ALL_DIRTY, Atom, Attribute, Document, DomNode, Element, ElementSnapshot, ImageSource,
    InlineDeclaration, Node, NodeKind, ScrollCommand, Slot, StructuralEdit,
};

pub use squark_types::engine::FlushReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayError {
    Journal(JournalError),
    InvalidNode(NodeId),
    SlotGeneration(NodeId),
    MissingAtom(u32),
    InvalidAtom(u32),
    MissingString(u32),
    InvalidInsert,
    InvalidRemove,
    InvalidRelease(NodeId),
    InvalidRecord(Opcode),
    Unsupported(Opcode),
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for ReplayError {}

#[derive(Default)]
struct ChildListChanges {
    membership_changed: bool,
    following: BTreeSet<NodeId>,
}

/// Only changed DOM payloads are copied. Existing Stylo data is preserved
/// across success and never touched by a batch that fails validation.
struct Changes<'a> {
    document: &'a Document,
    slots: BTreeMap<u32, Slot<DomNode>>,
    atoms: BTreeMap<u32, Atom>,
    sheets: BTreeMap<u32, Vec<Box<[u16]>>>,
    listeners: u8,
    focused: Option<NodeId>,
    scroll_commands: Vec<ScrollCommand>,
    image_sources: Vec<ImageSource>,
    released: Vec<NodeId>,
    child_lists: BTreeMap<NodeId, ChildListChanges>,
    relocated: BTreeSet<NodeId>,
}

impl Document {
    pub fn apply_journal(
        &mut self,
        decoder: &mut BatchDecoder,
        bytes: &[u8],
    ) -> Result<FlushReport, ReplayError> {
        self.apply_batch(decoder.validate(bytes).map_err(ReplayError::Journal)?)
    }

    pub fn apply_batch(&mut self, batch: DecodedBatch<'_, '_>) -> Result<FlushReport, ReplayError> {
        let strings = batch.strings();
        let report = self.apply_records(batch.records(), |index| {
            strings.get(index).map(|text| text.units().collect())
        })?;
        batch.commit();
        Ok(report)
    }

    /// Apply the shared engine view. Its caller commits the journal decoder on success.
    pub fn apply_decoded(
        &mut self,
        batch: squark_types::engine::DecodedBatch<'_>,
    ) -> Result<FlushReport, ReplayError> {
        self.apply_records(batch.records.iter().copied(), |index| {
            batch.strings.get(index as usize).map(|&text| text.into())
        })
    }

    fn apply_records(
        &mut self,
        records: impl ExactSizeIterator<Item = Record>,
        string: impl Fn(u32) -> Option<Box<[u16]>>,
    ) -> Result<FlushReport, ReplayError> {
        let mut changes = Changes {
            document: self,
            slots: BTreeMap::new(),
            atoms: BTreeMap::new(),
            sheets: BTreeMap::new(),
            listeners: self.window_listeners,
            focused: self.focused,
            scroll_commands: Vec::new(),
            image_sources: Vec::new(),
            released: Vec::new(),
            child_lists: BTreeMap::new(),
            relocated: BTreeSet::new(),
        };
        let applied = records.len() as u32;
        for record in records {
            changes.apply(record, &string)?;
        }

        let Changes {
            slots,
            atoms,
            sheets,
            listeners,
            focused,
            scroll_commands,
            image_sources,
            released,
            child_lists,
            relocated,
            ..
        } = changes;
        self.atoms.extend(atoms);
        self.sheets.extend(sheets);
        self.window_listeners = listeners;
        self.focused = focused;
        self.scroll_commands.extend(scroll_commands);
        self.image_sources.extend(image_sources);
        for (parent, change) in child_lists {
            // Capture the old edge elements and emptiness once, even when several
            // batches change the child list before the next style read.
            let initial = if !self.structural_edits.contains_key(&parent) {
                let mut was_empty = true;
                let mut old_edges = [None, None];
                for &child in self.children(parent).unwrap_or_default() {
                    let Some(node) = self.node(child) else {
                        continue;
                    };
                    match &node.kind {
                        NodeKind::Element(_) => {
                            old_edges[0].get_or_insert(child);
                            old_edges[1] = Some(child);
                            was_empty = false;
                        }
                        NodeKind::Text(text) if !text.is_empty() => was_empty = false,
                        _ => {}
                    }
                }
                Some(StructuralEdit {
                    membership_changed: false,
                    following: BTreeSet::new(),
                    old_edges,
                    was_empty,
                })
            } else {
                None
            };
            let edit = self
                .structural_edits
                .entry(parent)
                .or_insert_with(|| initial.expect("new structural edit"));
            edit.membership_changed |= change.membership_changed;
            edit.following.extend(change.following);
        }
        self.snapshot_changes(&slots);
        self.relocated.extend(relocated);
        for (slot, entry) in slots {
            let entry = match entry {
                Slot::Live(dom) => {
                    if let Some(Slot::Live(node)) = self.slots.get_mut(&slot)
                        && node.id == dom.id
                    {
                        if let (Some(old), Some(new)) = (node.element(), dom.element())
                            && (old.attrs != new.attrs || old.inline != new.inline)
                        {
                            self.selector_dirty.insert(node.id);
                        }
                        node.dom = dom;
                        continue;
                    }
                    if dom.element().is_some() {
                        self.selector_dirty.insert(dom.id);
                    }
                    Slot::Live(Node::new(dom))
                }
                Slot::Vacant(generation) => Slot::Vacant(generation),
                Slot::Retired => Slot::Retired,
            };
            self.slots.insert(slot, entry);
        }
        for id in &released {
            self.snapshots.remove(id);
            self.selector_dirty.remove(id);
            self.sheets.remove(&id.raw());
        }
        Ok(FlushReport { applied, released })
    }

    fn snapshot_changes(&mut self, slots: &BTreeMap<u32, Slot<DomNode>>) {
        use squark_types::journal::ElementState;
        let staged = |id: NodeId| match slots.get(&id.slot()) {
            Some(Slot::Live(dom)) if dom.id == id => Some(dom),
            None => self.node(id).map(|node| &node.dom),
            _ => None,
        };
        let mut snapshots = BTreeSet::new();
        for entry in slots.values() {
            let Slot::Live(dom) = entry else { continue };
            let id = dom.id;
            let Some(prior) = self.node(id) else { continue };
            let (Some(old), Some(new)) = (prior.element(), dom.element()) else {
                continue;
            };
            if old.form_state() != new.form_state()
                || old.attrs != new.attrs
                || old.inline != new.inline
            {
                snapshots.insert(id);
            }
            let disabled = ElementState::DISABLED.bits();
            let disabled_toggled = (old.form_state() ^ new.form_state()) & disabled != 0;
            let name = self.html_element(id);
            let legend_changed = name == Some("fieldset")
                && new.form_state() & disabled != 0
                && prior.children != dom.children
                && self.first_legend(id)
                    != dom.children.iter().copied().find(|&child| {
                        staged(child)
                            .and_then(DomNode::element)
                            .is_some_and(|element| {
                                element.namespace == ns!(html) && &*element.name == "legend"
                            })
                    });
            if (disabled_toggled && matches!(name, Some("fieldset" | "optgroup"))) || legend_changed
            {
                let mut pending = prior.children.clone();
                while let Some(child) = pending.pop() {
                    let Some(node) = self.node(child) else {
                        continue;
                    };
                    snapshots.insert(child);
                    pending.extend(node.children.iter().copied());
                }
            }
        }
        for id in snapshots {
            if self.snapshots.contains_key(&id) {
                continue;
            }
            if let Some(node) = self.node(id)
                && node.has_data.get()
                && let Some(element) = node.element()
            {
                let snapshot = ElementSnapshot {
                    element: element.clone(),
                    inherited_disabled: self.inherited_disabled(id),
                };
                self.snapshots.insert(id, snapshot);
            }
        }
    }
}

impl Changes<'_> {
    fn apply(
        &mut self,
        record: Record,
        lookup_string: impl Fn(u32) -> Option<Box<[u16]>>,
    ) -> Result<(), ReplayError> {
        let Record { op, flags, a, b, c } = record;
        let id = NodeId::from_raw(a);
        let invalid = ReplayError::InvalidRecord(op);
        let string = |i| lookup_string(i).ok_or(ReplayError::MissingString(i));
        let valid_flags = match op {
            Opcode::SetStyle => flags & !STYLE_IMPORTANT == 0,
            Opcode::SetSelection => flags & !SELECTION_BACKWARD == 0,
            Opcode::FontFace => flags <= 2,
            _ => flags == 0,
        };
        if !valid_flags {
            return Err(invalid);
        }
        match op {
            Opcode::DefineAtom => self.define_atom(a, c, string(b)?),
            Opcode::CreateElement => {
                let name = LocalName::from(String::from_utf16_lossy(&self.atom(b, &[1])?.value));
                let namespace = if c == 0 {
                    ns!()
                } else {
                    Namespace::from(String::from_utf16_lossy(&self.atom(c, &[4])?.value))
                };
                self.create(
                    id,
                    NodeKind::Element(Box::new(Element {
                        name,
                        namespace,
                        tag_atom: b,
                        namespace_atom: c,
                        attrs: Vec::new(),
                        inline: Vec::new(),
                        state: 0,
                        state_mask: 0,
                        value: None,
                        text_edit: None,
                        image_generation: 0,
                        blob_image_unavailable: false,
                        canvas_size: None,
                    })),
                )
            }
            Opcode::CreateText if c == 0 => self.create(id, NodeKind::Text(string(b)?)),
            Opcode::CreateNode => {
                let kind = match (b, c) {
                    (8, _) => NodeKind::Comment(string(c)?),
                    (9, u32::MAX) => NodeKind::Document,
                    (10, _) => NodeKind::Doctype(string(c)?),
                    (11, owner) => {
                        let template = if owner == u32::MAX {
                            None
                        } else {
                            let owner = NodeId::from_raw(owner);
                            let node = self.node_mut(owner)?;
                            let is_template = node.element().is_some_and(|element| {
                                element.namespace == ns!(html) && &*element.name == "template"
                            });
                            if !is_template || node.template_content.is_some() {
                                return Err(invalid);
                            }
                            node.template_content = Some(id);
                            Some(owner)
                        };
                        NodeKind::Fragment { template }
                    }
                    _ => return Err(invalid),
                };
                self.create(id, kind)
            }
            Opcode::Insert => self.insert(id, NodeId::from_raw(b), NodeId::from_raw(c)),
            Opcode::Remove if c == 0 => self.remove(id, NodeId::from_raw(b)),
            Opcode::SetAttr => self.set_attr(id, b, Some(&string(c)?)),
            Opcode::RemoveAttr if c == 0 => self.set_attr(id, b, None),
            Opcode::SetText if c == 0 => {
                let text = string(b)?;
                let node = self.node_mut(id)?;
                match &mut node.kind {
                    NodeKind::Text(value) => {
                        let changed_empty = value.is_empty() != text.is_empty();
                        *value = text;
                        if changed_empty && let Some(parent) = node.parent {
                            self.child_lists.entry(parent).or_default();
                        }
                    }
                    NodeKind::Comment(value) => *value = text,
                    _ => return Err(invalid),
                }
                self.mark_dirty(id)
            }
            Opcode::SetStyle => {
                let name = LocalName::from(String::from_utf16_lossy(&self.atom(b, &[3])?.value));
                let value = string(c)?;
                let element = self.element_mut(id)?;
                if let Some(index) = element
                    .inline
                    .iter()
                    .position(|declaration| declaration.name == name)
                {
                    element.inline.remove(index);
                }
                // Keep an empty override so clearing a property also removes it
                // from a style attribute that supplied the original declaration.
                element.inline.push(InlineDeclaration {
                    name,
                    value,
                    important: flags & STYLE_IMPORTANT != 0,
                });
                self.mark_dirty(id)
            }
            Opcode::SetState if (b | c) & !0xfff == 0 => {
                let element = self.element_mut(id)?;
                element.state = (element.state & !c) | (b & c);
                element.state_mask |= c;
                self.mark_dirty(id)
            }
            Opcode::SetValue if c == 0 => {
                let value = string(b)?;
                let node = self.node(id)?;
                let element = node.element().ok_or(ReplayError::InvalidNode(id))?;
                let unchanged = if let Some(previous) = element.value.as_deref() {
                    previous == value.as_ref()
                } else if element.input_kind().is_some() {
                    element
                        .attr(&ns!(), &markup5ever::local_name!("value"))
                        .map_or(value.is_empty(), |attr| attr.value == value)
                } else if element.is_text_control() {
                    node.children
                        .iter()
                        .filter_map(|&child| match &self.node(child).ok()?.kind {
                            NodeKind::Text(text) => Some(text.iter()),
                            _ => None,
                        })
                        .flatten()
                        .copied()
                        .eq(value.iter().copied())
                } else {
                    false
                };
                let element = self.element_mut(id)?;
                if !unchanged && element.is_text_control() {
                    let end = value.len() as u32;
                    let edit = element.text_edit.get_or_insert_with(Box::default);
                    **edit = super::TextEdit {
                        start: end,
                        end,
                        ..Default::default()
                    };
                }
                element.value = Some(value);
                self.mark_dirty(id)
            }
            Opcode::SetSelection if b <= c => {
                let element = self.element_mut(id)?;
                if !element.is_text_control() {
                    return Err(invalid);
                }
                let edit = element.text_edit.get_or_insert_with(Box::default);
                edit.start = b;
                edit.end = c;
                edit.backward = flags & SELECTION_BACKWARD != 0;
                Ok(())
            }
            Opcode::SetPreedit => {
                let text = string(b)?;
                let element = self.element_mut(id)?;
                if !element.is_text_control() || c as usize > text.len() {
                    return Err(invalid);
                }
                let edit = element.text_edit.get_or_insert_with(Box::default);
                edit.preedit = text;
                edit.cursor = c;
                Ok(())
            }
            Opcode::SheetInsert => {
                self.sheet_owner(id)?;
                let sheets = self.sheet(a);
                if b as usize > sheets.len() {
                    return Err(invalid);
                }
                sheets.insert(b as usize, string(c)?);
                Ok(())
            }
            Opcode::SheetDelete if c == 0 => {
                self.sheet_owner(id)?;
                let sheets = self.sheet(a);
                if b as usize >= sheets.len() {
                    return Err(invalid);
                }
                sheets.remove(b as usize);
                Ok(())
            }
            Opcode::ScrollTo => {
                let point = Point {
                    x: f32::from_bits(b),
                    y: f32::from_bits(c),
                };
                if !point.x.is_finite() || !point.y.is_finite() {
                    return Err(invalid);
                }
                self.node(id)?;
                self.scroll_commands.push(ScrollCommand::To(id, point));
                Ok(())
            }
            Opcode::Focus if c == 0 && b & !(FOCUS_BLUR | FOCUS_PREVENT_SCROLL) == 0 => {
                self.element_mut(id)?;
                if b & FOCUS_BLUR == 0 {
                    self.focused = Some(id);
                    if b & FOCUS_PREVENT_SCROLL == 0 {
                        self.scroll_commands.push(ScrollCommand::Focus(id));
                    }
                } else if self.focused == Some(id) {
                    self.focused = None;
                }
                Ok(())
            }
            Opcode::Release if b == 0 && c == 0 => self.release(id),
            Opcode::Listeners if b & !3 == 0 && c == 0 => {
                if id.is_none() {
                    self.listeners = b as u8;
                } else {
                    self.node_mut(id)?.listeners = b as u8;
                    self.mark_dirty(id)?;
                }
                Ok(())
            }
            Opcode::CanvasSubmit | Opcode::CanvasSize => {
                let element = self
                    .node(id)?
                    .element()
                    .ok_or(ReplayError::InvalidNode(id))?;
                if element.namespace != ns!(html)
                    || element.name != markup5ever::local_name!("canvas")
                {
                    return Err(invalid);
                }
                if op == Opcode::CanvasSize {
                    self.element_mut(id)?.canvas_size = Some([b, c]);
                }
                Ok(())
            }
            Opcode::FontFace if a != 0 => Ok(()),
            _ => Err(invalid),
        }
    }

    fn define_atom(&mut self, id: u32, kind: u32, value: Box<[u16]>) -> Result<(), ReplayError> {
        if id == 0 || !(1..=5).contains(&kind) || (kind == 5 && qualified(&value).is_none()) {
            return Err(ReplayError::InvalidAtom(id));
        }
        if let Some(old) = self.atoms.get(&id).or_else(|| self.document.atoms.get(&id)) {
            if old.kind != kind || old.value != value {
                return Err(ReplayError::InvalidAtom(id));
            }
        } else {
            self.atoms.insert(id, Atom { kind, value });
        }
        Ok(())
    }

    fn atom(&self, id: u32, kinds: &[u32]) -> Result<&Atom, ReplayError> {
        let atom = self
            .atoms
            .get(&id)
            .or_else(|| self.document.atoms.get(&id))
            .ok_or(ReplayError::MissingAtom(id))?;
        if !kinds.contains(&atom.kind) {
            return Err(ReplayError::InvalidAtom(id));
        }
        Ok(atom)
    }

    fn set_attr(
        &mut self,
        id: NodeId,
        atom_id: u32,
        value: Option<&[u16]>,
    ) -> Result<(), ReplayError> {
        let atom = self.atom(atom_id, &[2, 5])?;
        let (namespace, name) = if atom.kind == 5 {
            let (namespace, name) = qualified(&atom.value).expect("validated name atom");
            (Namespace::from(namespace), LocalName::from(name))
        } else {
            (
                ns!(),
                LocalName::from(String::from_utf16_lossy(&atom.value)),
            )
        };
        let style_attr = namespace == ns!() && name == markup5ever::local_name!("style");
        let src_attr = namespace == ns!() && name == markup5ever::local_name!("src");
        let bitmap_dimension = namespace == ns!() && matches!(&*name, "width" | "height");
        let element = self.element_mut(id)?;
        if bitmap_dimension && element.namespace == ns!(html) && &*element.name == "canvas" {
            element.canvas_size = None;
        }
        let old = element
            .attrs
            .iter()
            .position(|attr| attr.namespace == namespace && attr.name == name);
        if let Some(value) = value {
            let attr = Attribute {
                atom: atom_id,
                namespace,
                name,
                value: value.into(),
                scalar: String::from_utf16_lossy(value),
            };
            if let Some(index) = old {
                element.attrs[index] = attr;
            } else {
                element.attrs.push(attr);
            }
        } else if let Some(index) = old {
            element.attrs.remove(index);
        }
        if style_attr {
            element.inline.clear();
        }
        if src_attr
            && element.namespace == ns!(html)
            && element.name == markup5ever::local_name!("img")
        {
            element.image_generation = element
                .image_generation
                .checked_add(1)
                .ok_or(ReplayError::InvalidRecord(Opcode::SetAttr))?;
            element.blob_image_unavailable = false;
            let change = ImageSource {
                target: id,
                generation: element.image_generation,
                src: value.map(String::from_utf16_lossy),
            };
            self.image_sources.push(change);
        }
        self.mark_dirty(id)
    }

    fn node(&self, id: NodeId) -> Result<&DomNode, ReplayError> {
        match self.slots.get(&id.slot()) {
            Some(Slot::Live(node)) if node.id == id => Ok(node),
            None => self
                .document
                .node(id)
                .map(|node| &node.dom)
                .ok_or(ReplayError::InvalidNode(id)),
            _ => Err(ReplayError::InvalidNode(id)),
        }
    }

    fn node_mut(&mut self, id: NodeId) -> Result<&mut DomNode, ReplayError> {
        let slot = match self.slots.entry(id.slot()) {
            Entry::Occupied(slot) => slot.into_mut(),
            Entry::Vacant(slot) => {
                let node = self.document.node(id).ok_or(ReplayError::InvalidNode(id))?;
                slot.insert(Slot::Live(node.dom.clone()))
            }
        };
        match slot {
            Slot::Live(node) if node.id == id => Ok(node),
            _ => Err(ReplayError::InvalidNode(id)),
        }
    }

    fn element_mut(&mut self, id: NodeId) -> Result<&mut Element, ReplayError> {
        match &mut self.node_mut(id)?.kind {
            NodeKind::Element(element) => Ok(element),
            _ => Err(ReplayError::InvalidNode(id)),
        }
    }

    fn sheet_owner(&self, id: NodeId) -> Result<(), ReplayError> {
        let valid = self.node(id)?.element().is_some_and(|element| {
            (element.namespace == ns!(html) && matches!(&*element.name, "style" | "link"))
                || (element.namespace == ns!(svg) && &*element.name == "style")
        });
        if valid {
            Ok(())
        } else {
            Err(ReplayError::InvalidNode(id))
        }
    }

    fn sheet(&mut self, id: u32) -> &mut Vec<Box<[u16]>> {
        self.sheets
            .entry(id)
            .or_insert_with(|| self.document.sheets.get(&id).cloned().unwrap_or_default())
    }

    fn create(&mut self, id: NodeId, kind: NodeKind) -> Result<(), ReplayError> {
        if id.slot() == 0 {
            return Err(ReplayError::InvalidNode(id));
        }
        let available = match self.slots.get(&id.slot()) {
            None => match self.document.slots.get(&id.slot()) {
                None => true,
                Some(Slot::Vacant(generation)) => *generation == id.generation(),
                _ => false,
            },
            _ => false,
        };
        if !available {
            return Err(ReplayError::SlotGeneration(id));
        }
        self.slots
            .insert(id.slot(), Slot::Live(DomNode::new(id, kind)));
        Ok(())
    }

    fn insert(&mut self, parent: NodeId, child: NodeId, before: NodeId) -> Result<(), ReplayError> {
        let parent_node = self.node(parent)?;
        let child_node = self.node(child)?;
        if !matches!(
            parent_node.kind,
            NodeKind::Document | NodeKind::Element(_) | NodeKind::Fragment { .. }
        ) || matches!(
            child_node.kind,
            NodeKind::Document | NodeKind::Fragment { .. }
        ) || (matches!(child_node.kind, NodeKind::Doctype(_))
            && !matches!(parent_node.kind, NodeKind::Document))
            || child_node.parent.is_some()
        {
            return Err(ReplayError::InvalidInsert);
        }
        let mut ancestor = Some(parent);
        while let Some(id) = ancestor {
            if id == child {
                return Err(ReplayError::InvalidInsert);
            }
            ancestor = self.node(id)?.parent;
        }
        let index = if before.is_none() {
            parent_node.children.len()
        } else {
            self.node(before)?;
            parent_node
                .children
                .iter()
                .position(|&id| id == before)
                .ok_or(ReplayError::InvalidInsert)?
        };
        let prev = index
            .checked_sub(1)
            .map(|index| parent_node.children[index]);
        let next = parent_node.children.get(index).copied();
        self.node_mut(parent)?.children.insert(index, child);
        let child_node = self.node_mut(child)?;
        child_node.parent = Some(parent);
        child_node.prev = prev;
        child_node.next = next;
        if let Some(prev) = prev {
            self.node_mut(prev)?.next = Some(child);
        }
        if let Some(next) = next {
            self.node_mut(next)?.prev = Some(child);
        }
        self.record_child_list_change(parent, child, next)?;
        self.mark_dirty(child)
    }

    fn record_child_list_change(
        &mut self,
        parent: NodeId,
        child: NodeId,
        next: Option<NodeId>,
    ) -> Result<(), ReplayError> {
        match &self.node(child)?.kind {
            NodeKind::Element(_) => {
                let mut following = next;
                while let Some(id) = following {
                    let node = self.node(id)?;
                    if node.element().is_some() {
                        break;
                    }
                    following = node.next;
                }
                let change = self.child_lists.entry(parent).or_default();
                change.membership_changed = true;
                change.following.extend(following);
                self.relocated.insert(child);
            }
            NodeKind::Text(text) if !text.is_empty() => {
                self.child_lists.entry(parent).or_default();
            }
            _ => {}
        }
        Ok(())
    }

    fn remove(&mut self, parent: NodeId, child: NodeId) -> Result<(), ReplayError> {
        let child_node = self.node(child)?;
        if child_node.parent != Some(parent) {
            return Err(ReplayError::InvalidRemove);
        }
        let (prev, next) = (child_node.prev, child_node.next);
        let children = &mut self.node_mut(parent)?.children;
        let index = children
            .iter()
            .position(|&id| id == child)
            .ok_or(ReplayError::InvalidRemove)?;
        children.remove(index);
        let node = self.node_mut(child)?;
        node.parent = None;
        node.prev = None;
        node.next = None;
        if let Some(prev) = prev {
            self.node_mut(prev)?.next = next;
        }
        if let Some(next) = next {
            self.node_mut(next)?.prev = prev;
        }
        self.record_child_list_change(parent, child, next)?;
        self.mark_dirty(child)?;
        self.mark_dirty(parent)
    }

    fn release(&mut self, id: NodeId) -> Result<(), ReplayError> {
        let node = self.node(id)?;
        if id == self.document.root || node.parent.is_some() {
            return Err(ReplayError::InvalidRelease(id));
        }
        // A detached parent can be finalized before its children. Break the
        // mirror's links without acknowledging ids JS did not release.
        let children = node.children.clone();
        let template_content = node.template_content;
        let template = match node.kind {
            NodeKind::Fragment { template } => template,
            _ => None,
        };
        if let Some(content) = template_content {
            self.node_mut(content)?.kind = NodeKind::Fragment { template: None };
        }
        if let Some(template) = template {
            self.node_mut(template)?.template_content = None;
        }
        for child in children {
            let node = self.node_mut(child)?;
            node.parent = None;
            node.prev = None;
            node.next = None;
        }
        let slot = id
            .generation()
            .checked_add(1)
            .map_or(Slot::Retired, Slot::Vacant);
        self.slots.insert(id.slot(), slot);
        self.focused = self.focused.filter(|&value| value != id);
        self.released.push(id);
        Ok(())
    }

    fn mark_dirty(&mut self, id: NodeId) -> Result<(), ReplayError> {
        let node = self.node_mut(id)?;
        node.dirty |= ALL_DIRTY;
        let mut ancestor = node.parent;
        while let Some(id) = ancestor {
            let node = self.node_mut(id)?;
            node.descendants |= ALL_DIRTY;
            ancestor = node.parent;
        }
        Ok(())
    }
}

fn qualified(value: &[u16]) -> Option<(String, String)> {
    let cut = value.iter().position(|&unit| unit == 0)?;
    let namespace = &value[..cut];
    let name = &value[cut + 1..];
    if namespace.is_empty() || name.is_empty() || name.contains(&0) {
        return None;
    }
    let name = String::from_utf16_lossy(name);
    let local = name.rsplit(':').next()?.to_owned();
    (!local.is_empty()).then(|| (String::from_utf16_lossy(namespace), local))
}

#[cfg(test)]
mod tests {
    use squark_journal::{BatchWriter, Operand};
    use style::context::QuirksMode;

    use super::*;

    const ROOT: u32 = 1;
    const CHILD: u32 = 2;
    const TEXT: u32 = 3;
    const SURROGATE: &[u16] = &[0xd800];

    fn setup() -> (Document, BatchDecoder) {
        let id = NodeId::from_raw(ROOT);
        (
            Document::new(id, QuirksMode::NoQuirks).unwrap(),
            BatchDecoder::new(id).unwrap(),
        )
    }

    fn flush(
        doc: &mut Document,
        decoder: &mut BatchDecoder,
        records: &[(Opcode, u32, Operand<'_>, u32)],
    ) -> Result<FlushReport, ReplayError> {
        let mut writer = BatchWriter::new();
        for &(op, a, b, c) in records {
            writer.record(op, 0, a, b, c).unwrap();
        }
        doc.apply_journal(decoder, &writer.finish()[0])
    }

    #[test]
    fn release_acknowledges_ids_and_only_allows_reuse_in_the_following_flush() {
        let (mut doc, mut decoder) = setup();
        let text = NodeId::from_raw(TEXT);
        let create = (Opcode::CreateText, TEXT, SURROGATE.into(), 0);
        flush(&mut doc, &mut decoder, &[create]).unwrap();
        assert_eq!(doc.text(text), Some(SURROGATE));
        let next = NodeId::new(TEXT, 1).unwrap();
        let release = (Opcode::Release, TEXT, 0.into(), 0);
        let reuse = (Opcode::CreateText, next.raw(), SURROGATE.into(), 0);
        assert!(matches!(
            flush(&mut doc, &mut decoder, &[release, reuse]),
            Err(ReplayError::Journal(_))
        ));
        assert!(doc.text(text).is_some());
        assert_eq!(
            flush(&mut doc, &mut decoder, &[release]).unwrap().released,
            vec![text]
        );
        assert!(matches!(
            flush(&mut doc, &mut decoder, &[create]),
            Err(ReplayError::Journal(_))
        ));
        flush(&mut doc, &mut decoder, &[reuse]).unwrap();
        let stale = (Opcode::SetText, TEXT, SURROGATE.into(), 0);
        assert!(matches!(
            flush(&mut doc, &mut decoder, &[stale]),
            Err(ReplayError::Journal(_))
        ));
        assert_eq!(doc.text(next), Some(SURROGATE));
    }

    #[test]
    fn retires_wrapped_slots() {
        let (mut doc, mut decoder) = setup();
        for generation in 0..=u8::MAX {
            let id = NodeId::new(TEXT, generation).unwrap();
            flush(
                &mut doc,
                &mut decoder,
                &[
                    (Opcode::CreateText, id.raw(), SURROGATE.into(), 0),
                    (Opcode::Release, id.raw(), 0.into(), 0),
                ],
            )
            .unwrap();
        }
        let create = (Opcode::CreateText, TEXT, SURROGATE.into(), 0);
        assert!(matches!(
            flush(&mut doc, &mut decoder, &[create]),
            Err(ReplayError::Journal(_))
        ));
        assert!(doc.text(NodeId::from_raw(TEXT)).is_none());
    }

    #[test]
    fn malformed_topology_rolls_back_decoder_and_mirror() {
        let (mut doc, mut decoder) = setup();
        let text = NodeId::from_raw(TEXT);
        flush(
            &mut doc,
            &mut decoder,
            &[
                (Opcode::CreateText, TEXT, SURROGATE.into(), 0),
                (Opcode::Insert, ROOT, TEXT.into(), 0),
            ],
        )
        .unwrap();
        let malformed = [
            (Opcode::DefineAtom, 8, "div".into(), 1),
            (Opcode::CreateElement, CHILD, 8.into(), 0),
            (Opcode::Remove, ROOT, TEXT.into(), 0),
            (Opcode::Insert, TEXT, CHILD.into(), 0),
        ];
        assert_eq!(
            flush(&mut doc, &mut decoder, &malformed),
            Err(ReplayError::InvalidInsert)
        );
        assert_eq!(
            doc.children(NodeId::from_raw(ROOT)),
            Some([text].as_slice())
        );
        assert_eq!(doc.text(text), Some(SURROGATE));
        // Same slot and atom ID can be used since the failed batch committed neither one.
        flush(
            &mut doc,
            &mut decoder,
            &[
                (Opcode::DefineAtom, 8, "span".into(), 1),
                (Opcode::CreateElement, CHILD, 8.into(), 0),
            ],
        )
        .unwrap();
    }

    #[test]
    fn detaches_only_the_released_parent_references() {
        let (mut doc, mut decoder) = setup();
        flush(
            &mut doc,
            &mut decoder,
            &[
                (Opcode::CreateNode, CHILD, 11.into(), u32::MAX),
                (Opcode::CreateText, TEXT, SURROGATE.into(), 0),
                (Opcode::Insert, CHILD, TEXT.into(), 0),
            ],
        )
        .unwrap();
        let report = flush(
            &mut doc,
            &mut decoder,
            &[(Opcode::Release, CHILD, 0.into(), 0)],
        )
        .unwrap();
        assert_eq!(report.released, vec![NodeId::from_raw(CHILD)]);
        flush(
            &mut doc,
            &mut decoder,
            &[(Opcode::Release, TEXT, 0.into(), 0)],
        )
        .unwrap();
    }
    #[test]
    fn read_view_exposes_tree_links_raw_names_and_effective_listener_flags() {
        use squark_types::fragment::MirrorRead;
        let (mut doc, mut decoder) = setup();
        let tag = 8;
        let attr = 9;
        let xmlns = "http://www.w3.org/1999/xlink\0xlink:href";
        flush(
            &mut doc,
            &mut decoder,
            &[
                (Opcode::DefineAtom, tag, "svg".into(), 1),
                (Opcode::DefineAtom, attr, xmlns.into(), 5),
                (Opcode::CreateElement, CHILD, tag.into(), 0),
                (Opcode::CreateText, TEXT, SURROGATE.into(), 0),
                (Opcode::Insert, ROOT, TEXT.into(), 0),
                (Opcode::Insert, ROOT, CHILD.into(), TEXT),
                (Opcode::Listeners, 0, 1.into(), 0),
                (Opcode::Listeners, ROOT, 2.into(), 0),
            ],
        )
        .unwrap();
        let mut writer = BatchWriter::new();
        writer
            .record(Opcode::SetAttr, 0, CHILD, attr, SURROGATE)
            .unwrap();
        doc.apply_journal(&mut decoder, &writer.finish()[0])
            .unwrap();
        let mirror: &dyn MirrorRead = &doc;
        let child = NodeId::from_raw(CHILD);
        let text = NodeId::from_raw(TEXT);
        let node = mirror.node(child).unwrap();
        assert_eq!(node.links.next, Some(text));
        assert_eq!(mirror.node(text).unwrap().links.prev, Some(child));
        assert_eq!(mirror.effective_listeners(child).bits(), 3);
        assert_eq!(node.attrs[0].value, SURROGATE);
        assert_eq!(
            mirror.atom(node.attrs[0].name).unwrap(),
            xmlns.encode_utf16().collect::<Vec<_>>()
        );

        flush(
            &mut doc,
            &mut decoder,
            &[(Opcode::Remove, ROOT, CHILD.into(), 0)],
        )
        .unwrap();
        let mirror: &dyn MirrorRead = &doc;
        assert_eq!(mirror.node(text).unwrap().links.prev, None);
        assert_eq!(mirror.node(child).unwrap().links.next, None);
    }
    #[test]
    fn retains_template_content_as_an_inert_tree() {
        let (mut doc, mut decoder) = setup();
        let template = NodeId::from_raw(CHILD);
        let content = NodeId::from_raw(4);
        let text = NodeId::from_raw(TEXT);
        let mut writer = BatchWriter::new();
        writer
            .record(Opcode::DefineAtom, 0, 1, "template", 1)
            .unwrap();
        writer
            .record(Opcode::DefineAtom, 0, 2, "http://www.w3.org/1999/xhtml", 4)
            .unwrap();
        writer
            .record(Opcode::CreateElement, 0, template.raw(), 1, 2)
            .unwrap();
        writer
            .record(Opcode::CreateNode, 0, content.raw(), 11, template.raw())
            .unwrap();
        writer
            .record(Opcode::CreateText, 0, text.raw(), "inert", 0)
            .unwrap();
        writer
            .record(Opcode::Insert, 0, content.raw(), text.raw(), 0)
            .unwrap();
        writer
            .record(Opcode::Insert, 0, ROOT, template.raw(), 0)
            .unwrap();
        doc.apply_journal(&mut decoder, &writer.finish()[0])
            .unwrap();

        assert_eq!(
            doc.children(NodeId::from_raw(ROOT)),
            Some([template].as_slice())
        );
        assert_eq!(doc.children(template), Some([].as_slice()));
        assert_eq!(doc.template_content(template), Some(content));
        assert_eq!(doc.children(content), Some([text].as_slice()));
        assert_eq!(doc.node(content).unwrap().parent, None);
    }
}
