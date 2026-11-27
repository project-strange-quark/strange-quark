// SPDX-License-Identifier: MIT

//! Host input messages crossing into the document realm and their replies.

use std::{sync::mpsc::Sender, time::Instant};

use squark_types::host::{
    ImeKind, Input, InputTask, KeyKind, PointerKind, PointerSample, PointerType,
};
use v8::{Function, Local, Object, PinScope, Value};

use crate::{
    bindings::set,
    document::{DocumentError, millis},
    task_loop::call,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputResult {
    pub request: u32,
    pub default_prevented: bool,
}

#[derive(Debug)]
pub(crate) struct QueuedInput {
    pub task: InputTask,
    pub reply: Option<Sender<Result<InputResult, DocumentError>>>,
    previous_samples: Vec<PointerSample>,
}

impl QueuedInput {
    pub fn new(task: InputTask, reply: Option<Sender<Result<InputResult, DocumentError>>>) -> Self {
        Self {
            task,
            reply,
            previous_samples: Vec::new(),
        }
    }

    pub fn is_deferred_move(&self) -> bool {
        self.reply.is_none()
            && matches!(
                self.task.input,
                Input::Pointer {
                    kind: PointerKind::Move,
                    boundary_update: false,
                    ..
                }
            )
    }

    pub fn finish(&mut self) {
        if !self.previous_samples.is_empty()
            && let Input::Pointer { samples, .. } = &mut self.task.input
        {
            let mut merged = std::mem::take(&mut self.previous_samples);
            merged.extend(std::mem::take(samples).into_vec());
            merged.sort_by_key(|sample| sample.time);
            *samples = merged.into_boxed_slice();
        }
    }

    /// Keep only the newest move across consecutive, compatible messages.
    /// The older samples stay available to getCoalescedEvents().
    pub fn coalesce(&mut self, next: &mut Self) -> bool {
        if self.reply.is_some()
            || next.reply.is_some()
            || self.task.target != next.task.target
            || self.task.related != next.task.related
            || self.task.modifiers != next.task.modifiers
            || self.task.time > next.task.time
        {
            return false;
        }
        let Input::Pointer {
            kind: PointerKind::Move,
            boundary_update: false,
            id,
            device,
            primary,
            css,
            pressure,
            buttons,
            samples,
            ..
        } = &mut self.task.input
        else {
            return false;
        };
        let Input::Pointer {
            kind: PointerKind::Move,
            boundary_update: false,
            id: next_id,
            device: next_device,
            primary: next_primary,
            buttons: next_buttons,
            ..
        } = &next.task.input
        else {
            return false;
        };
        if (*id, *device, *primary, *buttons)
            != (*next_id, *next_device, *next_primary, *next_buttons)
        {
            return false;
        }
        self.previous_samples
            .extend(std::mem::take(samples).into_vec());
        self.previous_samples.push(PointerSample {
            time: self.task.time,
            css: *css,
            pressure: *pressure,
        });
        std::mem::swap(&mut self.task, &mut next.task);
        true
    }
}

struct Reply {
    request: u32,
    prevented: Option<bool>,
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    let callback = Function::new(scope, input_result)?;
    set(scope, host, "inputResult", callback.into())
}

fn input_result(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    let (Ok(request), Ok(prevented)) = (
        Local::<v8::Uint32>::try_from(args.get(0)),
        Local::<v8::Boolean>::try_from(args.get(1)),
    ) else {
        return;
    };
    if let Some(reply) = scope.get_slot_mut::<Reply>()
        && reply.request == request.value()
        && reply.prevented.is_none()
    {
        reply.prevented = Some(prevented.is_true());
    }
}

pub(crate) fn dispatch<'s>(
    scope: &mut PinScope<'s, '_>,
    dispatcher: Local<'s, Function>,
    task: &InputTask,
    cancelable: bool,
    origin: Instant,
) -> Result<InputResult, DocumentError> {
    let request = task.request;
    let message = encode(scope, task, cancelable, origin)
        .ok_or_else(|| DocumentError::Setup("could not encode host input".into()))?;
    scope.set_slot(Reply {
        request,
        prevented: None,
    });
    let result = call(scope, dispatcher, &[message.into()]);
    let reply = scope.remove_slot::<Reply>().unwrap();
    result.map_err(|error| DocumentError::JavaScript(error.to_string()))?;
    let default_prevented = reply
        .prevented
        .ok_or_else(|| DocumentError::JavaScript("host input did not return inputResult".into()))?;
    Ok(InputResult {
        request,
        default_prevented,
    })
}

fn encode<'s>(
    scope: &mut PinScope<'s, '_>,
    task: &InputTask,
    cancelable: bool,
    origin: Instant,
) -> Option<Local<'s, Object>> {
    let msg = Object::new(scope);
    numbers(
        scope,
        msg,
        [
            ("id", task.request.into()),
            ("target", task.target.raw().into()),
            ("related", task.related.raw().into()),
            ("time", millis(task.time, origin)),
        ],
    )?;
    boolean(scope, msg, "cancelable", cancelable)?;

    let fields = Object::new(scope);
    for (name, value) in [
        ("altKey", task.modifiers.alt),
        ("ctrlKey", task.modifiers.ctrl),
        ("metaKey", task.modifiers.meta),
        ("shiftKey", task.modifiers.shift),
    ] {
        boolean(scope, fields, name, value)?;
    }
    let kind = match &task.input {
        Input::Pointer {
            kind,
            boundary_update,
            click_count,
            id,
            device,
            primary,
            css,
            pressure,
            button,
            buttons,
            samples,
            ..
        } => {
            numbers(
                scope,
                fields,
                [
                    ("pointerId", (*id).into()),
                    ("x", css.x.into()),
                    ("y", css.y.into()),
                    ("pressure", (*pressure).into()),
                    ("button", (*button).into()),
                    ("buttons", (*buttons).into()),
                ],
            )?;
            string(
                scope,
                fields,
                "pointerType",
                match device {
                    PointerType::Mouse => "mouse",
                    PointerType::Pen => "pen",
                    PointerType::Touch => "touch",
                },
            )?;
            boolean(scope, fields, "isPrimary", *primary)?;
            boolean(scope, fields, "boundaryUpdate", *boundary_update)?;
            if let Some(click_count) = click_count {
                numbers(scope, fields, [("clickCount", f64::from(*click_count))])?;
            }
            let samples = samples
                .iter()
                .map(|sample| {
                    let object = Object::new(scope);
                    numbers(
                        scope,
                        object,
                        [
                            ("x", sample.css.x.into()),
                            ("y", sample.css.y.into()),
                            ("pressure", sample.pressure.into()),
                            ("time", millis(sample.time, origin)),
                        ],
                    )?;
                    Some(Local::<Value>::from(object))
                })
                .collect::<Option<Vec<_>>>()?;
            let samples = v8::Array::new_with_elements(scope, &samples);
            set(scope, fields, "samples", samples.into())?;
            match kind {
                PointerKind::Down => "pointerdown",
                PointerKind::Up => "pointerup",
                PointerKind::Move => "pointermove",
                PointerKind::Cancel => "pointercancel",
            }
        }
        Input::Wheel { css, delta, mode } => {
            numbers(
                scope,
                fields,
                [
                    ("x", css.x.into()),
                    ("y", css.y.into()),
                    ("deltaX", delta[0]),
                    ("deltaY", delta[1]),
                    ("deltaZ", delta[2]),
                    ("deltaMode", (*mode as u8).into()),
                ],
            )?;
            "wheel"
        }
        Input::Key {
            kind,
            key,
            code,
            location,
            repeat,
            composing,
        } => {
            string(scope, fields, "key", key)?;
            string(scope, fields, "code", code)?;
            numbers(scope, fields, [("location", (*location).into())])?;
            boolean(scope, fields, "repeat", *repeat)?;
            boolean(scope, fields, "isComposing", *composing)?;
            match kind {
                KeyKind::Down => "keydown",
                KeyKind::Up => "keyup",
            }
        }
        Input::Ime {
            kind,
            text,
            selection_utf16,
        } => {
            string(
                scope,
                fields,
                "kind",
                match kind {
                    ImeKind::Start => "start",
                    ImeKind::Update => "update",
                    ImeKind::Commit => "commit",
                    ImeKind::End => "end",
                },
            )?;
            string(scope, fields, "text", text)?;
            let selection = match selection_utf16 {
                Some([start, end]) => {
                    let start = v8::Integer::new_from_unsigned(scope, *start);
                    let end = v8::Integer::new_from_unsigned(scope, *end);
                    v8::Array::new_with_elements(scope, &[start.into(), end.into()]).into()
                }
                None => v8::null(scope).into(),
            };
            set(scope, fields, "selection", selection)?;
            "ime"
        }
    };
    string(scope, msg, "type", kind)?;
    set(scope, msg, "fields", fields.into())?;
    Some(msg)
}

fn numbers(
    scope: &mut PinScope<'_, '_>,
    object: Local<'_, Object>,
    entries: impl IntoIterator<Item = (&'static str, f64)>,
) -> Option<()> {
    for (key, value) in entries {
        let value = v8::Number::new(scope, value);
        set(scope, object, key, value.into())?;
    }
    Some(())
}

fn boolean(
    scope: &mut PinScope<'_, '_>,
    object: Local<'_, Object>,
    key: &str,
    value: bool,
) -> Option<()> {
    let value = v8::Boolean::new(scope, value);
    set(scope, object, key, value.into())
}

fn string(
    scope: &mut PinScope<'_, '_>,
    object: Local<'_, Object>,
    key: &str,
    value: &str,
) -> Option<()> {
    let value = v8::String::new(scope, value)?;
    set(scope, object, key, value.into())
}

#[cfg(test)]
mod tests {
    use squark_types::{NodeId, Point, host::Modifiers};

    use super::*;

    #[test]
    fn click_count_reaches_v8_input_fields_as_an_optional_number() {
        crate::initialize_test_v8();
        let origin = Instant::now();
        let task = |click_count| InputTask {
            request: 1,
            target: NodeId::NONE,
            related: NodeId::NONE,
            time: origin,
            modifiers: Modifiers::default(),
            input: Input::Pointer {
                kind: PointerKind::Up,
                boundary_update: false,
                click_count,
                id: 1,
                device: PointerType::Mouse,
                primary: true,
                css: Point { x: 1.0, y: 2.0 },
                pressure: 0.0,
                button: 0,
                buttons: 0,
                samples: Box::new([]),
            },
        };

        let isolate = &mut v8::Isolate::new(Default::default());
        v8::scope!(let scope, isolate);
        let context = v8::Context::new(scope, Default::default());
        let scope = &mut v8::ContextScope::new(scope, context);
        let fields_name = v8::String::new(scope, "fields").unwrap();
        let click_count_name = v8::String::new(scope, "clickCount").unwrap();

        for (click_count, expected) in [(Some(2), Some(2.0)), (None, None)] {
            let encoded = encode(scope, &task(click_count), true, origin).unwrap();
            let fields =
                v8::Local::<Object>::try_from(encoded.get(scope, fields_name.into()).unwrap())
                    .unwrap();
            let value = fields.get(scope, click_count_name.into()).unwrap();
            if let Some(expected) = expected {
                assert!(value.is_number());
                assert_eq!(value.number_value(scope), Some(expected));
            } else {
                assert!(value.is_undefined());
            }
        }
    }
}
