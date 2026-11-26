// SPDX-License-Identifier: MIT

use squark_types::engine::FaceId;
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue, Value};

use crate::bindings::{array, set, throw, unsigned, utf16, with_bridge};

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    for (name, callback) in [
        ("fontLoad", Function::new(scope, load)),
        ("fontMatch", Function::new(scope, matches)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn load(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    let Some(face) = unsigned(args.get(0)) else {
        throw(scope, "invalid font face id", false);
        return;
    };
    if let Err(error) = with_bridge(scope, |bridge| bridge.engine.font_load(FaceId(face))) {
        throw(scope, &error.to_string(), false);
    }
}

fn matches(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let (Some(font), Some(text)) = (utf16(scope, args.get(0)), utf16(scope, args.get(1))) else {
        throw(scope, "invalid font matching arguments", true);
        return;
    };
    match with_bridge(scope, |bridge| bridge.engine.match_font_faces(&font, &text)) {
        Ok(faces) => {
            if let Some(faces) = array(scope, faces, |scope, face| {
                Some(v8::Integer::new_from_unsigned(scope, face.0).into())
            }) {
                result.set(faces.into());
            }
        }
        Err(error) => throw(scope, &error.to_string(), true),
    }
}

pub(crate) fn resource<'s>(
    scope: &mut PinScope<'s, '_>,
    face: FaceId,
    result: Option<&Result<(), String>>,
) -> Option<[Local<'s, Value>; 2]> {
    let face = v8::Integer::new_from_unsigned(scope, face.0);
    let event = match result {
        None => v8::undefined(scope).into(),
        Some(Ok(())) => v8::null(scope).into(),
        Some(Err(message)) => v8::String::new(scope, message)?.into(),
    };
    Some([face.into(), event])
}
