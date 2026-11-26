// SPDX-License-Identifier: MIT
#![allow(unsafe_code)]

use squark_types::journal::{RECORD_SIZE, Record, decode_all};
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Harness {
    entry_path: PathBuf,
    entry_id: Option<i32>,
    timer: Option<v8::Global<v8::Function>>,
    records: Option<Vec<Record>>,
}

fn compile<'s>(scope: &mut v8::PinScope<'s, '_>, path: &Path) -> Option<v8::Local<'s, v8::Module>> {
    let source = match std::fs::read_to_string(path) {
        Ok(source) => source,
        Err(error) => {
            throw(scope, &format!("{}: {error}", path.display()));
            return None;
        }
    };
    let code = v8::String::new(scope, &source)?;
    let name = v8::String::new(scope, path.to_str()?)?;
    let origin = v8::ScriptOrigin::new(
        scope,
        name.into(),
        0,
        0,
        false,
        -1,
        None,
        false,
        false,
        true,
        None,
    );
    let mut source = v8::script_compiler::Source::new(code, Some(&origin));
    v8::script_compiler::compile_module(scope, &mut source)
}

fn resolve<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    attributes: v8::Local<'s, v8::FixedArray>,
    referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    // SAFETY: V8 supplied this entered context to its module resolution callback.
    v8::callback_scope!(unsafe scope, context);
    let name = specifier.to_rust_string_lossy(scope);
    let harness = scope.get_slot::<Harness>()?;
    if name != "./records.mjs"
        || attributes.length() != 0
        || referrer.script_id() != harness.entry_id
    {
        throw(scope, &format!("Unsupported fixture import: {name}"));
        return None;
    }

    let path = harness.entry_path.with_file_name(&name[2..]);
    compile(scope, &path)
}

fn throw(scope: &mut v8::PinScope<'_, '_>, message: &str) {
    if let Some(message) = v8::String::new(scope, message) {
        let exception = v8::Exception::type_error(scope, message);
        scope.throw_exception(exception);
    }
}

fn schedule_timer(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut ret: v8::ReturnValue<v8::Value>,
) {
    let Ok(callback) = v8::Local::<v8::Function>::try_from(args.get(0)) else {
        throw(scope, "Expected a timer callback");
        return;
    };
    if args.get(1).uint32_value(scope) != Some(0)
        || scope.get_slot::<Harness>().unwrap().timer.is_some()
    {
        throw(
            scope,
            "The harness supports a single pending zero-delay timer",
        );
        return;
    }

    let timer = v8::Global::new(scope, callback);
    scope.get_slot_mut::<Harness>().unwrap().timer = Some(timer);
    ret.set_uint32(1);
}

fn decode_flush(bytes: &[u8]) -> Option<Vec<Record>> {
    let record_bytes = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
    let end = 16usize.checked_add(record_bytes)?;
    // This fixture uses only opcodes without string operands.
    let empty_strings_header = [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
    if bytes.get(4..16)? != empty_strings_header || bytes.get(end..)? != [0; 4] {
        return None;
    }
    decode_all(bytes.get(16..end)?).ok()
}

fn flush(
    scope: &mut v8::PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut ret: v8::ReturnValue<v8::Value>,
) {
    let Ok(buffer) = v8::Local::<v8::ArrayBuffer>::try_from(args.get(0)) else {
        throw(scope, "Expected a journal ArrayBuffer");
        return;
    };
    let Some(length) = args
        .get(1)
        .uint32_value(scope)
        .map(|length| length as usize)
    else {
        throw(scope, "Expected a journal length");
        return;
    };
    if length > buffer.byte_length() {
        throw(scope, "Journal length exceeds the buffer");
        return;
    }

    let backing = buffer.get_backing_store();
    let bytes: Vec<_> = backing[..length].iter().map(std::cell::Cell::get).collect();
    let Some(records) = decode_flush(&bytes) else {
        throw(scope, "Invalid journal in the records fixture");
        return;
    };
    ret.set_uint32(records.len() as u32);
    scope.get_slot_mut::<Harness>().unwrap().records = Some(records);
}

#[test]
fn disk_modules_microtasks_and_timers_can_flush_the_journal() {
    let platform = v8::new_default_platform(0, false).make_shared();
    v8::V8::initialize_platform(platform);
    v8::V8::initialize();

    let isolate = &mut v8::Isolate::new(Default::default());
    isolate.set_microtasks_policy(v8::MicrotasksPolicy::Explicit);
    let entry_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/entry.mjs");
    isolate.set_slot(Harness {
        entry_path: entry_path.clone(),
        ..Default::default()
    });

    v8::scope!(let scope, isolate);
    let context = v8::Context::new(scope, Default::default());
    let scope = &mut v8::ContextScope::new(scope, context);
    let global = context.global(scope);
    let squark = v8::Object::new(scope);
    let squark_name = v8::String::new(scope, "__squark").unwrap();
    let flush_name = v8::String::new(scope, "flush").unwrap();
    let flush_callback = v8::Function::new(scope, flush).unwrap();
    assert_eq!(
        squark.set(scope, flush_name.into(), flush_callback.into()),
        Some(true)
    );
    assert_eq!(
        global.set(scope, squark_name.into(), squark.into()),
        Some(true)
    );

    let timer_name = v8::String::new(scope, "setTimeout").unwrap();
    let timer_callback = v8::Function::new(scope, schedule_timer).unwrap();
    assert_eq!(
        global.set(scope, timer_name.into(), timer_callback.into()),
        Some(true)
    );

    let entry = compile(scope, &entry_path).expect("compile the module from disk");
    scope.get_slot_mut::<Harness>().unwrap().entry_id = entry.script_id();
    assert_eq!(entry.instantiate_module(scope, resolve), Some(true));

    let evaluated = entry.evaluate(scope).expect("evaluate the entry module");
    let evaluated = v8::Local::<v8::Promise>::try_from(evaluated).unwrap();
    assert_eq!(evaluated.state(), v8::PromiseState::Fulfilled);
    let timer = scope
        .get_slot_mut::<Harness>()
        .unwrap()
        .timer
        .take()
        .expect("timer queued");
    assert!(scope.get_slot::<Harness>().unwrap().records.is_none());

    scope.perform_microtask_checkpoint();
    assert!(scope.get_slot::<Harness>().unwrap().records.is_none());

    let timer = v8::Local::new(scope, timer);
    let receiver = v8::undefined(scope);
    v8::tc_scope!(let scope, scope);
    if timer.call(scope, receiver.into(), &[]).is_none() {
        let exception = scope.exception().expect("timer callback threw");
        panic!("{}", exception.to_rust_string_lossy(scope));
    }
    scope.perform_microtask_checkpoint();

    let records = scope
        .get_slot::<Harness>()
        .unwrap()
        .records
        .as_ref()
        .expect("journal flushed");
    assert_eq!(records.len() * RECORD_SIZE, 48);
    let decoded: Vec<_> = records
        .iter()
        .map(|record| (record.op, record.flags, record.a, record.b, record.c))
        .collect();
    use squark_types::journal::Opcode::{CreateElement, Insert};
    assert_eq!(
        decoded,
        [
            (CreateElement, 0, 1, 1, 2),
            (CreateElement, 0, 2, 3, 2),
            (Insert, 0, 1, 2, 0)
        ]
    );
}
