// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use base64::{Engine as _, alphabet, engine::general_purpose};
use ring::rand::{SecureRandom, SystemRandom};
use url::Url;
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue};

use crate::bindings::{set, string, throw, unsigned};

#[derive(Clone, Debug)]
pub struct BlobAsset {
    pub mime: String,
    pub bytes: Arc<[u8]>,
}

/// A document can share its object URLs with its image resolver.
#[derive(Clone, Debug, Default)]
pub struct BlobRegistry(Arc<Mutex<HashMap<String, BlobAsset>>>);

impl BlobRegistry {
    pub fn resolve(&self, url: &str) -> Option<BlobAsset> {
        let key = url.split('#').next()?;
        self.0.lock().ok()?.get(key).cloned()
    }

    fn register(&self, origin: &str, asset: BlobAsset) -> Option<String> {
        let uuid = uuid()?;
        let url = format!("blob:{origin}/{uuid}");
        self.0.lock().ok()?.insert(url.clone(), asset);
        Some(url)
    }

    fn revoke(&self, url: &str) {
        if let Ok(mut blobs) = self.0.lock() {
            blobs.remove(url.split('#').next().unwrap_or_default());
        }
    }
}

pub(crate) fn registry<'a>(scope: &'a PinScope<'_, '_>) -> &'a BlobRegistry {
    &scope
        .get_slot::<Data>()
        .expect("data bindings installed")
        .registry
}

struct Data {
    origin: String,
    registry: BlobRegistry,
}

pub(crate) fn install(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    document: &Url,
    registry: BlobRegistry,
) -> Option<()> {
    scope.set_slot(Data {
        origin: document.origin().ascii_serialization(),
        registry,
    });
    for (name, callback) in [
        ("blobRegister", Function::new(scope, blob_register)),
        ("blobRevoke", Function::new(scope, blob_revoke)),
        ("decodeText", Function::new(scope, decode_text)),
        ("base64Encode", Function::new(scope, base64_encode)),
        ("base64Decode", Function::new(scope, base64_decode)),
        ("randomBytes", Function::new(scope, random_bytes)),
        ("randomUUID", Function::new(scope, random_uuid)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn blob_register(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let (Some(bytes), Some(mime)) = (
        crate::network::bytes(args.get(0)),
        string(scope, args.get(1)),
    ) else {
        throw(scope, "invalid blob URL arguments", false);
        return;
    };
    let data = scope.get_slot::<Data>().expect("data bindings installed");
    let url = data.registry.register(
        &data.origin,
        BlobAsset {
            mime,
            bytes: bytes.into(),
        },
    );
    if let Some(url) = url.and_then(|url| v8::String::new(scope, &url)) {
        result.set(url.into());
    }
}

fn blob_revoke(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    if let Some(url) = string(scope, args.get(0)) {
        registry(scope).revoke(&url);
    }
}

fn decode_text(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Some(bytes) = crate::network::bytes(args.get(0)) else {
        return;
    };
    let label = string(scope, args.get(1)).unwrap_or_default();
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    let (text, _, _) = encoding.decode(&bytes);
    if let Some(text) = v8::String::new(scope, &text) {
        result.set(text.into());
    }
}

fn base64_encode(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(bytes) = crate::network::bytes(args.get(0)) {
        let engine = if args.get(1).is_true() {
            &general_purpose::URL_SAFE_NO_PAD
        } else {
            &general_purpose::STANDARD
        };
        if let Some(value) = v8::String::new(scope, &engine.encode(bytes)) {
            result.set(value.into());
        }
    }
}

fn base64_decode(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    const FORGIVING: general_purpose::GeneralPurpose = general_purpose::GeneralPurpose::new(
        &alphabet::STANDARD,
        general_purpose::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let value = string(scope, args.get(0))
        .and_then(|text| {
            if args.get(1).is_true() {
                general_purpose::URL_SAFE_NO_PAD.decode(text).ok()
            } else {
                FORGIVING.decode(text).ok()
            }
        })
        .and_then(|bytes| crate::network::bytes_value(scope, bytes));
    result.set(value.unwrap_or_else(|| v8::null(scope).into()));
}

pub(crate) fn random(length: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0; length];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes)
}

fn random_bytes(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let value = unsigned(args.get(0))
        .filter(|size| *size <= 65536)
        .and_then(|size| random(size as usize))
        .and_then(|bytes| crate::network::bytes_value(scope, bytes));
    if let Some(value) = value {
        result.set(value);
    } else {
        throw(scope, "random bytes unavailable", false);
    }
}

fn uuid() -> Option<String> {
    let mut bytes = random(16)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let mut id = String::with_capacity(36);
    use std::fmt::Write as _;
    for (i, byte) in bytes.into_iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            id.push('-');
        }
        write!(id, "{byte:02x}").ok()?;
    }
    Some(id)
}

fn random_uuid(
    scope: &mut PinScope<'_, '_>,
    _: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(value) = uuid().and_then(|uuid| v8::String::new(scope, &uuid)) {
        result.set(value.into());
    } else {
        throw(scope, "random UUID unavailable", false);
    }
}
