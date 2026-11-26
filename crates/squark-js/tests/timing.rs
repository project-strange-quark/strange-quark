// SPDX-License-Identifier: MIT

#![allow(unsafe_code)]

mod support;

use std::cell::Cell;
use std::rc::Rc;

use squark_js::task_loop::{TaskLoop, install_task_host};
use support::{export, initialize_v8, load_dom_fixture};

fn read_fixture<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    fixture: v8::Local<'s, v8::Object>,
    name: &str,
) -> String {
    let callback = export(scope, fixture, name);
    let receiver = v8::undefined(scope);
    callback
        .call(scope, receiver.into(), &[])
        .unwrap()
        .to_rust_string_lossy(scope)
}

#[test]
fn fake_clock_drives_task_turns_microtasks_and_frames() {
    initialize_v8();
    let isolate = &mut v8::Isolate::new(Default::default());
    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let time = Rc::new(Cell::new(0.0));
    let host = install_task_host(scope, {
        let time = time.clone();
        move || time.get()
    })
    .unwrap();

    let fixture = load_dom_fixture(scope, "timing.mjs", include_str!("fixtures/timing.mjs"));
    let receiver = v8::undefined(scope);
    let installed = export(scope, fixture, "install")
        .call(scope, receiver.into(), &[host.into()])
        .unwrap();
    let installed = v8::Local::<v8::Object>::try_from(installed).unwrap();
    let runtime = TaskLoop::new(scope, installed).unwrap();
    read_fixture(scope, fixture, "seed");
    assert_eq!(read_fixture(scope, fixture, "drain"), "");
    runtime.checkpoint_after_startup(scope);
    assert_eq!(
        read_fixture(scope, fixture, "drain"),
        "startup promise|startup microtask"
    );
    assert_eq!(runtime.next_timer_deadline(scope), Some(5.0));
    assert!(runtime.has_posted_tasks(scope));
    assert_eq!(runtime.run_ready(scope, 0.0, 10).unwrap(), 0);
    assert_eq!(runtime.run_ready(scope, 1.0, 1).unwrap(), 1);
    assert_eq!(
        read_fixture(scope, fixture, "drain"),
        "first|first microtask"
    );
    assert!(runtime.has_posted_tasks(scope));

    time.set(5.0);
    runtime.run_frame(scope, 4.5, 6.0, 10).unwrap();
    assert_eq!(
        read_fixture(scope, fixture, "drain"),
        "second|second microtask|timer one|timer microtask|timer two|frame one 4.5|frame microtask|frame two 4.5"
    );
    assert!(!runtime.has_posted_tasks(scope));
    assert_eq!(runtime.next_timer_deadline(scope), None);
    time.set(15.0);
    runtime.run_frame(scope, 15.0, 16.0, 10).unwrap();
    assert_eq!(read_fixture(scope, fixture, "drain"), "next frame 15");

    read_fixture(scope, fixture, "seedNesting");
    assert_eq!(runtime.run_ready(scope, 16.0, 10).unwrap(), 6);
    assert_eq!(read_fixture(scope, fixture, "drain"), "1|2|3|4|5|6");
    assert_eq!(runtime.next_timer_deadline(scope), Some(16.0));
    time.set(16.0);
    assert_eq!(runtime.run_ready(scope, 17.0, 10).unwrap(), 1);
    assert_eq!(read_fixture(scope, fixture, "drain"), "interval");
    assert_eq!(runtime.next_timer_deadline(scope), Some(19.0));
    time.set(19.0);
    assert_eq!(runtime.run_ready(scope, 20.0, 10).unwrap(), 1);
    assert_eq!(read_fixture(scope, fixture, "drain"), "7");
    assert_eq!(runtime.next_timer_deadline(scope), None);

    read_fixture(scope, fixture, "seedFailures");
    let error = runtime.run_frame(scope, 19.0, 20.0, 10).unwrap_err();
    assert!(error.to_string().contains("timer error"));
    assert_eq!(
        read_fixture(scope, fixture, "drain"),
        "throwing task microtask|timer after error|frame after error"
    );
}
