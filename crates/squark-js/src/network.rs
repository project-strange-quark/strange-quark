// SPDX-License-Identifier: MIT

use std::{
    collections::{HashMap, VecDeque},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

use serde::Deserialize;
use squark_net::{Allowlist, Data, Event, Headers, Redirect, Request, Response, Transport};
use url::Url;
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue, Value};

use crate::{
    DocumentShutdown,
    bindings::{array, set, string, two_byte, unsigned, utf16},
    loader::{self, ScriptSources},
    shutdown::STORAGE_CLOSE_BUDGET,
    storage::{Change, Storage},
};

#[derive(Default, Deserialize)]
#[serde(default)]
struct Manifest {
    app_name: Option<String>,
    entry_html: Option<String>,
    network: NetworkConfig,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct NetworkConfig {
    allow: Vec<String>,
}

struct Network {
    document: Url,
    dev: bool,
    entry_html: Option<String>,
    policy: Allowlist,
    transport: Transport,
    incoming: Receiver<(u32, Event)>,
    direct_wakeup: bool,
    local: VecDeque<(u32, Event)>,
    local_bodies: HashMap<u32, std::io::Cursor<Vec<u8>>>,
    storage: Option<Storage>,
    shutdown: DocumentShutdown,
}

/// Installs fetch and WebSocket bindings. A worker passes `storage: false`:
/// it has no localStorage and must not open the app's storage files.
pub(crate) fn install(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    document: Url,
    sources: &ScriptSources,
    storage: bool,
) -> Option<()> {
    let manifest = sources
        .entry(squark_types::pack::MANIFEST_PATH)
        .and_then(|(_, bytes)| serde_json::from_slice::<Manifest>(&bytes).ok())
        .unwrap_or_default();
    let policy = Allowlist::new(
        &manifest.network.allow,
        sources.dev.then(|| document.clone()),
    );
    let (sender, incoming) = mpsc::channel();
    let storage = manifest
        .app_name
        .as_deref()
        .filter(|_| storage)
        .and_then(|app| Storage::new(app, sources.storage_root.as_deref()));
    scope.set_slot(Network {
        document,
        dev: sources.dev,
        entry_html: manifest.entry_html,
        policy: policy.clone(),
        transport: Transport::new(policy, sender),
        incoming,
        direct_wakeup: false,
        local: VecDeque::new(),
        local_bodies: HashMap::new(),
        storage,
        shutdown: sources.shutdown.clone(),
    });
    macro_rules! binding {
        ($name:literal, $handler:ident) => {
            let function = Function::new(scope, $handler)?;
            set(scope, host, $name, function.into())?;
        };
    }
    binding!("fetchStart", fetch_start);
    binding!("fetchCredit", fetch_credit);
    binding!("fetchCancel", cancel);
    binding!("socketOpen", socket_open);
    binding!("socketSend", socket_send);
    binding!("socketClose", socket_close);
    binding!("utf8Encode", utf8_encode);
    binding!("utf8Decode", utf8_decode);
    binding!("runClassicScript", run_classic);
    binding!("idbRead", idb_read);
    binding!("idbCommit", idb_commit);
    binding!("idbShutdownTimeRemaining", idb_shutdown_time_remaining);
    binding!("storageEntries", storage_entries);
    binding!("storageCommit", storage_commit);
    binding!("storageFlush", storage_flush);
    Some(())
}

fn network<'a>(scope: &'a mut PinScope<'_, '_>) -> &'a mut Network {
    scope.get_slot_mut().expect("network bindings installed")
}

/// Whether a script at `url` comes from the pack rather than the network.
pub(crate) fn is_packed(scope: &mut PinScope<'_, '_>, url: &Url) -> bool {
    let net = network(scope);
    !net.dev && url.origin() == net.document.origin()
}

/// GETs a whole response on the calling thread, which a worker's
/// importScripts needs. `cancelled` is polled while the request runs.
pub(crate) fn fetch_blocking(
    scope: &mut PinScope<'_, '_>,
    url: Url,
    cancelled: &dyn Fn() -> bool,
) -> Option<(Response, Vec<u8>)> {
    const CREDIT: usize = 1024 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(60);
    let (sender, receiver) = mpsc::channel();
    let mut transport = Transport::new(network(scope).policy.clone(), sender);
    transport.fetch(
        0,
        Request {
            url,
            method: "GET".into(),
            headers: Vec::new(),
            body: None,
            redirect: Redirect::Follow,
        },
    );
    let deadline = Instant::now() + TIMEOUT;
    let mut response = None;
    let mut body = Vec::new();
    while Instant::now() < deadline && !cancelled() {
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok((_, Event::Headers(head))) => {
                response = Some(head);
                transport.credit(0, CREDIT);
            }
            Ok((_, Event::Chunk(bytes))) => {
                body.extend_from_slice(&bytes);
                transport.credit(0, CREDIT);
            }
            Ok((_, Event::End)) => return response.map(|response| (response, body)),
            Ok(_) | Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    None
}

pub(crate) fn set_waker(scope: &mut PinScope<'_, '_>, wake: impl Fn() + Send + Sync + 'static) {
    let net = network(scope);
    net.transport.set_waker(wake);
    net.direct_wakeup = true;
}

pub(crate) fn wakeup(scope: &mut PinScope<'_, '_>) -> Option<Duration> {
    let net = network(scope);
    if net.local.is_empty()
        && let Ok(event) = net.incoming.try_recv()
    {
        net.local.push_back(event);
    }
    if !net.local.is_empty() {
        Some(Duration::ZERO)
    } else {
        // The host owns the document receiver. Bound the wait while IO is
        // active so completions also reach hidden or unpaced documents.
        (!net.direct_wakeup && net.transport.active()).then_some(Duration::from_millis(10))
    }
}

pub(crate) fn next<'s>(scope: &mut PinScope<'s, '_>) -> Option<Local<'s, Value>> {
    let net = network(scope);
    let (id, event) = net
        .local
        .pop_front()
        .or_else(|| net.incoming.try_recv().ok())?;
    if matches!(event, Event::Close { .. } | Event::End) {
        net.transport.cancel(id);
    }
    let (kind, payload) = match event {
        Event::Headers(response) => ("headers", response_value(scope, response)?),
        Event::Chunk(bytes) => ("chunk", bytes_value(scope, bytes)?),
        Event::Open(protocol) => ("open", v8::String::new(scope, &protocol)?.into()),
        Event::Message(Data::Binary(bytes)) => ("message", bytes_value(scope, bytes)?),
        Event::Message(Data::Text(text)) => ("message", v8::String::new(scope, &text)?.into()),
        Event::Sent(bytes) => ("sent", v8::Number::new(scope, bytes as f64).into()),
        Event::Close {
            code,
            reason,
            clean,
        } => {
            let value = Object::new(scope);
            let code = v8::Integer::new(scope, i32::from(code));
            let reason = v8::String::new(scope, &reason)?;
            let clean = v8::Boolean::new(scope, clean);
            set(scope, value, "code", code.into())?;
            set(scope, value, "reason", reason.into())?;
            set(scope, value, "wasClean", clean.into())?;
            ("close", value.into())
        }
        Event::End => ("end", v8::null(scope).into()),
        Event::Error => ("error", v8::null(scope).into()),
    };
    let id = v8::Integer::new_from_unsigned(scope, id);
    let kind = v8::String::new(scope, kind)?;
    Some(v8::Array::new_with_elements(scope, &[id.into(), kind.into(), payload]).into())
}

fn response_value<'s>(
    scope: &mut PinScope<'s, '_>,
    response: Response,
) -> Option<Local<'s, Value>> {
    let object = Object::new(scope);
    let url = v8::String::new(scope, &response.url)?;
    let status = v8::Integer::new(scope, i32::from(response.status));
    let text = v8::String::new(scope, &response.status_text)?;
    let redirected = v8::Boolean::new(scope, response.redirected);
    let headers = array(scope, response.headers, |scope, (name, value)| {
        let name = v8::String::new(scope, &name)?;
        let value = v8::String::new(scope, &value)?;
        Some(v8::Array::new_with_elements(scope, &[name.into(), value.into()]).into())
    })?;
    set(scope, object, "url", url.into())?;
    set(scope, object, "status", status.into())?;
    set(scope, object, "statusText", text.into())?;
    set(scope, object, "redirected", redirected.into())?;
    set(scope, object, "headers", headers.into())?;
    Some(object.into())
}

pub(crate) fn bytes_value<'s>(
    scope: &mut PinScope<'s, '_>,
    bytes: Vec<u8>,
) -> Option<Local<'s, Value>> {
    let len = bytes.len();
    let buffer = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    let buffer = v8::ArrayBuffer::with_backing_store(scope, &buffer);
    Some(v8::Uint8Array::new(scope, buffer, 0, len)?.into())
}

pub(crate) fn bytes(value: Local<'_, Value>) -> Option<Vec<u8>> {
    let view = Local::<v8::Uint8Array>::try_from(value).ok()?;
    let mut bytes = vec![0; view.byte_length()];
    view.copy_contents(&mut bytes);
    Some(bytes)
}

fn strings(scope: &mut PinScope<'_, '_>, value: Local<'_, Value>) -> Option<Vec<String>> {
    let values = Local::<v8::Array>::try_from(value).ok()?;
    (0..values.length())
        .map(|index| {
            let item = values.get_index(scope, index)?;
            string(scope, item)
        })
        .collect()
}

fn headers(scope: &mut PinScope<'_, '_>, value: Local<'_, Value>) -> Option<Headers> {
    let values = Local::<v8::Array>::try_from(value).ok()?;
    (0..values.length())
        .map(|index| {
            let item = values.get_index(scope, index)?;
            let mut pair = strings(scope, item)?.into_iter();
            Some((pair.next()?, pair.next()?))
        })
        .collect()
}

fn request(scope: &mut PinScope<'_, '_>, args: &FunctionCallbackArguments<'_>) -> Option<Request> {
    let url = network(scope).document.clone();
    let url = url.join(&string(scope, args.get(1))?).ok()?;
    let method = string(scope, args.get(2))?;
    let headers = headers(scope, args.get(3))?;
    let redirect = match string(scope, args.get(5))?.as_str() {
        "follow" => Redirect::Follow,
        "error" => Redirect::Error,
        "manual" => Redirect::Manual,
        _ => return None,
    };
    Some(Request {
        url,
        method,
        headers,
        body: bytes(args.get(4)),
        redirect,
    })
}

fn fetch_start(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    let Some(id) = unsigned(args.get(0)) else {
        return;
    };
    let Some(mut request) = request(scope, &args) else {
        network(scope).local.push_back((id, Event::Error));
        return;
    };
    request.url.set_fragment(None);
    if request.url.scheme() == "data" {
        let response = data_response(&request);
        let net = network(scope);
        if let Some((response, body)) = response {
            local_response(net, id, response, body);
        } else {
            net.local.push_back((id, Event::Error));
        }
        return;
    }
    let entry = if request.url.scheme() == "blob" {
        if request.method != "GET" {
            network(scope).local.push_back((id, Event::Error));
            return;
        }
        let Some(blob) = crate::data::registry(scope).resolve(request.url.as_str()) else {
            network(scope).local.push_back((id, Event::Error));
            return;
        };
        Some((blob.mime, blob.bytes.to_vec()))
    } else {
        let net = network(scope);
        if net.dev || request.url.origin() != net.document.origin() {
            net.transport.fetch(id, request);
            return;
        }
        let path = if request.url.path() == "/" {
            net.entry_html
                .clone()
                .or_else(|| Some("/index.html".into()))
        } else {
            loader::pack_path(&request.url).ok()
        };
        path.and_then(|path| loader::pack_entry(scope, &path))
    };
    let net = network(scope);
    let (status, status_text, headers, body) = match request.method.as_str() {
        "GET" | "HEAD" => match entry {
            Some((mime, bytes)) => {
                let length = bytes.len();
                (
                    200,
                    "OK",
                    vec![
                        ("content-type".into(), mime),
                        ("content-length".into(), length.to_string()),
                    ],
                    if request.method == "GET" {
                        bytes
                    } else {
                        Vec::new()
                    },
                )
            }
            None => (404, "Not Found", Vec::new(), Vec::new()),
        },
        _ => (
            405,
            "Method Not Allowed",
            vec![("allow".into(), "GET, HEAD".into())],
            Vec::new(),
        ),
    };
    local_response(
        net,
        id,
        Response {
            url: request.url.to_string(),
            status,
            status_text: status_text.into(),
            headers,
            redirected: false,
        },
        body,
    );
}

fn data_response(request: &Request) -> Option<(Response, Vec<u8>)> {
    let data = data_url::DataUrl::process(request.url.as_str()).ok()?;
    let (mut body, _) = data.decode_to_vec().ok()?;
    if request.method == "HEAD" {
        body.clear();
    }
    Some((
        Response {
            url: request.url.to_string(),
            status: 200,
            status_text: "OK".into(),
            headers: vec![("content-type".into(), data.mime_type().to_string())],
            redirected: false,
        },
        body,
    ))
}

fn local_response(net: &mut Network, id: u32, response: Response, body: Vec<u8>) {
    net.local.push_back((id, Event::Headers(response)));
    if body.is_empty() {
        net.local.push_back((id, Event::End));
    } else {
        net.local_bodies.insert(id, std::io::Cursor::new(body));
    }
}

fn fetch_credit(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    use std::io::Read;
    let (Some(id), Some(credit)) = (unsigned(args.get(0)), unsigned(args.get(1))) else {
        return;
    };
    let net = network(scope);
    let credit = (credit as usize).min(64 * 1024);
    if let Some(source) = net.local_bodies.get_mut(&id) {
        let mut bytes = vec![0; credit];
        let count = source
            .read(&mut bytes)
            .expect("reading local response bytes");
        bytes.truncate(count);
        if count != 0 {
            net.local.push_back((id, Event::Chunk(bytes)));
        }
        if source.position() == source.get_ref().len() as u64 {
            net.local_bodies.remove(&id);
            net.local.push_back((id, Event::End));
        }
    } else {
        net.transport.credit(id, credit);
    }
}

fn cancel(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    if let Some(id) = unsigned(args.get(0)) {
        let net = network(scope);
        net.local_bodies.remove(&id);
        net.local.retain(|(event_id, _)| id != *event_id);
        net.transport.cancel(id);
    }
}

fn socket_open(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    let Some(id) = unsigned(args.get(0)) else {
        return;
    };
    let Some(url) = string(scope, args.get(1)).and_then(|url| Url::parse(&url).ok()) else {
        return;
    };
    let Some(protocols) = strings(scope, args.get(2)) else {
        return;
    };
    let net = network(scope);
    net.transport.socket(
        id,
        url,
        net.document.origin().ascii_serialization(),
        protocols,
    );
}

fn socket_send(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    let Some(id) = unsigned(args.get(0)) else {
        return;
    };
    let value = args.get(1);
    if let Some(data) = string(scope, value)
        .map(Data::Text)
        .or_else(|| bytes(value).map(Data::Binary))
    {
        network(scope).transport.send(id, data);
    }
}

fn socket_close(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    if let (Some(id), Some(code), Some(reason)) = (
        unsigned(args.get(0)),
        unsigned(args.get(1)),
        string(scope, args.get(2)),
    ) {
        network(scope).transport.close(id, code as u16, reason);
    }
}

fn utf8_encode(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(value) =
        string(scope, args.get(0)).and_then(|value| bytes_value(scope, value.into_bytes()))
    {
        result.set(value);
    }
}

fn utf8_decode(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(bytes) = bytes(args.get(0))
        && let Some(value) = v8::String::new(scope, &String::from_utf8_lossy(&bytes))
    {
        result.set(value.into());
    }
}

fn run_classic<'s>(
    scope: &mut PinScope<'s, '_>,
    args: FunctionCallbackArguments<'s>,
    _: ReturnValue<'_>,
) {
    if let (Ok(code), Some(url)) = (
        Local::<v8::String>::try_from(args.get(0)),
        string(scope, args.get(1)),
    ) {
        loader::classic(scope, code, &url);
    }
}

fn storage_entries(
    scope: &mut PinScope<'_, '_>,
    _: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Some(storage) = network(scope).storage.as_ref() else {
        result.set(v8::null(scope).into());
        return;
    };
    let entries = storage.read();
    if let Ok(entries) = entries
        && let Some(entries) = array(scope, entries, |scope, (key, value)| {
            let key = two_byte(scope, &key)?;
            let value = two_byte(scope, &value)?;
            Some(v8::Array::new_with_elements(scope, &[key.into(), value.into()]).into())
        })
    {
        result.set(entries.into());
    }
}

fn storage_commit(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let change = if args.get(0).is_null() {
        Change::Clear
    } else {
        let Some(key) = utf16(scope, args.get(0)) else {
            return;
        };
        match utf16(scope, args.get(1)) {
            Some(value) => Change::Set(key, value),
            None => Change::Remove(key),
        }
    };
    let ok = network(scope)
        .storage
        .as_ref()
        .is_none_or(|storage| storage.commit(change).is_ok());
    result.set_bool(ok);
}

fn storage_flush(
    scope: &mut PinScope<'_, '_>,
    _: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let net = network(scope);
    let deadline = net
        .shutdown
        .deadline()
        .unwrap_or_else(|| Instant::now() + STORAGE_CLOSE_BUDGET);
    let flush = net.storage.as_ref().map(|storage| storage.flush(deadline));
    match flush {
        Some(Err(error)) => {
            eprintln!("squark localStorage shutdown flush failed: {error}");
            result.set_bool(false);
        }
        _ => result.set_bool(true),
    }
}

fn idb_read(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Some(name) = utf16(scope, args.get(0)) else {
        return;
    };
    let net = network(scope);
    let Some(store) = net.storage.as_ref() else {
        result.set(v8::null(scope).into());
        return;
    };
    match store.read_database(&name, net.shutdown.deadline()) {
        Ok((revision, bytes)) => {
            let revision = v8::Integer::new_from_unsigned(scope, revision);
            let data = bytes
                .and_then(|data| bytes_value(scope, data))
                .unwrap_or_else(|| v8::null(scope).into());
            result.set(v8::Array::new_with_elements(scope, &[revision.into(), data]).into());
        }
        Err(error) => crate::bindings::throw(scope, &error.to_string(), false),
    }
}

fn idb_commit(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let (Some(name), Some(revision)) = (utf16(scope, args.get(0)), unsigned(args.get(1))) else {
        return;
    };
    let data = bytes(args.get(2));
    let net = network(scope);
    let Some(store) = net.storage.as_ref() else {
        return;
    };
    let deadline = net
        .shutdown
        .deadline()
        .unwrap_or_else(|| Instant::now() + STORAGE_CLOSE_BUDGET);
    match store.write_database(&name, revision, data, deadline) {
        Ok(written) => result.set_bool(written),
        Err(error) => crate::bindings::throw(scope, &error.to_string(), false),
    }
}

fn idb_shutdown_time_remaining(
    scope: &mut PinScope<'_, '_>,
    _: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    result.set_bool(
        network(scope)
            .shutdown
            .deadline()
            .is_some_and(|deadline| Instant::now() < deadline),
    );
}
