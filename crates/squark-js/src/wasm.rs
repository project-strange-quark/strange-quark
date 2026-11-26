// SPDX-License-Identifier: MIT

use std::time::Duration;

use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue};

use crate::bindings::set;

#[derive(Default)]
struct Pending(usize);

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    scope.set_slot(Pending::default());
    let callback = Function::new(scope, pending)?;
    set(scope, host, "wasmPending", callback.into())
}

pub(crate) fn wakeup(scope: &PinScope<'_, '_>) -> Option<Duration> {
    (scope.get_slot::<Pending>()?.0 != 0).then_some(Duration::from_millis(2))
}

fn pending(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    let pending = &mut scope.get_slot_mut::<Pending>().unwrap().0;
    if args.get(0).is_true() {
        *pending += 1;
    } else {
        *pending = pending.saturating_sub(1);
    }
}
