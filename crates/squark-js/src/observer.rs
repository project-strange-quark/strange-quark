// SPDX-License-Identifier: MIT

use squark_types::{
    NodeId,
    engine::{
        CssLength, IntersectionEntry, IntersectionOptions, LogicalSize, ResizeBox, ResizeEntry,
    },
};
use v8::{Function, Local, Object, PinScope, Value};

use crate::bindings::{array, get, rect, set, string, unsigned, with_bridge};

enum Registration {
    Resize(u32, NodeId, ResizeBox),
    UnobserveResize(u32, NodeId),
    DisconnectResize(u32),
    Intersection(u32, NodeId, IntersectionOptions),
    UnobserveIntersection(u32, NodeId),
    DisconnectIntersection(u32),
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    scope.set_slot(Vec::<Registration>::new());
    for (name, callback) in [
        ("observeResize", Function::new(scope, observe_resize)),
        ("unobserveResize", Function::new(scope, unobserve_resize)),
        ("disconnectResize", Function::new(scope, disconnect_resize)),
        (
            "observeIntersection",
            Function::new(scope, observe_intersection),
        ),
        (
            "unobserveIntersection",
            Function::new(scope, unobserve_intersection),
        ),
        (
            "disconnectIntersection",
            Function::new(scope, disconnect_intersection),
        ),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

pub(crate) fn apply_registrations(scope: &mut PinScope<'_, '_>) {
    let pending = std::mem::take(scope.get_slot_mut::<Vec<Registration>>().unwrap());
    with_bridge(scope, |bridge| {
        for change in pending {
            match change {
                Registration::Resize(id, target, box_kind) => {
                    bridge.engine.observe_resize(id, target, box_kind)
                }
                Registration::UnobserveResize(id, target) => {
                    bridge.engine.unobserve_resize(id, target)
                }
                Registration::DisconnectResize(id) => bridge.engine.disconnect_resize(id),
                Registration::Intersection(id, target, options) => {
                    bridge.engine.observe_intersection(id, target, options)
                }
                Registration::UnobserveIntersection(id, target) => {
                    bridge.engine.unobserve_intersection(id, target)
                }
                Registration::DisconnectIntersection(id) => {
                    bridge.engine.disconnect_intersection(id)
                }
            }
        }
    });
}

pub(crate) fn pending(scope: &PinScope<'_, '_>) -> bool {
    !scope.get_slot::<Vec<Registration>>().unwrap().is_empty()
}

fn queue(scope: &mut PinScope<'_, '_>, registration: Registration) {
    scope
        .get_slot_mut::<Vec<Registration>>()
        .unwrap()
        .push(registration);
}

fn target(args: &v8::FunctionCallbackArguments) -> Option<(u32, NodeId)> {
    Some((
        unsigned(args.get(0))?,
        NodeId::from_raw(unsigned(args.get(1))?),
    ))
}

fn observe_resize(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    let Some((id, node)) = target(&args) else {
        return;
    };
    let box_kind = match string(scope, args.get(2)).as_deref() {
        Some("content-box") => ResizeBox::ContentBox,
        Some("border-box") => ResizeBox::BorderBox,
        Some("device-pixel-content-box") => ResizeBox::DevicePixelContentBox,
        _ => return,
    };
    queue(scope, Registration::Resize(id, node, box_kind));
}

fn unobserve_resize(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    if let Some((id, node)) = target(&args) {
        queue(scope, Registration::UnobserveResize(id, node));
    }
}

fn disconnect_resize(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    if let Some(id) = unsigned(args.get(0)) {
        queue(scope, Registration::DisconnectResize(id));
    }
}

fn observe_intersection(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    if let Some((id, node)) = target(&args)
        && let Some(options) = read_options(scope, args.get(2))
    {
        queue(scope, Registration::Intersection(id, node, options));
    }
}

fn read_options(
    scope: &mut PinScope<'_, '_>,
    value: Local<'_, Value>,
) -> Option<IntersectionOptions> {
    let options = Local::<Object>::try_from(value).ok()?;
    let root = get(scope, options, "root")?;
    let root = if root.is_null() {
        None
    } else {
        Some(NodeId::from_raw(unsigned(root)?))
    };

    let margins = Local::<v8::Array>::try_from(get(scope, options, "rootMargin")?).ok()?;
    if margins.length() != 4 {
        return None;
    }
    let mut root_margin = [CssLength::Px(0.0); 4];
    for (index, output) in root_margin.iter_mut().enumerate() {
        let margin = Local::<Object>::try_from(margins.get_index(scope, index as u32)?).ok()?;
        let value = number(get(scope, margin, "value")?)?;
        let unit = get(scope, margin, "unit")?;
        *output = match string(scope, unit)?.as_str() {
            "px" => CssLength::Px(value),
            "%" => CssLength::Percent(value),
            _ => return None,
        };
    }
    let values = Local::<v8::Array>::try_from(get(scope, options, "thresholds")?).ok()?;
    let thresholds = (0..values.length())
        .map(|i| number(values.get_index(scope, i)?).filter(|v| (0.0..=1.0).contains(v)))
        .collect::<Option<Vec<_>>>()?;
    Some(IntersectionOptions {
        root,
        root_margin,
        thresholds,
    })
}

fn number(value: Local<'_, Value>) -> Option<f64> {
    let value = Local::<v8::Number>::try_from(value).ok()?.value();
    value.is_finite().then_some(value)
}

fn unobserve_intersection(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    if let Some((id, node)) = target(&args) {
        queue(scope, Registration::UnobserveIntersection(id, node));
    }
}

fn disconnect_intersection(
    scope: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    _: v8::ReturnValue,
) {
    if let Some(id) = unsigned(args.get(0)) {
        queue(scope, Registration::DisconnectIntersection(id));
    }
}

pub(crate) fn resize_entries<'s>(
    scope: &mut PinScope<'s, '_>,
    entries: Vec<ResizeEntry>,
) -> Option<Local<'s, Value>> {
    Some(
        array(scope, entries, |scope, entry| {
            let object = entry_object(scope, entry.observer, entry.target)?;
            let depth = v8::Integer::new_from_unsigned(scope, entry.depth);
            set(scope, object, "depth", depth.into())?;
            let content = rect(scope, entry.content_rect)?;
            set(scope, object, "contentRect", content.into())?;
            for (name, sizes) in [
                ("contentBoxSize", entry.content_box),
                ("borderBoxSize", entry.border_box),
                ("devicePixelContentBoxSize", entry.device_pixel_content_box),
            ] {
                let values = array(scope, sizes, size)?;
                set(scope, object, name, values.into())?;
            }
            Some(object.into())
        })?
        .into(),
    )
}

fn size<'s>(scope: &mut PinScope<'s, '_>, size: LogicalSize) -> Option<Local<'s, Value>> {
    let value = Object::new(scope);
    let inline = v8::Number::new(scope, size.inline_size);
    let block = v8::Number::new(scope, size.block_size);
    set(scope, value, "inlineSize", inline.into())?;
    set(scope, value, "blockSize", block.into())?;
    Some(value.into())
}

pub(crate) fn intersection_entries<'s>(
    scope: &mut PinScope<'s, '_>,
    entries: Vec<IntersectionEntry>,
) -> Option<Local<'s, Value>> {
    Some(
        array(scope, entries, |scope, entry| {
            let object = entry_object(scope, entry.observer, entry.target)?;
            let time = v8::Number::new(scope, entry.time.as_secs_f64() * 1000.0);
            let ratio = v8::Number::new(scope, entry.ratio);
            let is_intersecting = v8::Boolean::new(scope, entry.is_intersecting);
            let root = match entry.root_bounds {
                Some(bounds) => rect(scope, bounds)?.into(),
                None => v8::null(scope).into(),
            };
            let bounds = rect(scope, entry.bounds)?;
            let intersection = rect(scope, entry.intersection)?;
            set(scope, object, "time", time.into())?;
            set(scope, object, "intersectionRatio", ratio.into())?;
            set(scope, object, "isIntersecting", is_intersecting.into())?;
            set(scope, object, "rootBounds", root)?;
            set(scope, object, "boundingClientRect", bounds.into())?;
            set(scope, object, "intersectionRect", intersection.into())?;
            Some(object.into())
        })?
        .into(),
    )
}

fn entry_object<'s>(
    scope: &mut PinScope<'s, '_>,
    observer: u32,
    node: NodeId,
) -> Option<Local<'s, Object>> {
    let result = Object::new(scope);
    let observer = v8::Integer::new_from_unsigned(scope, observer);
    let node = v8::Integer::new_from_unsigned(scope, node.raw());
    set(scope, result, "observer", observer.into())?;
    set(scope, result, "target", node.into())?;
    Some(result)
}
