// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::fmt;

use squark_types::NodeId;
use squark_types::journal::{
    AtomKind, DecodeError, ElementState, FOCUS_BLUR, FOCUS_PREVENT_SCROLL, FontFaceAction,
    ListenerFlags, NO_STRING, NodeType, Opcode, RECORD_SIZE, Record, SELECTION_BACKWARD,
    STYLE_IMPORTANT,
};

use crate::batch::{Batch, BatchError, StringTable};

const HTML_NAMESPACE: &str = "http://www.w3.org/1999/xhtml";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordError {
    Decode(DecodeError),
    Flags(u8),
    Operand(&'static str),
    StringIndex(u32),
    Atom(u32),
    Node(NodeId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalError {
    DocumentId(NodeId),
    Batch(BatchError),
    Record { index: usize, error: RecordError },
}

impl fmt::Display for JournalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DocumentId(id) => write!(f, "invalid document id {}", id.raw()),
            Self::Batch(error) => write!(f, "invalid journal batch: {error:?}"),
            Self::Record { index, error } => write!(f, "invalid journal record {index}: {error:?}"),
        }
    }
}

impl std::error::Error for JournalError {}

#[derive(Debug)]
pub struct Atom {
    pub kind: AtomKind,
    pub text: Box<[u16]>,
}

#[derive(Clone, Copy, Debug)]
enum Slot {
    Live { generation: u8, template: bool },
    Released(u8),
}

#[derive(Debug, Default)]
struct State {
    atoms: HashMap<u32, Atom>,
    highest_atom: u32,
    slots: HashMap<u32, Slot>,
}

/// One decoder per document. Atom definitions and generations survive flushes.
#[derive(Debug)]
pub struct BatchDecoder {
    document: NodeId,
    state: State,
}

impl BatchDecoder {
    /// Seeds the document node from document.bin's header. Pass NONE for a
    /// stream with no preexisting document node.
    pub fn new(document: NodeId) -> Result<Self, JournalError> {
        let mut state = State::default();
        if !document.is_none() {
            if document.slot() == 0 {
                return Err(JournalError::DocumentId(document));
            }
            state.slots.insert(
                document.slot(),
                Slot::Live {
                    generation: document.generation(),
                    template: false,
                },
            );
        }
        Ok(Self { document, state })
    }

    /// Checks the complete wire batch before exposing any records. The mirror
    /// can then validate topology and resource dependencies and apply it.
    pub fn validate<'state, 'bytes>(
        &'state mut self,
        bytes: &'bytes [u8],
    ) -> Result<DecodedBatch<'state, 'bytes>, JournalError> {
        let batch = Batch::new(bytes).map_err(JournalError::Batch)?;
        let mut changes = State {
            highest_atom: self.state.highest_atom,
            ..State::default()
        };
        for (index, bytes) in batch.records.iter().enumerate() {
            let result = Record::decode(bytes, index * RECORD_SIZE)
                .map_err(RecordError::Decode)
                .and_then(|record| self.validate_record(record, batch.strings, &mut changes));
            result.map_err(|error| JournalError::Record { index, error })?;
        }
        Ok(DecodedBatch {
            batch,
            decoder: self,
            changes,
        })
    }

    fn validate_record(
        &self,
        record: Record,
        strings: StringTable<'_>,
        changes: &mut State,
    ) -> Result<(), RecordError> {
        use Opcode::*;
        use RecordError::{Node, Operand};

        let Record { op, flags, a, b, c } = record;
        let flags_valid = match op {
            FontFace => FontFaceAction::from_raw(flags).is_some(),
            SetStyle => flags & !STYLE_IMPORTANT == 0,
            SetSelection => flags & !SELECTION_BACKWARD == 0,
            _ => flags == 0,
        };
        if !flags_valid {
            return Err(RecordError::Flags(flags));
        }
        if matches!(
            op,
            CreateText
                | Remove
                | RemoveAttr
                | SetText
                | SetValue
                | SheetDelete
                | Focus
                | Release
                | Listeners
                | AttachShadow
                | AssignSlot
                | CreateSheet
                | AdoptSheet
        ) && c != 0
        {
            return Err(Operand("unused c"));
        }

        let string = |index| strings.get(index).ok_or(RecordError::StringIndex(index));
        let atom = |id, kinds: &[AtomKind]| {
            find_atom(changes, &self.state, id)
                .filter(|atom| kinds.contains(&atom.kind))
                .ok_or(RecordError::Atom(id))
        };
        let node = |raw| {
            let id = NodeId::from_raw(raw);
            match changes
                .slots
                .get(&id.slot())
                .or_else(|| self.state.slots.get(&id.slot()))
            {
                Some(Slot::Live {
                    generation,
                    template,
                }) if *generation == id.generation() => Ok((id, *template)),
                _ => Err(Node(id)),
            }
        };

        let mut template = false;
        match op {
            CreateElement => {
                let tag = atom(b, &[AtomKind::Tag])?;
                if c != 0 {
                    let namespace = atom(c, &[AtomKind::Namespace])?;
                    template = tag.text.iter().copied().eq("template".encode_utf16())
                        && namespace
                            .text
                            .iter()
                            .copied()
                            .eq(HTML_NAMESPACE.encode_utf16());
                }
            }
            CreateText => {
                string(b)?;
            }
            CreateNode => {
                let kind = NodeType::from_raw(b).ok_or(Operand("nodeType"))?;
                if kind.has_string() {
                    string(c)?;
                } else if kind == NodeType::DocumentFragment && c != NO_STRING {
                    let (_, is_template) = node(c)?;
                    if !is_template {
                        return Err(Operand(
                            "template fragment owner is not an HTML template element",
                        ));
                    }
                } else if c != NO_STRING {
                    return Err(Operand("missing NO_STRING sentinel"));
                }
            }
            DefineAtom => {
                let kind = AtomKind::from_raw(c).ok_or(Operand("atom kind"))?;
                let text = string(b)?;
                if a == 0 {
                    return Err(RecordError::Atom(a));
                }
                if let Some(defined) = find_atom(changes, &self.state, a) {
                    if kind != defined.kind || !text.units().eq(defined.text.iter().copied()) {
                        return Err(RecordError::Atom(a));
                    }
                } else {
                    if a <= changes.highest_atom {
                        return Err(RecordError::Atom(a));
                    }
                    if kind == AtomKind::QualifiedAttribute {
                        let mut units = text.units();
                        if !matches!(units.next(), Some(first) if first != 0)
                            || !units.any(|unit| unit == 0)
                            || !matches!(units.next(), Some(first) if first != 0)
                            || units.any(|unit| unit == 0)
                        {
                            return Err(Operand("qualified attribute name"));
                        }
                    }
                    changes.atoms.insert(
                        a,
                        Atom {
                            kind,
                            text: text.units().collect(),
                        },
                    );
                    changes.highest_atom = a;
                }
            }
            Insert | Remove => {
                node(a)?;
                node(b)?;
                if c != 0 {
                    node(c)?;
                }
            }
            SetAttr | RemoveAttr => {
                node(a)?;
                atom(b, &[AtomKind::Attribute, AtomKind::QualifiedAttribute])?;
                if op == SetAttr {
                    string(c)?;
                }
            }
            SetText | SetValue => {
                node(a)?;
                string(b)?;
            }
            SetSelection => {
                node(a)?;
                if b > c {
                    return Err(Operand("selection start exceeds end"));
                }
            }
            SetPreedit => {
                node(a)?;
                if c as usize > string(b)?.units().len() {
                    return Err(Operand("cursor exceeds preedit length"));
                }
            }
            SetStyle => {
                node(a)?;
                atom(b, &[AtomKind::CssProperty])?;
                string(c)?;
            }
            SetState => {
                node(a)?;
                ElementState::from_bits(b).ok_or(Operand("element state"))?;
                ElementState::from_bits(c).ok_or(Operand("element state mask"))?;
            }
            SheetInsert => {
                node(a)?;
                string(c)?;
            }
            SheetDelete => {
                node(a)?;
            }
            ScrollTo | CanvasSize => {
                node(a)?;
            }
            Focus => {
                node(a)?;
                if b & !(FOCUS_BLUR | FOCUS_PREVENT_SCROLL) != 0 {
                    return Err(Operand("focus flags"));
                }
            }
            Release => {
                let (id, _) = node(a)?;
                if b != 0 || id == self.document {
                    return Err(Operand("release"));
                }
                changes
                    .slots
                    .insert(id.slot(), Slot::Released(id.generation()));
            }
            CanvasSubmit => {
                node(a)?;
                // The engine checks the id and length against its staging store.
            }
            FontFace => {
                string(b)?;
                string(c)?;
            }
            Listeners => {
                if a != 0 {
                    node(a)?;
                }
                ListenerFlags::from_bits(b).ok_or(Operand("listener flags"))?;
            }
            AttachShadow => {
                node(b)?;
            }
            AssignSlot | AdoptSheet => {
                node(a)?;
                if b != 0 {
                    node(b)?;
                }
            }
            CreateSheet => {
                if b != NO_STRING {
                    string(b)?;
                }
            }
        }

        if matches!(
            op,
            CreateElement | CreateText | CreateNode | AttachShadow | CreateSheet
        ) {
            let id = NodeId::from_raw(a);
            // A release in this batch has not been acknowledged yet.
            if id.slot() == 0 || changes.slots.contains_key(&id.slot()) {
                return Err(Node(id));
            }
            let expected = match self.state.slots.get(&id.slot()) {
                None => Some(0),
                Some(Slot::Released(generation)) => generation.checked_add(1),
                Some(Slot::Live { .. }) => None,
            };
            if expected != Some(id.generation()) {
                return Err(Node(id));
            }
            changes.slots.insert(
                id.slot(),
                Slot::Live {
                    generation: id.generation(),
                    template,
                },
            );
        }
        Ok(())
    }
}

fn find_atom<'a>(changes: &'a State, state: &'a State, id: u32) -> Option<&'a Atom> {
    changes.atoms.get(&id).or_else(|| state.atoms.get(&id))
}

/// A validated view of the borrowed batch and its atom table. Dropping the
/// view leaves the decoder unchanged so a failed mirror application can retry.
#[derive(Debug)]
#[must_use = "apply the batch, then commit it to acknowledge the writes"]
pub struct DecodedBatch<'state, 'bytes> {
    batch: Batch<'bytes>,
    decoder: &'state mut BatchDecoder,
    changes: State,
}

impl<'bytes> DecodedBatch<'_, 'bytes> {
    /// The original validated record region, in wire order.
    pub fn record_bytes(&self) -> &'bytes [u8] {
        self.batch.records.as_flattened()
    }

    pub fn records(&self) -> impl ExactSizeIterator<Item = Record> + '_ {
        self.batch.records.iter().enumerate().map(|(index, bytes)| {
            Record::decode(bytes, index * RECORD_SIZE).expect("record was validated")
        })
    }

    pub fn strings(&self) -> StringTable<'bytes> {
        self.batch.strings
    }

    pub fn atom(&self, id: u32) -> Option<&Atom> {
        find_atom(&self.changes, &self.decoder.state, id)
    }

    /// Acknowledges the batch after a successful mirror application.
    pub fn commit(self) {
        let state = &mut self.decoder.state;
        state.atoms.extend(self.changes.atoms);
        state.highest_atom = self.changes.highest_atom;
        state.slots.extend(self.changes.slots);
    }
}
