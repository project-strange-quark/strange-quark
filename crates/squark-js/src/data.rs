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
    decoders: TextDecoders,
}

/// The state of each `TextDecoder` (Encoding §7.2): a stream may stop in the
/// middle of a character, which the next `decode` call completes.
#[derive(Default)]
struct TextDecoders {
    next: u32,
    open: HashMap<u32, TextDecoderState>,
}

struct TextDecoderState {
    encoding: &'static encoding_rs::Encoding,
    ignore_bom: bool,
    decoder: encoding_rs::Decoder,
}

impl TextDecoderState {
    fn fresh(encoding: &'static encoding_rs::Encoding, ignore_bom: bool) -> encoding_rs::Decoder {
        // Without ignoreBOM, a BOM of the decoder's own encoding is dropped
        // at the start of a stream (and only there).
        if ignore_bom {
            encoding.new_decoder_without_bom_handling()
        } else {
            encoding.new_decoder_with_bom_removal()
        }
    }

    /// Decodes `bytes`, keeping an incomplete trailing character for the next
    /// call unless `flush`. `None` is a fatal error (malformed input).
    fn decode(&mut self, bytes: &[u8], flush: bool, fatal: bool) -> Option<String> {
        let capacity = self
            .decoder
            .max_utf8_buffer_length(bytes.len())
            .unwrap_or(usize::MAX / 2);
        let mut text = String::with_capacity(capacity);
        let complete = if fatal {
            let (result, _) = self
                .decoder
                .decode_to_string_without_replacement(bytes, &mut text, flush);
            !matches!(result, encoding_rs::DecoderResult::Malformed(..))
        } else {
            // The buffer holds the worst case, so all input is consumed.
            let (result, _, _) = self.decoder.decode_to_string(bytes, &mut text, flush);
            debug_assert_eq!(result, encoding_rs::CoderResult::InputEmpty);
            true
        };
        if flush || !complete {
            self.decoder = Self::fresh(self.encoding, self.ignore_bom);
        }
        complete.then_some(text)
    }
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
        decoders: TextDecoders::default(),
    });
    for (name, callback) in [
        ("blobRegister", Function::new(scope, blob_register)),
        ("blobRevoke", Function::new(scope, blob_revoke)),
        ("decodeText", Function::new(scope, decode_text)),
        ("encodingName", Function::new(scope, encoding_name)),
        ("textDecoderOpen", Function::new(scope, text_decoder_open)),
        (
            "textDecoderDecode",
            Function::new(scope, text_decoder_decode),
        ),
        ("textDecoderClose", Function::new(scope, text_decoder_close)),
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

/// The canonical, lowercase name of the encoding a label names, if
/// TextDecoder supports it (not the "replacement" encoding).
fn encoding_name(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let label = string(scope, args.get(0)).unwrap_or_default();
    let Some(encoding) = encoding_rs::Encoding::for_label_no_replacement(label.as_bytes()) else {
        return;
    };
    if let Some(name) = v8::String::new(scope, &encoding.name().to_ascii_lowercase()) {
        result.set(name.into());
    }
}

fn text_decoder_open(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let label = string(scope, args.get(0)).unwrap_or_default();
    let Some(encoding) = encoding_rs::Encoding::for_label_no_replacement(label.as_bytes()) else {
        throw(scope, "unsupported encoding", false);
        return;
    };
    let ignore_bom = args.get(1).is_true();
    let decoders = &mut scope
        .get_slot_mut::<Data>()
        .expect("data bindings installed")
        .decoders;
    decoders.next = decoders.next.wrapping_add(1).max(1);
    let id = decoders.next;
    decoders.open.insert(
        id,
        TextDecoderState {
            encoding,
            ignore_bom,
            decoder: TextDecoderState::fresh(encoding, ignore_bom),
        },
    );
    result.set(v8::Integer::new_from_unsigned(scope, id).into());
}

/// `(id, bytes, flush, fatal)`: the decoded text, or null on a fatal error.
fn text_decoder_decode(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let (Some(id), Some(bytes)) = (unsigned(args.get(0)), crate::network::bytes(args.get(1)))
    else {
        throw(scope, "invalid TextDecoder arguments", false);
        return;
    };
    let flush = args.get(2).is_true();
    let fatal = args.get(3).is_true();
    let Some(state) = scope
        .get_slot_mut::<Data>()
        .expect("data bindings installed")
        .decoders
        .open
        .get_mut(&id)
    else {
        throw(scope, "TextDecoder is closed", false);
        return;
    };
    match state.decode(&bytes, flush, fatal) {
        Some(text) => {
            if let Some(text) = v8::String::new(scope, &text) {
                result.set(text.into());
            }
        }
        None => result.set_null(),
    }
}

fn text_decoder_close(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    if let Some(id) = unsigned(args.get(0)) {
        scope
            .get_slot_mut::<Data>()
            .expect("data bindings installed")
            .decoders
            .open
            .remove(&id);
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

#[cfg(test)]
mod text_decoder_tests {
    use super::TextDecoderState;

    fn state(label: &str, ignore_bom: bool) -> TextDecoderState {
        let encoding = encoding_rs::Encoding::for_label_no_replacement(label.as_bytes()).unwrap();
        TextDecoderState {
            encoding,
            ignore_bom,
            decoder: TextDecoderState::fresh(encoding, ignore_bom),
        }
    }

    #[test]
    fn streams_complete_characters_split_across_chunks() {
        let mut utf8 = state("utf-8", false);
        // "é€" split inside both characters, after a BOM.
        assert_eq!(
            utf8.decode(&[0xef, 0xbb, 0xbf, 0xc3], false, false)
                .unwrap(),
            ""
        );
        assert_eq!(utf8.decode(&[0xa9, 0xe2, 0x82], false, false).unwrap(), "é");
        assert_eq!(utf8.decode(&[0xac], true, false).unwrap(), "€");
        // A flush ends the stream: a later BOM starts a new one and is dropped.
        assert_eq!(
            utf8.decode(&[0xef, 0xbb, 0xbf, 0x61], true, false).unwrap(),
            "a"
        );
        // An incomplete character at a flush is replaced, or fatal.
        assert_eq!(
            utf8.decode(&[0x61, 0xe2], true, false).unwrap(),
            "a\u{fffd}"
        );
        assert_eq!(utf8.decode(&[0x61, 0xe2], true, true), None);
        assert_eq!(
            utf8.decode(&[0x62], true, true).unwrap(),
            "b",
            "reset after an error"
        );

        let mut keep = state("utf8", true);
        assert_eq!(
            keep.decode(&[0xef, 0xbb, 0xbf, 0x61], true, false).unwrap(),
            "\u{feff}a"
        );

        let mut utf16 = state("utf-16le", false);
        assert_eq!(utf16.decode(&[0x41], false, false).unwrap(), "");
        assert_eq!(
            utf16.decode(&[0x00, 0x3d, 0xd8], false, false).unwrap(),
            "A"
        );
        assert_eq!(
            utf16.decode(&[0x00, 0xde], true, false).unwrap(),
            "\u{1f600}"
        );

        let mut sjis = state("shift_jis", false);
        assert_eq!(sjis.decode(&[0x82], false, false).unwrap(), "");
        assert_eq!(sjis.decode(&[0xa0], true, false).unwrap(), "あ");
    }
}
