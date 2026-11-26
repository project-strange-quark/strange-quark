// SPDX-License-Identifier: MIT
//! Scroll offsets shared by the compositor, squark-js and the engine.

use crate::{NodeId, Point};

/// One scroll container's offset, with the generation that orders it.
///
/// Generations are per owner and increase on every change, whether the
/// compositor (a user scroll) or the engine (a document scroll) made it. A
/// receiver ignores an offset whose generation is not newer than the last one
/// it applied for that owner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollOffset {
    pub owner: NodeId,
    pub offset: Point,
    pub generation: u64,
}

impl ScrollOffset {
    /// True when `self` should replace an offset last applied at `applied`.
    pub fn is_newer_than(&self, applied: Option<u64>) -> bool {
        applied.is_none_or(|g| self.generation > g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_generations_win() {
        let o = ScrollOffset {
            owner: NodeId::new(1, 0).unwrap(),
            offset: Point { x: 0.0, y: 10.0 },
            generation: 3,
        };
        assert!(o.is_newer_than(None));
        assert!(o.is_newer_than(Some(2)));
        assert!(!o.is_newer_than(Some(3)));
    }
}
