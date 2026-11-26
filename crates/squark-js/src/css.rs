// SPDX-License-Identifier: MIT

use squark_types::pack::ScriptKind;
use v8::{Function, Local, Object, PinScope, Value};

use crate::{
    bindings::{array, set, two_byte, utf16, with_bridge},
    bootstrap::PackedDocument,
    loader,
};

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    let parse = Function::new(scope, parse_rule)?;
    let media = Function::new(scope, evaluate_media)?;
    set(scope, host, "cssParseRule", parse.into())?;
    let css_supports = Function::new(scope, supports)?;
    set(scope, host, "cssSupports", css_supports.into())?;
    set(scope, host, "matchMedia", media.into())
}

fn supports(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments<'_>,
    mut result: v8::ReturnValue,
) {
    let Some(condition) = utf16(scope, args.get(0)) else {
        return;
    };
    let supported = with_bridge(scope, |bridge| bridge.engine.css_supports(&condition));
    result.set(v8::Boolean::new(scope, supported).into());
}

fn parse_rule(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let Some(text) = utf16(scope, args.get(0)) else {
        return;
    };
    let rule = with_bridge(scope, |bridge| bridge.engine.parse_rule(&text));
    if let Some(rule) = rule {
        if let Some(text) = two_byte(scope, &rule) {
            result.set(text.into());
        }
    } else {
        result.set_null();
    }
}

fn evaluate_media(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    let Some(query) = utf16(scope, args.get(0)) else {
        return;
    };
    let (media, matched) = with_bridge(scope, |bridge| bridge.engine.evaluate_media(&query));
    if let Some(media) = two_byte(scope, &media) {
        let object = Object::new(scope);
        let matched = v8::Boolean::new(scope, matched);
        if set(scope, object, "media", media.into()).is_some()
            && set(scope, object, "matches", matched.into()).is_some()
        {
            result.set(object.into());
        }
    }
}

pub(crate) fn packed_stylesheets<'s>(
    scope: &mut PinScope<'s, '_>,
    packed: &PackedDocument<'_>,
) -> Option<Local<'s, v8::Array>> {
    let links = packed
        .scripts()
        .filter(|(entry, _, _)| entry.kind == ScriptKind::ExternalStylesheet)
        .filter_map(|(entry, source, _)| {
            let path = String::from_utf16_lossy(&source.units().collect::<Vec<_>>());
            Some((entry.node_id, loader::stylesheet_url(scope, &path)?))
        })
        .collect::<Vec<_>>();
    array(scope, links, |scope, (node, url)| {
        let node = v8::Integer::new_from_unsigned(scope, node.raw());
        let url = v8::String::new(scope, &url)?;
        Some(v8::Array::new_with_elements(scope, &[node.into(), url.into()]).into())
    })
}

pub use squark_types::engine::{AnimationEvent, AnimationEventType};

fn event_name(kind: AnimationEventType) -> &'static str {
    use AnimationEventType::*;
    match kind {
        TransitionRun => "transitionrun",
        TransitionStart => "transitionstart",
        TransitionEnd => "transitionend",
        TransitionCancel => "transitioncancel",
        AnimationStart => "animationstart",
        AnimationIteration => "animationiteration",
        AnimationEnd => "animationend",
        AnimationCancel => "animationcancel",
    }
}

pub(crate) fn animation<'s>(
    scope: &mut PinScope<'s, '_>,
    event: AnimationEvent,
) -> Option<Local<'s, Value>> {
    let object = Object::new(scope);
    let target = v8::Integer::new_from_unsigned(scope, event.target.raw());
    let kind = v8::String::new(scope, event_name(event.kind))?;
    let name = v8::String::new(scope, &event.name)?;
    let elapsed = v8::Number::new(scope, event.elapsed_time);
    let pseudo = match event.pseudo {
        Some(pseudo) => v8::String::new(scope, &pseudo)?.into(),
        None => v8::null(scope).into(),
    };
    set(scope, object, "target", target.into())?;
    set(scope, object, "pseudo", pseudo)?;
    set(scope, object, "type", kind.into())?;
    set(scope, object, "name", name.into())?;
    set(scope, object, "elapsedTime", elapsed.into())?;
    Some(object.into())
}
