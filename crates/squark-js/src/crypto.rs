// SPDX-License-Identifier: MIT

use std::collections::HashMap;

use ring::{aead, digest};
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue};

use crate::{
    bindings::{set, string, throw, unsigned},
    data::random,
    network::{bytes, bytes_value},
};

#[derive(Default)]
struct Keys {
    last_id: u32,
    keys: HashMap<u32, Key>,
}

struct Key {
    bytes: Vec<u8>,
    extractable: bool,
    usages: u32,
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    scope.set_slot(Keys::default());
    for (name, callback) in [
        ("cryptoDigest", Function::new(scope, digest_bytes)),
        ("cryptoGenerateKey", Function::new(scope, generate_key)),
        ("cryptoImportKey", Function::new(scope, import_key)),
        ("cryptoExportKey", Function::new(scope, export_key)),
        ("cryptoForgetKey", Function::new(scope, forget_key)),
        ("cryptoAesGcm", Function::new(scope, aes_gcm)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn digest_bytes(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let algorithm = match string(scope, args.get(0)).as_deref() {
        Some("SHA-1") => &digest::SHA1_FOR_LEGACY_USE_ONLY,
        Some("SHA-256") => &digest::SHA256,
        _ => return,
    };
    if let Some(value) = bytes(args.get(1))
        .and_then(|data| bytes_value(scope, digest::digest(algorithm, &data).as_ref().to_vec()))
    {
        result.set(value);
    }
}

fn save_key(
    scope: &mut PinScope<'_, '_>,
    args: &FunctionCallbackArguments<'_>,
    bytes: Vec<u8>,
) -> Option<u32> {
    let usages = unsigned(args.get(2))?;
    if !matches!(bytes.len(), 16 | 32) || usages == 0 || usages & !3 != 0 {
        return None;
    }
    let keys = scope.get_slot_mut::<Keys>()?;
    keys.last_id = keys.last_id.checked_add(1)?;
    keys.keys.insert(
        keys.last_id,
        Key {
            bytes,
            extractable: args.get(1).is_true(),
            usages,
        },
    );
    Some(keys.last_id)
}

fn generate_key(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let id = unsigned(args.get(0))
        .filter(|n| matches!(n, 128 | 256))
        .and_then(|n| random(n as usize / 8))
        .and_then(|bytes| save_key(scope, &args, bytes));
    if let Some(id) = id {
        result.set_uint32(id);
    } else {
        throw(scope, "AES key generation failed", false);
    }
}

fn import_key(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(id) = bytes(args.get(0)).and_then(|bytes| save_key(scope, &args, bytes)) {
        result.set_uint32(id);
    } else {
        throw(scope, "invalid AES key", false);
    }
}

fn export_key(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let key = unsigned(args.get(0))
        .and_then(|id| scope.get_slot::<Keys>()?.keys.get(&id))
        .filter(|key| key.extractable)
        .map(|key| key.bytes.clone());
    if let Some(value) = key.and_then(|key| bytes_value(scope, key)) {
        result.set(value);
    } else {
        throw(scope, "AES key cannot be exported", false);
    }
}

fn forget_key(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    if let Some(id) = unsigned(args.get(0)) {
        scope
            .get_slot_mut::<Keys>()
            .expect("keys installed")
            .keys
            .remove(&id);
    }
}

fn encrypt_or_decrypt(
    scope: &PinScope<'_, '_>,
    args: &FunctionCallbackArguments<'_>,
) -> Option<Vec<u8>> {
    let key = scope
        .get_slot::<Keys>()?
        .keys
        .get(&unsigned(args.get(0))?)?;
    let encrypt = args.get(4).is_true();
    if key.usages & (if encrypt { 1 } else { 2 }) == 0 {
        return None;
    }
    let algorithm = if key.bytes.len() == 16 {
        &aead::AES_128_GCM
    } else {
        &aead::AES_256_GCM
    };
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(algorithm, &key.bytes).ok()?);
    let nonce = aead::Nonce::try_assume_unique_for_key(&bytes(args.get(1))?).ok()?;
    let aad = aead::Aad::from(bytes(args.get(2))?);
    let mut data = bytes(args.get(3))?;
    if encrypt {
        key.seal_in_place_append_tag(nonce, aad, &mut data).ok()?;
    } else {
        let length = key.open_in_place(nonce, aad, &mut data).ok()?.len();
        data.truncate(length);
    }
    Some(data)
}

fn aes_gcm(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let data = encrypt_or_decrypt(scope, &args).and_then(|data| bytes_value(scope, data));
    result.set(data.unwrap_or_else(|| v8::null(scope).into()));
}
