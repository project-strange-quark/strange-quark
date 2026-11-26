// SPDX-License-Identifier: MIT

use std::collections::HashMap;
use std::path::Path;
use std::sync::Once;

pub fn initialize_v8() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        v8::V8::set_flags_from_string("--expose-gc");
        v8::V8::initialize_platform(v8::new_default_platform(0, false).make_shared());
        v8::V8::initialize();
    });
}

type Modules = HashMap<String, v8::Global<v8::Module>>;

fn compile<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    source: &str,
) -> v8::Local<'s, v8::Module> {
    let name = v8::String::new(scope, name).unwrap();
    let source = v8::String::new(scope, source).unwrap();
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
    v8::script_compiler::compile_module(
        scope,
        &mut v8::script_compiler::Source::new(source, Some(&origin)),
    )
    .unwrap()
}

fn resolve<'s>(
    context: v8::Local<'s, v8::Context>,
    specifier: v8::Local<'s, v8::String>,
    _attributes: v8::Local<'s, v8::FixedArray>,
    _referrer: v8::Local<'s, v8::Module>,
) -> Option<v8::Local<'s, v8::Module>> {
    // SAFETY: V8 supplied this entered context to its module resolution callback.
    v8::callback_scope!(unsafe scope, context);
    let name = specifier.to_rust_string_lossy(scope);
    let dependency = scope.get_slot::<Modules>()?.get(&name)?.clone();
    Some(v8::Local::new(scope, dependency))
}

pub fn export<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    namespace: v8::Local<'s, v8::Object>,
    name: &str,
) -> v8::Local<'s, v8::Function> {
    let name = v8::String::new(scope, name).unwrap();
    v8::Local::try_from(namespace.get(scope, name.into()).unwrap()).unwrap()
}

pub fn load_dom_fixture<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    source: &str,
) -> v8::Local<'s, v8::Object> {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/@strange-quark/dom/dist");
    let definitions: Modules = ["index.js", "canvas.js", "canvas-path.js", "canvas-matrix.js", "canvas-stream.js", "image-bitmap.js", "fetch.js", "file-reader.js", "file-list.js", "form-data.js", "platform.js", "fullscreen.js", "activation.js", "clipboard.js", "fonts.js", "indexed-db.js", "web-crypto.js", "wasm.js", "network.js", "navigation.js", "scroll.js", "tree-walker.js", "stylesheet.js", "observers.js", "media.js", "input.js", "document.js", "inner-text.js", "journal.js", "collections.js", "events.js", "timing.js", "style.js", "css-namespace.js", "mutation.js", "abort.js"]
        .into_iter()
        .map(|name| {
            let path = dist.join(name);
            let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
                panic!("Cannot read {}: {error}. Run npm run build --workspace @strange-quark/dom from the repository root.", path.display());
            });
            let specifier = format!("./{name}");
            let module = compile(scope, &specifier, &source);
            (specifier, v8::Global::new(scope, module))
        })
        .collect();
    scope.set_slot(definitions);
    let fixture = compile(scope, name, source);
    assert_eq!(fixture.instantiate_module(scope, resolve), Some(true));
    let evaluated = v8::Local::<v8::Promise>::try_from(fixture.evaluate(scope).unwrap()).unwrap();
    assert_eq!(evaluated.state(), v8::PromiseState::Fulfilled);
    v8::Local::try_from(fixture.get_module_namespace()).unwrap()
}
