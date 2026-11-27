// SPDX-License-Identifier: MIT

//! One ES module registry per document, including the private squark-dom prelude.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};

use percent_encoding::percent_decode_str;
use squark_types::pack::{ScriptKind, is_valid_pack_path};
use url::Url;
use v8::{Local, PinScope};

use crate::module_cache;

pub(crate) const RUNTIME_ENTRY: &str = "squark-runtime:///index.js";
pub(crate) const RUNTIME_JOURNAL: &str = "squark-runtime:///journal.js";
pub(crate) const RUNTIME_WORKER: &str = "squark-runtime:///worker-global.js";

type PackLookup = dyn Fn(&str) -> Option<(String, Vec<u8>)> + Send + Sync;
type RuntimeLookup = dyn Fn(&str) -> Option<String> + Send + Sync;

/// The composition root adapts Pack::entry and the embedded, compiled DOM
/// modules to these lookups. Keys are absolute canonical paths. Workers read
/// the same lookups from their own threads.
pub struct ScriptSources {
    pack: Arc<PackLookup>,
    runtime: Arc<RuntimeLookup>,
    pub(crate) dev: bool,
    module_cache: bool,
    pub(crate) blobs: crate::BlobRegistry,
    pub(crate) canvas: Option<Box<dyn crate::CanvasBackend>>,
    pub(crate) host_requests: Option<std::sync::mpsc::Sender<squark_types::host::HostRequest>>,
    pub(crate) storage_root: Option<std::path::PathBuf>,
    pub(crate) shutdown: crate::DocumentShutdown,
    pub(crate) native: Option<std::sync::Arc<dyn squark_types::native::NativeDispatcher>>,
}

impl ScriptSources {
    pub fn new(
        pack: impl Fn(&str) -> Option<(String, Vec<u8>)> + Send + Sync + 'static,
        runtime: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            pack: Arc::new(pack),
            runtime: Arc::new(runtime),
            dev: false,
            module_cache: true,
            blobs: Default::default(),
            canvas: None,
            host_requests: None,
            storage_root: None,
            shutdown: Default::default(),
            native: None,
        }
    }

    /// Expose the app runtime's native modules to the document's JavaScript.
    pub fn with_native(
        mut self,
        dispatcher: std::sync::Arc<dyn squark_types::native::NativeDispatcher>,
    ) -> Self {
        self.native = Some(dispatcher);
        self
    }
    /// Allow connections to the document's exact HTTP/WS development origin.
    pub fn with_dev_server(mut self) -> Self {
        self.dev = true;
        self
    }

    /// Route clipboard and dialog requests to the host. Return results as
    /// DocumentTask::HostReply on this document's queue.
    pub fn with_host_requests(
        mut self,
        sender: std::sync::mpsc::Sender<squark_types::host::HostRequest>,
    ) -> Self {
        self.host_requests = Some(sender);
        self
    }

    /// Override the per-user storage base, for isolated document hosts and tests.
    pub fn with_storage_root(mut self, root: std::path::PathBuf) -> Self {
        self.storage_root = Some(root);
        self
    }

    /// Share the document's object URLs with a host asset resolver.
    pub fn with_blob_registry(mut self, blobs: crate::BlobRegistry) -> Self {
        self.blobs = blobs;
        self
    }

    /// Attach the canvas services supplied by the document host.
    pub fn with_canvas_backend(mut self, backend: impl crate::CanvasBackend + 'static) -> Self {
        self.canvas = Some(Box::new(backend));
        self
    }

    /// Share the document's stop signal with the host that owns its services.
    pub fn with_shutdown(mut self, shutdown: crate::DocumentShutdown) -> Self {
        self.shutdown = shutdown;
        self
    }

    pub(crate) fn entry(&self, path: &str) -> Option<(String, Vec<u8>)> {
        (self.pack)(path)
    }

    /// The sources a worker thread loads its scripts from: the pack, the
    /// runtime modules, the dev server and the document's object URLs.
    pub(crate) fn share(&self) -> SharedSources {
        SharedSources {
            pack: Arc::clone(&self.pack),
            runtime: Arc::clone(&self.runtime),
            dev: self.dev,
            blobs: self.blobs.clone(),
        }
    }
}

/// The thread-safe part of a document's ScriptSources.
#[derive(Clone)]
pub(crate) struct SharedSources {
    pack: Arc<PackLookup>,
    runtime: Arc<RuntimeLookup>,
    dev: bool,
    blobs: crate::BlobRegistry,
}

impl From<SharedSources> for ScriptSources {
    /// A worker compiles its modules without the document's code cache.
    fn from(shared: SharedSources) -> Self {
        Self {
            pack: shared.pack,
            runtime: shared.runtime,
            dev: shared.dev,
            module_cache: false,
            blobs: shared.blobs,
            canvas: None,
            host_requests: None,
            storage_root: None,
            shutdown: Default::default(),
            native: None,
        }
    }
}

impl std::fmt::Debug for ScriptSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptSources").finish_non_exhaustive()
    }
}

struct Entry {
    base: Url,
    module: v8::Global<v8::Module>,
    evaluation: Option<v8::Global<v8::Promise>>,
}

struct DevImport {
    root: Url,
    frontier: VecDeque<Url>,
    seen: HashSet<Url>,
    resolver: v8::Global<v8::PromiseResolver>,
}

struct DevModules {
    fetch: v8::Global<v8::Function>,
    sources: HashMap<Url, v8::Global<v8::Promise>>,
    pending: Vec<DevImport>,
}

pub(crate) struct Loader {
    sources: ScriptSources,
    document: Url,
    modules: HashMap<String, Entry>,
    scripts: HashMap<i32, String>,
    imports: VecDeque<(Url, v8::Global<v8::PromiseResolver>)>,
    dev: Option<DevModules>,
    cache_root: Option<PathBuf>,
    cache_misses: Option<Vec<(PathBuf, v8::Global<v8::Module>)>>,
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, document: Url, sources: ScriptSources) {
    let cache_root = if sources.dev || !sources.module_cache {
        None
    } else {
        module_cache::root(sources.storage_root.as_deref())
    };
    scope.set_slot(Loader {
        document,
        sources,
        modules: HashMap::new(),
        scripts: HashMap::new(),
        imports: VecDeque::new(),
        dev: None,
        cache_root,
        cache_misses: Some(Vec::new()),
    });
    scope.set_host_import_module_dynamically_callback(dynamic_import);
    scope.set_host_initialize_import_meta_object_callback(import_meta);
}

pub(crate) fn install_dev_fetch(scope: &mut PinScope<'_, '_>, fetch: Local<'_, v8::Function>) {
    if registry(scope).sources.dev {
        let fetch = v8::Global::new(scope, fetch);
        scope.get_slot_mut::<Loader>().unwrap().dev = Some(DevModules {
            fetch,
            sources: HashMap::new(),
            pending: Vec::new(),
        });
    }
}

pub(crate) fn pack_entry(scope: &mut PinScope<'_, '_>, path: &str) -> Option<(String, Vec<u8>)> {
    registry(scope).sources.entry(path)
}

fn registry<'a>(scope: &'a PinScope<'_, '_>) -> &'a Loader {
    scope.get_slot::<Loader>().expect("module loader installed")
}

pub(crate) fn pack_path(url: &Url) -> Result<String, String> {
    let path = url.path();
    if path
        .as_bytes()
        .windows(3)
        .any(|part| part.eq_ignore_ascii_case(b"%2f") || part.eq_ignore_ascii_case(b"%5c"))
    {
        return Err(format!("encoded separator in script URL {url}"));
    }
    let path = percent_decode_str(path)
        .decode_utf8()
        .map_err(|_| format!("invalid UTF-8 in script URL {url}"))?;
    if !is_valid_pack_path(&path) {
        return Err(format!("invalid script path {path}"));
    }
    Ok(path.into_owned())
}

fn resolve_url(base: &Url, specifier: &str) -> Result<Url, String> {
    if !(specifier.starts_with(['/', '?', '#'])
        || specifier.starts_with("./")
        || specifier.starts_with("../")
        || Url::parse(specifier).is_ok())
    {
        return Err(format!("unresolved bare specifier {specifier:?}"));
    }
    let url = base
        .join(specifier)
        .map_err(|error| format!("invalid module URL {specifier:?}: {error}"))?;
    let allowed = if base.scheme() == "squark-runtime" {
        url.scheme() == "squark-runtime" && url.host_str().is_none()
    } else {
        url.origin() == base.origin()
            && matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
    };
    if !allowed {
        return Err(format!("module URL is outside its pack: {url}"));
    }
    Ok(url)
}

pub(crate) fn stylesheet_url(scope: &PinScope<'_, '_>, path: &str) -> Option<String> {
    let loader = registry(scope);
    let url = resolve_url(&loader.document, path).ok()?;
    let (mime, _) = (loader.sources.pack)(&pack_path(&url).ok()?)?;
    mime.split(';')
        .next()?
        .trim()
        .eq_ignore_ascii_case("text/css")
        .then(|| url.into())
}

pub(crate) fn is_javascript(mime: &str) -> bool {
    let essence = mime.split(';').next().unwrap_or_default().trim();
    matches!(
        essence.to_ascii_lowercase().as_str(),
        "text/javascript" | "application/javascript" | "text/ecmascript" | "application/ecmascript"
    )
}

fn source_text(loader: &Loader, url: &Url) -> Result<Option<String>, String> {
    // Worker entries can be object URLs or data: URLs. A module's imports
    // still resolve only to its own origin's HTTP(S) URLs.
    match url.scheme() {
        "blob" => {
            return loader
                .sources
                .blobs
                .resolve(url.as_str())
                .map(|blob| Some(String::from_utf8_lossy(&blob.bytes).into_owned()))
                .ok_or_else(|| format!("missing blob script {url}"));
        }
        "data" => {
            return data_url::DataUrl::process(url.as_str())
                .ok()
                .and_then(|data| data.decode_to_vec().ok())
                .map(|(bytes, _)| Some(String::from_utf8_lossy(&bytes).into_owned()))
                .ok_or_else(|| format!("invalid data script {url}"));
        }
        _ => {}
    }
    let path = pack_path(url)?;
    if url.scheme() == "squark-runtime" {
        return (loader.sources.runtime)(&path)
            .map(Some)
            .ok_or_else(|| format!("missing squark-dom runtime module {path}"));
    }
    let Some((mime, bytes)) = (loader.sources.pack)(&path) else {
        return Ok(None);
    };
    if !is_javascript(&mime) {
        return Err(format!("packed script {path} has MIME {mime}"));
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    Ok(Some(String::from_utf8_lossy(bytes).into_owned()))
}

fn origin<'s>(
    scope: &mut PinScope<'s, '_>,
    name: &str,
    module: bool,
) -> Option<v8::ScriptOrigin<'s>> {
    let name = v8::String::new(scope, name)?;
    Some(v8::ScriptOrigin::new(
        scope,
        name.into(),
        0,
        0,
        false,
        -1,
        None,
        false,
        false,
        module,
        None,
    ))
}

fn compile<'s>(
    scope: &mut PinScope<'s, '_>,
    key: &str,
    base: Url,
    code: Local<'s, v8::String>,
    cache_path: Option<PathBuf>,
) -> Option<Local<'s, v8::Module>> {
    use v8::script_compiler::{self, CachedData, CompileOptions, NoCacheReason, Source};

    let origin = origin(scope, base.as_str(), true)?;
    let cached = cache_path.as_deref().and_then(module_cache::read);
    let (module, cache_hit) = if let Some(bytes) = &cached {
        let mut source = Source::new_with_cached_data(code, Some(&origin), CachedData::new(bytes));
        let module = script_compiler::compile_module2(
            scope,
            &mut source,
            CompileOptions::ConsumeCodeCache,
            NoCacheReason::NoReason,
        )?;
        (
            module,
            source
                .get_cached_data()
                .is_some_and(|data| !data.rejected()),
        )
    } else {
        let mut source = Source::new(code, Some(&origin));
        (script_compiler::compile_module(scope, &mut source)?, false)
    };
    let global = v8::Global::new(scope, module);
    let loader = scope.get_slot_mut::<Loader>().unwrap();
    if !cache_hit
        && let Some(path) = cache_path
        && let Some(misses) = &mut loader.cache_misses
    {
        misses.push((path, global.clone()));
    }
    loader.scripts.insert(module.script_id()?, key.into());
    loader.modules.insert(
        key.into(),
        Entry {
            base,
            module: global,
            evaluation: None,
        },
    );
    Some(module)
}

pub(crate) fn persist_startup_cache(scope: &mut PinScope<'_, '_>) {
    let misses = scope
        .get_slot_mut::<Loader>()
        .unwrap()
        .cache_misses
        .take()
        .unwrap_or_default();
    let entries = misses
        .into_iter()
        .filter_map(|(path, module)| {
            let module = Local::new(scope, module);
            if module.get_status() != v8::ModuleStatus::Evaluated {
                return None;
            }
            let bytes = module
                .get_unbound_module_script(scope)
                .create_code_cache()?
                .to_vec();
            Some((path, bytes))
        })
        .collect();
    module_cache::write_entries(entries);
}

pub(crate) fn load<'s>(scope: &mut PinScope<'s, '_>, url: Url) -> Option<Local<'s, v8::Module>> {
    let key = url.as_str().to_owned();
    if let Some(entry) = registry(scope).modules.get(&key) {
        let module = entry.module.clone();
        return Some(Local::new(scope, module));
    }
    let text = match source_text(registry(scope), &url) {
        Ok(Some(code)) => code,
        Ok(None) => return throw(scope, &format!("missing packed script {url}")),
        Err(error) => return throw(scope, &error),
    };
    let cache_path = match url.scheme() {
        "http" | "https" => registry(scope)
            .cache_root
            .as_deref()
            .and_then(|root| module_cache::entry(root, &key, &text)),
        _ => None,
    };
    let code = v8::String::new(scope, &text)?;
    compile(scope, &key, url, code, cache_path)
}

pub(crate) fn evaluate<'s>(
    scope: &mut PinScope<'s, '_>,
    module: Local<'s, v8::Module>,
) -> Option<Local<'s, v8::Promise>> {
    if module.get_status() == v8::ModuleStatus::Errored {
        scope.throw_exception(module.get_exception());
        return None;
    }
    let key = registry(scope).scripts.get(&module.script_id()?)?.clone();
    if let Some(existing) = registry(scope).modules[&key].evaluation.clone() {
        return Some(Local::new(scope, existing));
    }
    if module.get_status() == v8::ModuleStatus::Uninstantiated
        && module.instantiate_module(scope, static_import) != Some(true)
    {
        return None;
    }
    let promise = Local::<v8::Promise>::try_from(module.evaluate(scope)?).ok()?;
    let global = v8::Global::new(scope, promise);
    scope
        .get_slot_mut::<Loader>()
        .unwrap()
        .modules
        .get_mut(&key)?
        .evaluation = Some(global);
    Some(promise)
}

fn static_import<'s>(
    context: Local<'s, v8::Context>,
    specifier: Local<'s, v8::String>,
    attributes: Local<'s, v8::FixedArray>,
    referrer: Local<'s, v8::Module>,
) -> Option<Local<'s, v8::Module>> {
    // SAFETY: V8 invokes the resolver with this entered context.
    v8::callback_scope!(unsafe scope, context);
    if attributes.length() != 0 {
        return throw(
            scope,
            "packed JavaScript modules do not accept import attributes",
        );
    }
    let specifier = specifier.to_rust_string_lossy(scope);
    let loader = registry(scope);
    let key = loader.scripts.get(&referrer.script_id()?)?;
    match resolve_url(&loader.modules[key].base, &specifier) {
        Ok(url) => load(scope, url),
        Err(error) => throw(scope, &error),
    }
}

extern "C" fn import_meta(
    context: Local<v8::Context>,
    module: Local<v8::Module>,
    meta: Local<v8::Object>,
) {
    // SAFETY: V8 invokes the callback with this entered context.
    v8::callback_scope!(unsafe scope, context);
    let loader = registry(scope);
    if let Some(key) = module.script_id().and_then(|id| loader.scripts.get(&id)) {
        let url = loader.modules[key].base.as_str().to_owned();
        if let (Some(key), Some(value)) =
            (v8::String::new(scope, "url"), v8::String::new(scope, &url))
        {
            meta.create_data_property(scope, key.into(), value.into());
        }
    }
}

fn dynamic_import<'s>(
    scope: &mut PinScope<'s, '_>,
    _: Local<'s, v8::Data>,
    name: Local<'s, v8::Value>,
    specifier: Local<'s, v8::String>,
    attributes: Local<'s, v8::FixedArray>,
) -> Option<Local<'s, v8::Promise>> {
    let name = name.to_rust_string_lossy(scope);
    let specifier = specifier.to_rust_string_lossy(scope);
    let loader = registry(scope);
    let base = loader
        .modules
        .get(&name)
        .map(|entry| entry.base.clone())
        .or_else(|| Url::parse(&name).ok())
        .unwrap_or_else(|| loader.document.clone());
    let url = if attributes.length() == 0 {
        resolve_url(&base, &specifier)
    } else {
        Err("packed JavaScript modules do not accept import attributes".into())
    };
    let resolver = v8::PromiseResolver::new(scope)?;
    let promise = resolver.get_promise(scope);
    match url {
        Ok(url) => {
            // Evaluate after the active module has returned and its promise is registered.
            let callback = v8::Function::new(scope, run_import)?;
            let resolver = v8::Global::new(scope, resolver);
            scope
                .get_slot_mut::<Loader>()?
                .imports
                .push_back((url, resolver));
            scope.enqueue_microtask(callback);
        }
        Err(error) => {
            let error = v8::String::new(scope, &error)?;
            let exception = v8::Exception::type_error(scope, error);
            resolver.reject(scope, exception)?;
        }
    }
    Some(promise)
}

fn run_import(scope: &mut PinScope<'_, '_>, _: v8::FunctionCallbackArguments, _: v8::ReturnValue) {
    let (url, resolver) = scope
        .get_slot_mut::<Loader>()
        .unwrap()
        .imports
        .pop_front()
        .unwrap();
    let resolver = Local::new(scope, resolver);
    if registry(scope).dev.is_some() && url.scheme() != "squark-runtime" {
        queue_dev_import(scope, url, resolver);
        return;
    }
    let result = import_evaluation(scope, url);
    if let Some(result) = result {
        match result {
            Ok(promise) => {
                resolver.resolve(scope, promise.into());
            }
            Err(exception) => {
                resolver.reject(scope, exception);
            }
        }
    }
}

fn import_evaluation<'s>(
    scope: &mut PinScope<'s, '_>,
    url: Url,
) -> Option<Result<Local<'s, v8::Promise>, Local<'s, v8::Value>>> {
    v8::tc_scope!(let scope, scope);
    let loaded = load(scope, url).and_then(|module| evaluate_namespace(scope, module));
    match loaded {
        Some(promise) => Some(Ok(promise)),
        None => scope.exception().map(Err),
    }
}

fn evaluate_namespace<'s>(
    scope: &mut PinScope<'s, '_>,
    module: Local<'s, v8::Module>,
) -> Option<Local<'s, v8::Promise>> {
    let promise = evaluate(scope, module)?;
    let callback = v8::Function::builder(module_namespace)
        .data(module.get_module_namespace())
        .build(scope)?;
    promise.then(scope, callback)
}

fn queue_dev_import(scope: &mut PinScope<'_, '_>, root: Url, resolver: Local<v8::PromiseResolver>) {
    let resolver = v8::Global::new(scope, resolver);
    scope
        .get_slot_mut::<Loader>()
        .unwrap()
        .dev
        .as_mut()
        .unwrap()
        .pending
        .push(DevImport {
            frontier: VecDeque::from([root.clone()]),
            seen: HashSet::from([root.clone()]),
            root,
            resolver,
        });
    advance_dev(scope);
}

/// Drive module graphs after the network and its promise reactions have run.
/// V8's synchronous linker only sees the graph after it has been compiled.
pub(crate) fn advance_dev(scope: &mut PinScope<'_, '_>) -> bool {
    let Some(dev) = scope
        .get_slot_mut::<Loader>()
        .and_then(|loader| loader.dev.as_mut())
    else {
        return false;
    };
    let mut completed = false;
    for mut job in std::mem::take(&mut dev.pending) {
        v8::tc_scope!(let scope, scope);
        let result = match prefetch(scope, &mut job) {
            Some(false) => {
                scope
                    .get_slot_mut::<Loader>()
                    .unwrap()
                    .dev
                    .as_mut()
                    .unwrap()
                    .pending
                    .push(job);
                continue;
            }
            Some(true) => {
                load(scope, job.root).and_then(|module| evaluate_namespace(scope, module))
            }
            None => None,
        };
        let resolver = Local::new(scope, job.resolver);
        if let Some(promise) = result {
            resolver.resolve(scope, promise.into());
        } else if let Some(error) = scope.exception().or_else(|| {
            let message = v8::String::new(scope, "could not prepare development module")?;
            Some(v8::Exception::type_error(scope, message))
        }) {
            resolver.reject(scope, error);
        }
        completed = true;
    }
    completed
}

fn prefetch(scope: &mut PinScope<'_, '_>, job: &mut DevImport) -> Option<bool> {
    let mut waiting = VecDeque::new();
    while let Some(url) = job.frontier.pop_front() {
        let Some(module) = dev_module(scope, &url)? else {
            waiting.push_back(url);
            continue;
        };
        let base = registry(scope).modules[url.as_str()].base.clone();
        let requests = module.get_module_requests();
        for index in 0..requests.length() {
            let request = Local::<v8::ModuleRequest>::try_from(requests.get(scope, index)?).ok()?;
            if request.get_import_attributes().length() != 0 {
                return throw(scope, "JavaScript modules do not accept import attributes");
            }
            let specifier = request.get_specifier().to_rust_string_lossy(scope);
            let dependency = match resolve_url(&base, &specifier) {
                Ok(url) => url,
                Err(error) => return throw(scope, &error),
            };
            if job.seen.insert(dependency.clone()) {
                job.frontier.push_back(dependency);
            }
        }
    }
    job.frontier = waiting;
    Some(job.frontier.is_empty())
}

fn dev_module<'s>(
    scope: &mut PinScope<'s, '_>,
    url: &Url,
) -> Option<Option<Local<'s, v8::Module>>> {
    let key = url.as_str();
    let loader = registry(scope);
    if let Some(entry) = loader.modules.get(key) {
        return Some(Some(Local::new(scope, &entry.module)));
    }
    // The archive has path keys. A Vite query selects a separate revision.
    let packed = if url.query().is_some() {
        Ok(None)
    } else {
        source_text(loader, url)
    };
    let code = match packed {
        Ok(Some(text)) => v8::String::new(scope, &text)?,
        Ok(None) => {
            if url.origin() != loader.document.origin() {
                return throw(
                    scope,
                    &format!("module URL is outside the development origin: {url}"),
                );
            }
            let Some(code) = fetched_code(scope, url)? else {
                return Some(None);
            };
            code
        }
        Err(error) => return throw(scope, &error),
    };
    let module = compile(scope, key, url.clone(), code, None)?;
    scope
        .get_slot_mut::<Loader>()
        .unwrap()
        .dev
        .as_mut()
        .unwrap()
        .sources
        .remove(url);
    Some(Some(module))
}

fn fetched_code<'s>(
    scope: &mut PinScope<'s, '_>,
    url: &Url,
) -> Option<Option<Local<'s, v8::String>>> {
    let dev = registry(scope).dev.as_ref().unwrap();
    let promise = if let Some(source) = dev.sources.get(url) {
        Local::new(scope, source)
    } else {
        let fetch = Local::new(scope, &dev.fetch);
        let address = v8::String::new(scope, url.as_str())?;
        let receiver = v8::undefined(scope);
        let value = fetch.call(scope, receiver.into(), &[address.into()])?;
        let promise = Local::<v8::Promise>::try_from(value).ok()?;
        promise.mark_as_handled();
        let stored = v8::Global::new(scope, promise);
        scope
            .get_slot_mut::<Loader>()
            .unwrap()
            .dev
            .as_mut()
            .unwrap()
            .sources
            .insert(url.clone(), stored);
        promise
    };
    match promise.state() {
        v8::PromiseState::Pending => Some(None),
        v8::PromiseState::Rejected => {
            let error = promise.result(scope);
            scope.throw_exception(error);
            None
        }
        v8::PromiseState::Fulfilled => {
            let response = Local::<v8::Array>::try_from(promise.result(scope)).ok()?;
            let mime = response.get_index(scope, 0)?.to_rust_string_lossy(scope);
            if !is_javascript(&mime) {
                return throw(scope, &format!("development module {url} has MIME {mime}"));
            }
            Some(Some(
                Local::<v8::String>::try_from(response.get_index(scope, 1)?).ok()?,
            ))
        }
    }
}

fn module_namespace(
    _: &mut PinScope<'_, '_>,
    args: v8::FunctionCallbackArguments,
    mut result: v8::ReturnValue,
) {
    result.set(args.data());
}

pub(crate) fn has_packed_script(scope: &PinScope<'_, '_>, payload: &[u16]) -> bool {
    let loader = registry(scope);
    resolve_url(&loader.document, &String::from_utf16_lossy(payload))
        .ok()
        .and_then(|url| pack_path(&url).ok())
        .is_some_and(|path| loader.sources.entry(&path).is_some())
}

pub(crate) fn can_fetch_dev_script(scope: &PinScope<'_, '_>, payload: &[u16]) -> bool {
    let loader = registry(scope);
    loader.dev.is_some()
        && resolve_url(&loader.document, &String::from_utf16_lossy(payload)).is_ok()
}

/// Whether the module at `url` started evaluating, which tells a module
/// worker's evaluation errors from its fetch, parse and link failures.
pub(crate) fn module_evaluated(scope: &PinScope<'_, '_>, url: &Url) -> bool {
    registry(scope)
        .modules
        .get(url.as_str())
        .is_some_and(|entry| entry.evaluation.is_some())
}

pub(crate) fn evaluate_entry<'s>(
    scope: &mut PinScope<'s, '_>,
    url: Url,
) -> Option<Local<'s, v8::Promise>> {
    if registry(scope).dev.is_some() {
        let resolver = v8::PromiseResolver::new(scope)?;
        let promise = resolver.get_promise(scope);
        queue_dev_import(scope, url, resolver);
        Some(promise)
    } else {
        let module = load(scope, url)?;
        evaluate(scope, module)
    }
}

pub(crate) fn execute<'s>(
    scope: &mut PinScope<'s, '_>,
    kind: ScriptKind,
    node: u32,
    payload: &[u16],
    base_url: &str,
) -> Option<Option<Local<'s, v8::Promise>>> {
    let document = registry(scope).document.clone();
    let base = match document.join(base_url) {
        Ok(url) => url,
        Err(error) => return throw(scope, &format!("invalid script base URL: {error}")),
    };
    match kind {
        ScriptKind::ExternalModule | ScriptKind::ExternalClassic => {
            let specifier = String::from_utf16_lossy(payload);
            let url = match resolve_url(&document, &specifier) {
                Ok(url) => url,
                Err(error) => return throw(scope, &error),
            };
            if kind == ScriptKind::ExternalModule {
                Some(Some(evaluate_entry(scope, url)?))
            } else {
                let source = match source_text(registry(scope), &url) {
                    Ok(Some(code)) => v8::String::new(scope, &code)?,
                    Ok(None) => return throw(scope, &format!("missing packed script {url}")),
                    Err(error) => return throw(scope, &error),
                };
                classic(scope, source, url.as_str())?;
                Some(None)
            }
        }
        ScriptKind::InlineModule | ScriptKind::InlineClassic => {
            let source = v8::String::new_from_two_byte(scope, payload, v8::NewStringType::Normal)?;
            if kind == ScriptKind::InlineModule {
                let key = Url::parse(&format!("squark-inline:{node}")).ok()?;
                compile(scope, key.as_str(), base, source, None)?;
                Some(Some(evaluate_entry(scope, key)?))
            } else {
                classic(scope, source, base.as_str())?;
                Some(None)
            }
        }
        ScriptKind::ExternalStylesheet => Some(None),
    }
}

pub(crate) fn classic<'s>(
    scope: &mut PinScope<'s, '_>,
    code: Local<'s, v8::String>,
    name: &str,
) -> Option<Local<'s, v8::Value>> {
    let origin = origin(scope, name, false)?;
    v8::Script::compile(scope, code, Some(&origin))?.run(scope)
}

fn throw<T>(scope: &mut PinScope<'_, '_>, error: &str) -> Option<T> {
    if let Some(error) = v8::String::new(scope, error) {
        let exception = v8::Exception::type_error(scope, error);
        scope.throw_exception(exception);
    }
    None
}
