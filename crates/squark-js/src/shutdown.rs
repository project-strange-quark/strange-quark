// SPDX-License-Identifier: MIT

use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use squark_types::{engine::EngineError, render::ResourceSendError};

use crate::document::DocumentError;

pub(crate) const STORAGE_CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// A per-document stop signal shared with the host. Close it before tearing
/// down render services, then send `DocumentTask::Shutdown` to wake an idle
/// runner. A blocking backend must release its own pending requests on close.
#[derive(Clone, Debug, Default)]
pub struct DocumentShutdown(Arc<State>);

#[derive(Debug, Default)]
struct State {
    deadline: OnceLock<Instant>,
    isolate: Mutex<Option<v8::IsolateHandle>>,
}

impl DocumentShutdown {
    pub fn close(&self) {
        // Also serialize with the runner's own close before it clears V8
        // termination for the private storage drain.
        let isolate = self.0.isolate.lock().unwrap();
        if self
            .0
            .deadline
            .set(Instant::now() + STORAGE_CLOSE_BUDGET)
            .is_ok()
            && let Some(isolate) = isolate.as_ref()
        {
            isolate.terminate_execution();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.0.deadline.get().is_some()
    }

    pub(crate) fn attach(&self, isolate: v8::IsolateHandle) -> Result<(), DocumentError> {
        let mut registered = self.0.isolate.lock().unwrap();
        self.check()?;
        if registered.is_some() {
            return Err(DocumentError::Setup(
                "shutdown signal already belongs to a document".into(),
            ));
        }
        *registered = Some(isolate);
        Ok(())
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.0.deadline.get().copied()
    }

    pub(crate) fn check(&self) -> Result<(), DocumentError> {
        if self.is_closed() {
            Err(DocumentError::Closed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn engine<T>(&self, result: Result<T, EngineError>) -> Result<T, DocumentError> {
        // The engine exposes resource transport errors as strings. A host can
        // drop that transport before sending the queued shutdown message.
        if let Err(EngineError::Resource(message)) = &result
            && message == &ResourceSendError::Disconnected.to_string()
        {
            self.close();
        }
        self.check()?;
        result.map_err(DocumentError::Engine)
    }
}
