// SPDX-License-Identifier: MIT

use v8::{
    ArrayBuffer, Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue, Value,
    ValueDeserializerHelper, ValueSerializerHelper,
};

use crate::{
    bindings::{get, set, string},
    network::{bytes, bytes_value},
};

fn marker<'s>(scope: &mut PinScope<'s, '_>) -> Local<'s, v8::Private> {
    let name = v8::String::new(scope, "squark.blob").unwrap();
    v8::Private::for_api(scope, Some(name))
}

fn blob<'s>(
    scope: &mut PinScope<'s, '_>,
    object: Local<'_, Object>,
) -> Option<Local<'s, v8::Array>> {
    let marker = marker(scope);
    Local::try_from(object.get_private(scope, marker)?).ok()
}

struct Prototypes {
    blob: v8::Global<Object>,
    file: v8::Global<Object>,
    uncloneable: Vec<v8::Global<Object>>,
}

pub(crate) fn prepare(scope: &mut PinScope<'_, '_>) -> Option<()> {
    let global = scope.get_current_context().global(scope);
    let mut prototype = |name| {
        let constructor = Local::<Object>::try_from(get(scope, global, name)?).ok()?;
        let prototype = Local::<Object>::try_from(get(scope, constructor, "prototype")?).ok()?;
        Some(v8::Global::new(scope, prototype))
    };
    let blob = prototype("Blob")?;
    let file = prototype("File")?;
    let uncloneable = ["EventTarget", "Event", "CryptoKey"]
        .into_iter()
        .map(prototype)
        .collect::<Option<Vec<_>>>()?;
    scope.set_slot(Prototypes {
        blob,
        file,
        uncloneable,
    });
    Some(())
}

fn clone_error(scope: &mut PinScope<'_, '_>, message: &str) {
    let Some(message) = v8::String::new(scope, message) else {
        return;
    };
    let error = v8::Exception::error(scope, message);
    let name = v8::String::new(scope, "DataCloneError").unwrap();
    if let Ok(object) = Local::<Object>::try_from(error) {
        set(scope, object, "name", name.into());
    }
    scope.throw_exception(error);
}

struct Serializer;

impl v8::ValueSerializerImpl for Serializer {
    fn throw_data_clone_error<'s>(
        &self,
        scope: &mut PinScope<'s, '_>,
        message: Local<'s, v8::String>,
    ) {
        let message = message.to_rust_string_lossy(scope);
        clone_error(scope, &message);
    }

    fn has_custom_host_object(&self, _: &v8::Isolate) -> bool {
        true
    }

    fn is_host_object<'s>(
        &self,
        scope: &mut PinScope<'s, '_>,
        object: Local<'s, Object>,
    ) -> Option<bool> {
        if blob(scope, object).is_some() {
            return Some(true);
        }
        if object.is_proxy() {
            return Some(false);
        }
        if let Some(prototypes) = scope.get_slot::<Prototypes>() {
            let mut parent = object.get_prototype(scope);
            while let Some(object) = parent.and_then(|value| Local::<Object>::try_from(value).ok())
            {
                if prototypes
                    .uncloneable
                    .iter()
                    .any(|prototype| Local::new(scope, prototype) == object)
                {
                    return Some(true);
                }
                if object.is_proxy() {
                    break;
                }
                parent = object.get_prototype(scope);
            }
        }
        Some(false)
    }

    fn write_host_object<'s>(
        &self,
        scope: &mut PinScope<'s, '_>,
        object: Local<'s, Object>,
        writer: &dyn ValueSerializerHelper,
    ) -> Option<bool> {
        let Some(metadata) = blob(scope, object) else {
            clone_error(scope, "The value cannot be cloned");
            return None;
        };
        let data = bytes(metadata.get_index(scope, 0)?)?;
        let mime = metadata.get_index(scope, 1)?;
        let mime = string(scope, mime)?;
        let name = metadata.get_index(scope, 2)?;
        let name = string(scope, name);
        let modified = metadata
            .get_index(scope, 3)?
            .number_value(scope)
            .unwrap_or(0.0);
        writer.write_uint32(if name.is_some() { 1 } else { 0 });
        for field in [
            data.as_slice(),
            mime.as_bytes(),
            name.as_deref().unwrap_or_default().as_bytes(),
        ] {
            writer.write_uint32(u32::try_from(field.len()).ok()?);
            writer.write_raw_bytes(field);
        }
        writer.write_double(modified);
        Some(true)
    }
}

struct Deserializer;

impl v8::ValueDeserializerImpl for Deserializer {
    fn read_host_object<'s>(
        &self,
        scope: &mut PinScope<'s, '_>,
        reader: &dyn ValueDeserializerHelper,
    ) -> Option<Local<'s, Object>> {
        let mut tag = 0;
        if !reader.read_uint32(&mut tag) || tag > 1 {
            return None;
        }
        let field = || {
            let mut length = 0;
            if !reader.read_uint32(&mut length) {
                return None;
            }
            reader.read_raw_bytes(length as usize)
        };
        let data = field()?.to_vec();
        let mime = std::str::from_utf8(field()?).ok()?;
        let name = std::str::from_utf8(field()?).ok()?;
        let mut modified = 0.0;
        if !reader.read_double(&mut modified) {
            return None;
        }
        let data = bytes_value(scope, data)?;
        let mime = v8::String::new(scope, mime)?;
        let name = if tag == 1 {
            v8::String::new(scope, name)?.into()
        } else {
            v8::undefined(scope).into()
        };
        let modified = v8::Number::new(scope, modified);
        let metadata =
            v8::Array::new_with_elements(scope, &[data, mime.into(), name, modified.into()]);
        let prototypes = scope.get_slot::<Prototypes>()?;
        let prototype = Local::new(
            scope,
            if tag == 0 {
                &prototypes.blob
            } else {
                &prototypes.file
            },
        );
        // V8 forbids running JS while its deserializer delegate is on the stack.
        let object = Object::with_prototype_and_properties(scope, prototype.into(), &[], &[]);
        let marker = marker(scope);
        (object.set_private(scope, marker, metadata.into()) == Some(true)).then_some(object)
    }
}

fn encode(
    scope: &mut PinScope<'_, '_>,
    value: Local<'_, Value>,
    transfers: &[Local<'_, ArrayBuffer>],
) -> Option<Vec<u8>> {
    let context = scope.get_current_context();
    let writer = v8::ValueSerializer::new(scope, Box::new(Serializer));
    writer.write_header();
    for (index, buffer) in transfers.iter().enumerate() {
        writer.transfer_array_buffer(index as u32, *buffer);
    }
    if writer.write_value(context, value) != Some(true) {
        return None;
    }
    Some(writer.release())
}

fn decode<'s>(
    scope: &mut PinScope<'s, '_>,
    data: &[u8],
    transfers: &[Local<'_, ArrayBuffer>],
) -> Option<Local<'s, Value>> {
    let context = scope.get_current_context();
    let reader = v8::ValueDeserializer::new(scope, Box::new(Deserializer), data);
    if reader.read_header(context) != Some(true) {
        return None;
    }
    for (index, buffer) in transfers.iter().enumerate() {
        let receiving = ArrayBuffer::with_backing_store(scope, &buffer.get_backing_store());
        reader.transfer_array_buffer(index as u32, receiving);
    }
    reader.read_value(context)
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    for (name, callback) in [
        ("markBlob", Function::new(scope, mark_blob)),
        ("blobMetadata", Function::new(scope, blob_metadata)),
        ("serializeValue", Function::new(scope, serialize)),
        ("deserializeValue", Function::new(scope, deserialize)),
    ] {
        set(scope, host, name, callback?.into())?;
    }
    Some(())
}

fn blob_metadata(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Ok(object) = Local::<Object>::try_from(args.get(0))
        && let Some(metadata) = blob(scope, object)
    {
        result.set(metadata.into());
    }
}

fn mark_blob(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue<'_>,
) {
    if let Ok(object) = Local::<Object>::try_from(args.get(0)) {
        let marker = marker(scope);
        let metadata = v8::Array::new_with_elements(
            scope,
            &[args.get(1), args.get(2), args.get(3), args.get(4)],
        );
        object.set_private(scope, marker, metadata.into());
    }
}

fn serialize(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(data) = encode(scope, args.get(0), &[]).and_then(|bytes| bytes_value(scope, bytes))
    {
        result.set(data);
    }
}

fn deserialize(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    if let Some(data) = bytes(args.get(0))
        && let Some(value) = decode(scope, &data, &[])
    {
        result.set(value);
    }
}

pub(crate) fn clone_message(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let mut transfers = Vec::new();
    if let Ok(list) = Local::<v8::Array>::try_from(args.get(1)) {
        for index in 0..list.length() {
            let Some(buffer) = list
                .get_index(scope, index)
                .and_then(|value| Local::<ArrayBuffer>::try_from(value).ok())
            else {
                clone_error(scope, "Only ArrayBuffers can be transferred");
                return;
            };
            if buffer.was_detached() || !buffer.is_detachable() || transfers.contains(&buffer) {
                clone_error(scope, "Invalid or duplicate transferable");
                return;
            }
            transfers.push(buffer);
        }
    }
    if let Some(data) = encode(scope, args.get(0), &transfers)
        && let Some(value) = decode(scope, &data, &transfers)
    {
        for buffer in transfers {
            buffer.detach(None);
        }
        result.set(value);
    }
}
