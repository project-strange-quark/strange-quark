// SPDX-License-Identifier: MIT

use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
};

use squark_compositor::ScrollOffsets;
use squark_js::document::{DocumentError, DocumentTask, InputResult};
use squark_paint::{HitSnapshot, hit_test};
use squark_types::{
    NodeId,
    host::{DeltaMode, Input, InputTask, ListenerFlags},
};

use crate::{HostError, failure};

pub(crate) struct InputQueue {
    scroll: Arc<ScrollOffsets>,
    wheels: VecDeque<Wheel>,
}

struct Wheel {
    snapshot: Arc<HitSnapshot>,
    chain: Box<[NodeId]>,
    delta: [f64; 2],
    mode: DeltaMode,
    request: u32,
    decision: Option<mpsc::Receiver<Result<InputResult, DocumentError>>>,
}

impl InputQueue {
    pub fn new(scroll: Arc<ScrollOffsets>) -> Self {
        Self {
            scroll,
            wheels: VecDeque::new(),
        }
    }

    pub fn dispatch(
        &mut self,
        tasks: &mpsc::Sender<DocumentTask>,
        task: InputTask,
        snapshot: Option<Arc<HitSnapshot>>,
        decision: Option<mpsc::Sender<Result<InputResult, DocumentError>>>,
    ) -> Result<(), HostError> {
        // A key that may trigger a menu accelerator waits for the page's
        // decision; wheels make their own below.
        let mut reply = decision;
        let mut wheel = None;
        if let (Input::Wheel { css, delta, mode }, Some(snapshot)) = (&task.input, snapshot)
            && let Some(hit) = hit_test(&snapshot, *css)
            && hit.target == task.target
        {
            let decision = if hit.listeners.bits() & ListenerFlags::NON_PASSIVE_WHEEL.bits() != 0 {
                let (sender, receiver) = mpsc::channel();
                reply = Some(sender);
                Some(receiver)
            } else {
                None
            };
            wheel = Some(Wheel {
                snapshot,
                chain: hit.scroll_chain,
                delta: [delta[0], delta[1]],
                mode: *mode,
                request: task.request,
                decision,
            });
        }

        tasks
            .send(DocumentTask::Input { task, reply })
            .map_err(|_| failure("DocumentError", "document thread closed"))?;
        if let Some(wheel) = wheel {
            if wheel.decision.is_some() {
                self.wheels.push_back(wheel);
            } else {
                self.scroll
                    .wheel(&wheel.snapshot, &wheel.chain, wheel.delta, wheel.mode);
            }
        }
        Ok(())
    }

    pub fn pending(&self) -> bool {
        !self.wheels.is_empty()
    }

    /// Consume the cancelable wheel decisions in delivery order.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Some(wheel) = self.wheels.front() {
            let canceled = match wheel.decision.as_ref().map(mpsc::Receiver::try_recv) {
                None => false,
                Some(Ok(Ok(decision))) => {
                    decision.request != wheel.request || decision.default_prevented
                }
                Some(Err(mpsc::TryRecvError::Empty)) => break,
                Some(_) => true,
            };
            let wheel = self.wheels.pop_front().unwrap();
            if !canceled {
                changed |=
                    self.scroll
                        .wheel(&wheel.snapshot, &wheel.chain, wheel.delta, wheel.mode);
            }
        }
        changed
    }
}
