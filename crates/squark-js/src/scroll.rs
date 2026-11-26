// SPDX-License-Identifier: MIT

//! Accept compositor samples by owner generation and expose a borrowed frame view.

use squark_types::{ScrollOffset, engine::ScrollSync, render::FrameId};
use v8::{Local, Object, PinScope, Value};

use crate::bindings::{array, set};

#[derive(Default)]
pub(crate) struct ScrollState {
    stamp: (u64, u64),
    offsets: Vec<ScrollOffset>,
}

impl ScrollState {
    pub fn update(
        &mut self,
        frame: FrameId,
        tick: u64,
        offsets: &[ScrollOffset],
    ) -> Vec<ScrollOffset> {
        let mut accepted = Vec::new();
        for &sample in offsets {
            if sample.owner.is_none()
                || !sample.offset.x.is_finite()
                || !sample.offset.y.is_finite()
            {
                continue;
            }
            match self
                .offsets
                .binary_search_by_key(&sample.owner, |entry| entry.owner)
            {
                Ok(index) => {
                    if !sample.is_newer_than(Some(self.offsets[index].generation)) {
                        continue;
                    }
                    self.offsets[index] = sample;
                }
                Err(index) => self.offsets.insert(index, sample),
            }
            accepted.push(sample);
        }
        if !accepted.is_empty() {
            self.stamp = self.stamp.max((frame.0, tick));
        }
        accepted
    }

    pub fn offset(&self, owner: squark_types::NodeId) -> Option<ScrollOffset> {
        let index = self
            .offsets
            .binary_search_by_key(&owner, |entry| entry.owner)
            .ok()?;
        Some(self.offsets[index])
    }

    pub fn as_input(&self) -> ScrollSync<'_> {
        ScrollSync {
            frame: self.stamp.0,
            tick: self.stamp.1,
            offsets: &self.offsets,
        }
    }
}

pub(crate) fn samples<'s>(
    scope: &mut PinScope<'s, '_>,
    samples: Vec<ScrollOffset>,
) -> Option<Local<'s, Value>> {
    Some(
        array(scope, samples, |scope, sample| {
            let value = Object::new(scope);
            let target = v8::Integer::new_from_unsigned(scope, sample.owner.raw());
            let x = v8::Number::new(scope, sample.offset.x.into());
            let y = v8::Number::new(scope, sample.offset.y.into());
            set(scope, value, "target", target.into())?;
            set(scope, value, "x", x.into())?;
            set(scope, value, "y", y.into())?;
            Some(value.into())
        })?
        .into(),
    )
}
