// SPDX-License-Identifier: MIT
//! Types every Strange Quark crate shares.
//!
//! One module per boundary between crates. A change to a module affects the
//! crates on both sides of it.

#![forbid(unsafe_code)]

pub mod engine;
pub mod fragment;
pub mod geometry;
pub mod host;
pub mod id;
pub mod journal;
pub mod pack;
pub mod render;
pub mod scroll;

pub use geometry::{Point, Rect, Size};
pub use id::NodeId;
pub use scroll::ScrollOffset;
