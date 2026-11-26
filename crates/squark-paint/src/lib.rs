// SPDX-License-Identifier: MIT
//! Document-thread CSS paint and CPU hit testing.

pub mod display_items;

mod animation;
mod damage;
mod frame;
mod gradient;
mod hit;
mod properties;
mod shared;
mod stacking;

mod input;

#[cfg(test)]
mod tests;

pub use animation::{AnimationSample, AnimationTarget};
pub use damage::{frame_damage, project_bounds};
pub use frame::*;
pub use gradient::resolve_gradient;
pub use hit::*;
pub use properties::*;
pub use shared::{build_frame, build_hit_test, resolve_length, resolve_position};
