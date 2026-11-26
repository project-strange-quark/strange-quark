// SPDX-License-Identifier: MIT

//! Document-side canvas staging and native callbacks.

use std::collections::{BTreeMap, BTreeSet};

use squark_types::{
    NodeId,
    journal::{Opcode, Record},
};
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue, Value};

use crate::bindings::{array, set, string, throw, two_byte, unsigned, utf16, with_bridge};

type StagedBytes<'a> = Vec<(u32, &'a [u8])>;

#[derive(Default)]
pub(crate) struct Staging {
    last_id: u32,
    bytes: BTreeMap<u32, Vec<u8>>,
}

impl Staging {
    fn insert(&mut self, bytes: Vec<u8>) -> Option<u32> {
        self.last_id = self.last_id.checked_add(1)?;
        self.bytes.insert(self.last_id, bytes);
        Some(self.last_id)
    }

    pub(crate) fn resolve(
        &self,
        records: &[Record],
        strings: &[&[u16]],
    ) -> Result<StagedBytes<'_>, String> {
        let mut seen = BTreeSet::new();
        let mut staged = Vec::new();
        for record in records {
            let (id, length) = match record.op {
                Opcode::CanvasSubmit => (record.b, Some(record.c as usize)),
                Opcode::FontFace if record.flags == 0 => {
                    let descriptor = String::from_utf16_lossy(strings[record.c as usize]);
                    let json: serde_json::Value = serde_json::from_str(&descriptor)
                        .map_err(|error| format!("invalid font face descriptor: {error}"))?;
                    let Some(buffer) = json.get("source").and_then(|source| source.get("bufferId"))
                    else {
                        continue;
                    };
                    let id = buffer
                        .as_u64()
                        .and_then(|id| u32::try_from(id).ok())
                        .ok_or_else(|| "invalid font face buffer id".to_owned())?;
                    (id, None)
                }
                _ => continue,
            };
            if !seen.insert(id) {
                return Err(format!("buffer {id} is cited more than once"));
            }
            let bytes = self
                .bytes
                .get(&id)
                .ok_or_else(|| format!("buffer {id} was not staged"))?;
            if length.is_some_and(|length| bytes.len() != length) {
                return Err(format!("staged buffer {id} has an incorrect length"));
            }
            staged.push((id, bytes.as_slice()));
        }
        Ok(staged)
    }

    pub(crate) fn consume(&mut self, ids: Vec<u32>) {
        for id in ids {
            self.bytes.remove(&id);
        }
    }
}

/// Canvas backend services supplied by the composition root. These callbacks
/// stay on the document thread. A readback observes the last accepted journal prefix.
pub trait CanvasBackend {
    fn image_size(&self, _image: NodeId) -> Option<[u32; 2]> {
        None
    }

    fn readback(
        &mut self,
        _canvas: NodeId,
        _origin: [i32; 2],
        _size: [u32; 2],
    ) -> Result<Vec<u8>, String> {
        Err("canvas readback is unavailable".into())
    }

    fn encode(
        &mut self,
        _canvas: NodeId,
        _mime: &str,
        _quality: Option<f64>,
    ) -> Result<Vec<u8>, String> {
        Err("canvas encoding is unavailable".into())
    }
}

struct Unavailable;
impl CanvasBackend for Unavailable {}

pub(crate) struct Backend(Box<dyn CanvasBackend>);

pub(crate) fn install(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    backend: Option<Box<dyn CanvasBackend>>,
) -> Option<()> {
    scope.set_slot(Backend(backend.unwrap_or_else(|| Box::new(Unavailable))));
    for (name, callback) in [
        ("stageBytes", Function::new(scope, stage_bytes)),
        ("cssParseColor", Function::new(scope, parse_color)),
        ("cssValidateFilter", Function::new(scope, validate_filter)),
        ("cssParseCanvasFont", Function::new(scope, parse_font)),
        ("canvasImageSize", Function::new(scope, image_size)),
        ("canvasReadback", Function::new(scope, readback)),
        ("canvasEncodeSync", Function::new(scope, encode_sync)),
        ("canvasEncode", Function::new(scope, encode_async)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn backend<'a>(scope: &'a mut PinScope<'_, '_>) -> &'a mut dyn CanvasBackend {
    &mut *scope
        .get_slot_mut::<Backend>()
        .expect("canvas bindings installed")
        .0
}

fn stage_bytes(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Ok(view) = Local::<v8::Uint8Array>::try_from(args.get(0)) else {
        throw(scope, "stageBytes expects a Uint8Array", false);
        return;
    };
    let mut bytes = vec![0; view.byte_length()];
    if view.copy_contents(&mut bytes) != bytes.len() {
        throw(scope, "cannot copy detached staged bytes", false);
        return;
    }
    if let Some(id) = with_bridge(scope, |bridge| bridge.staging.insert(bytes)) {
        result.set_uint32(id);
    } else {
        throw(scope, "staging ID space exhausted", false);
    }
}

fn css_input(
    scope: &mut PinScope<'_, '_>,
    args: &FunctionCallbackArguments<'_>,
) -> Option<(Vec<u16>, NodeId)> {
    Some((
        utf16(scope, args.get(0))?,
        NodeId::from_raw(unsigned(args.get(1))?),
    ))
}

fn parse_color(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let color = css_input(scope, &args).and_then(|(text, canvas)| {
        with_bridge(scope, |bridge| bridge.engine.parse_color(canvas, &text))
    });
    let value = color.and_then(|color| {
        array(scope, color.to_vec(), |scope, byte| {
            Some(v8::Integer::new(scope, i32::from(byte)).into())
        })
        .map(Local::<Value>::from)
    });
    result.set(value.unwrap_or_else(|| v8::null(scope).into()));
}

fn validate_filter(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let valid = utf16(scope, args.get(0))
        .is_some_and(|text| with_bridge(scope, |bridge| bridge.engine.validate_filter(&text)));
    result.set_bool(valid);
}

fn parse_font(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let font = css_input(scope, &args).and_then(|(text, canvas)| {
        with_bridge(scope, |bridge| {
            bridge.engine.parse_canvas_font(canvas, &text)
        })
    });
    let value = font.and_then(|font| two_byte(scope, &font).map(Local::<Value>::from));
    result.set(value.unwrap_or_else(|| v8::null(scope).into()));
}

fn image_size(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let size = unsigned(args.get(0)).and_then(|id| backend(scope).image_size(NodeId::from_raw(id)));
    let value = size.and_then(|size| {
        array(scope, size.to_vec(), |scope, size| {
            Some(v8::Integer::new_from_unsigned(scope, size).into())
        })
        .map(Local::<Value>::from)
    });
    result.set(value.unwrap_or_else(|| v8::null(scope).into()));
}

fn buffer<'s>(scope: &mut PinScope<'s, '_>, bytes: Vec<u8>) -> Local<'s, v8::ArrayBuffer> {
    let backing = v8::ArrayBuffer::new_backing_store_from_vec(bytes).make_shared();
    v8::ArrayBuffer::with_backing_store(scope, &backing)
}

fn readback(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let read = (|| {
        let canvas = NodeId::from_raw(unsigned(args.get(0))?);
        let x = Local::<v8::Int32>::try_from(args.get(1)).ok()?.value();
        let y = Local::<v8::Int32>::try_from(args.get(2)).ok()?.value();
        let width = unsigned(args.get(3))?;
        let height = unsigned(args.get(4))?;
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        Some(
            backend(scope)
                .readback(canvas, [x, y], [width, height])
                .and_then(|bytes| {
                    if bytes.len() == len {
                        Ok(bytes)
                    } else {
                        Err("canvas readback size mismatch".into())
                    }
                }),
        )
    })();
    match read {
        Some(Ok(bytes)) => {
            let length = bytes.len();
            let buffer = buffer(scope, bytes);
            if let Some(view) = v8::Uint8ClampedArray::new(scope, buffer, 0, length) {
                result.set(view.into());
            }
        }
        Some(Err(error)) => throw(scope, &error, false),
        None => throw(scope, "invalid canvas readback arguments", false),
    }
}

fn encoded(
    scope: &mut PinScope<'_, '_>,
    args: &FunctionCallbackArguments<'_>,
) -> Result<Vec<u8>, String> {
    let canvas = unsigned(args.get(0))
        .map(NodeId::from_raw)
        .ok_or("invalid canvas ID")?;
    let mime = string(scope, args.get(1)).ok_or("invalid canvas MIME")?;
    if !matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp") {
        return Err("unsupported canvas MIME".into());
    }
    let quality = Local::<v8::Number>::try_from(args.get(2))
        .ok()
        .map(|value| value.value())
        .filter(|value| (0.0..=1.0).contains(value));
    backend(scope).encode(canvas, &mime, quality)
}

fn encode_sync(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    match encoded(scope, &args) {
        Ok(bytes) => result.set(buffer(scope, bytes).into()),
        Err(error) => throw(scope, &error, false),
    }
}

fn encode_async(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Some(resolver) = v8::PromiseResolver::new(scope) else {
        return;
    };
    let promise = resolver.get_promise(scope);
    match encoded(scope, &args) {
        Ok(bytes) => {
            let bytes = buffer(scope, bytes);
            resolver.resolve(scope, bytes.into());
        }
        Err(error) => {
            if let Some(message) = v8::String::new(scope, &error) {
                let error = v8::Exception::type_error(scope, message);
                resolver.reject(scope, error);
            }
        }
    }
    result.set(promise.into());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staged_bytes_are_checked_per_batch_and_consumed_only_after_acceptance() {
        let mut staging = Staging::default();
        let id = staging.insert(vec![0x4a, 0x43, 0x56, 0x31]).unwrap();
        let record = Record {
            op: Opcode::CanvasSubmit,
            a: 8,
            b: id,
            c: 4,
            flags: 0,
        };
        assert!(
            staging
                .resolve(
                    &[Record {
                        b: id + 1,
                        ..record
                    }],
                    &[]
                )
                .is_err()
        );
        assert!(staging.resolve(&[Record { c: 3, ..record }], &[]).is_err());
        assert!(staging.resolve(&[record, record], &[]).is_err());
        assert_eq!(
            staging.resolve(&[record], &[]).unwrap(),
            vec![(id, b"JCV1".as_slice())]
        );
        assert_eq!(
            staging.resolve(&[record], &[]).unwrap(),
            vec![(id, b"JCV1".as_slice())]
        );
        staging.consume(vec![id]);
        assert!(staging.resolve(&[record], &[]).is_err());
    }
}
