// SPDX-License-Identifier: MIT
//! Node identity (docs/architecture.md §4, "Node identity and lifetime").

/// A node's id as the journal carries it: a slot index in the low 24 bits and
/// a generation in the high 8 bits. Id 0 is never a node; records use it for
/// "none" (for example, `before = 0` in INSERT appends).
///
/// The generation lets the mirror reject a stale record that names a slot
/// reused after RELEASE.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(u32);

impl NodeId {
    pub const NONE: NodeId = NodeId(0);
    pub const SLOT_BITS: u32 = 24;
    pub const MAX_SLOT: u32 = (1 << Self::SLOT_BITS) - 1;

    /// Builds an id from a slot and a generation. Slot 0 is reserved.
    pub fn new(slot: u32, generation: u8) -> Option<NodeId> {
        if slot == 0 || slot > Self::MAX_SLOT {
            return None;
        }
        Some(NodeId(slot | (u32::from(generation) << Self::SLOT_BITS)))
    }

    pub const fn from_raw(raw: u32) -> NodeId {
        NodeId(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }

    pub const fn slot(self) -> u32 {
        self.0 & Self::MAX_SLOT
    }

    pub const fn generation(self) -> u8 {
        (self.0 >> Self::SLOT_BITS) as u8
    }

    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_and_generation_round_trip() {
        let id = NodeId::new(0x00AB_CDEF, 7).unwrap();
        assert_eq!(id.slot(), 0x00AB_CDEF);
        assert_eq!(id.generation(), 7);
        assert_eq!(NodeId::from_raw(id.raw()), id);
    }

    #[test]
    fn slot_zero_and_overflow_are_rejected() {
        assert!(NodeId::new(0, 0).is_none());
        assert!(NodeId::new(NodeId::MAX_SLOT + 1, 0).is_none());
        assert!(NodeId::NONE.is_none());
    }
}
