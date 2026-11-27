// SPDX-License-Identifier: MIT

//! Native module calls and events across V8 (architecture §13).
//!
//! JavaScript calls `__squark.nativeInvoke(id, module, command, json)`. The
//! dispatcher runs the command off the document thread and replies from any
//! thread; replies and module events wait in an inbox until the document
//! thread drains it, woken the way squark-net wakes it for network events.

use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use squark_types::native::{NativeCall, NativeDispatcher, NativeEvents, NativeResult};
use v8::{FunctionCallbackArguments, Local, Object, PinScope, ReturnValue, Value};

use crate::bindings::{set, string, throw, unsigned};

enum Message {
    Reply { id: u32, result: NativeResult },
    Event { name: String, payload: String },
}

type Waker = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct Inbox {
    queue: Mutex<VecDeque<Message>>,
    wake: Mutex<Option<Waker>>,
    closed: AtomicBool,
}

impl Inbox {
    fn push(&self, message: Message) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push_back(message);
        if let Some(wake) = self
            .wake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .as_ref()
        {
            wake();
        }
    }

    fn pop(&self) -> Option<Message> {
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .pop_front()
    }

    fn is_empty(&self) -> bool {
        self.queue
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .is_empty()
    }
}

impl NativeEvents for Inbox {
    fn emit(&self, event: &str, payload: String) {
        self.push(Message::Event {
            name: event.to_owned(),
            payload,
        });
    }
}

struct Native {
    dispatcher: Arc<dyn NativeDispatcher>,
    inbox: Arc<Inbox>,
}

pub(crate) fn install(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    dispatcher: Option<Arc<dyn NativeDispatcher>>,
) -> Option<()> {
    let Some(dispatcher) = dispatcher else {
        return Some(());
    };
    let inbox = Arc::new(Inbox::default());
    dispatcher.attach(inbox.clone());
    let modules = dispatcher.modules();
    scope.set_slot(Native { dispatcher, inbox });
    let names = modules
        .iter()
        .map(|name| v8::String::new(scope, name).map(Into::into))
        .collect::<Option<Vec<Local<Value>>>>()?;
    let names = v8::Array::new_with_elements(scope, &names);
    set(scope, host, "nativeModules", names.into())?;
    let invoke = v8::Function::new(scope, native_invoke)?;
    set(scope, host, "nativeInvoke", invoke.into())
}

fn native_invoke(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue) {
    let call = (|| {
        let id = unsigned(args.get(0))?;
        let call = NativeCall {
            module: string(scope, args.get(1))?,
            command: string(scope, args.get(2))?,
            args: string(scope, args.get(3))?,
        };
        Some((id, call))
    })();
    let Some((id, call)) = call else {
        throw(scope, "invalid native call", false);
        return;
    };
    let Some(native) = scope.get_slot::<Native>() else {
        throw(scope, "native modules are not available", false);
        return;
    };
    let inbox = Arc::clone(&native.inbox);
    native.dispatcher.invoke(
        call,
        Box::new(move |result| inbox.push(Message::Reply { id, result })),
    );
}

pub(crate) fn set_waker(scope: &mut PinScope<'_, '_>, wake: impl Fn() + Send + Sync + 'static) {
    if let Some(native) = scope.get_slot::<Native>() {
        *native
            .inbox
            .wake
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(Box::new(wake));
    }
}

pub(crate) fn wakeup(scope: &mut PinScope<'_, '_>) -> Option<Duration> {
    let native = scope.get_slot::<Native>()?;
    (!native.inbox.is_empty()).then_some(Duration::ZERO)
}

/// Drop events and replies that arrive after the document stops.
pub(crate) fn close(scope: &mut PinScope<'_, '_>) {
    if let Some(native) = scope.get_slot::<Native>() {
        native.inbox.closed.store(true, Ordering::Release);
    }
}

/// The next message for squark-dom's native dispatcher:
/// `["reply", id, ok, json]` or `["event", name, json]`.
pub(crate) fn next<'s>(scope: &mut PinScope<'s, '_>) -> Option<Local<'s, Value>> {
    let message = scope.get_slot::<Native>()?.inbox.pop()?;
    let values: Vec<Local<Value>> = match message {
        Message::Reply { id, result } => {
            let (ok, json) = match result {
                Ok(json) => (true, json),
                Err(json) => (false, json),
            };
            vec![
                v8::String::new(scope, "reply")?.into(),
                v8::Integer::new_from_unsigned(scope, id).into(),
                v8::Boolean::new(scope, ok).into(),
                v8::String::new(scope, &json)?.into(),
            ]
        }
        Message::Event { name, payload } => vec![
            v8::String::new(scope, "event")?.into(),
            v8::String::new(scope, &name)?.into(),
            v8::String::new(scope, &payload)?.into(),
        ],
    };
    Some(v8::Array::new_with_elements(scope, &values).into())
}
