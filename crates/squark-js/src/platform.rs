// SPDX-License-Identifier: MIT

use std::sync::mpsc::Sender;

use squark_types::{
    host::{ClipboardItem, FileHandle, HostError, HostRequest, HostValue},
    native::WindowOp,
};
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue, Value};

use crate::{
    bindings::{array, set, string, throw, unsigned},
    data,
    network::{bytes, bytes_value},
};

pub(crate) fn install(
    scope: &mut PinScope<'_, '_>,
    host: Local<'_, Object>,
    sender: Option<Sender<HostRequest>>,
) -> Option<()> {
    if let Some(sender) = sender {
        scope.set_slot(sender);
        for (name, callback) in [
            ("clipboardRead", Function::new(scope, clipboard_read)),
            ("clipboardWrite", Function::new(scope, clipboard_write)),
            ("fileOpen", Function::new(scope, file_open)),
            ("fileSave", Function::new(scope, file_save)),
            ("openExternal", Function::new(scope, open_external)),
            ("fullscreen", Function::new(scope, fullscreen)),
            ("windowOp", Function::new(scope, window_op)),
        ] {
            set(scope, host, name, callback?.into())?;
        }
    }
    Some(())
}

fn send(scope: &mut PinScope<'_, '_>, request: Option<HostRequest>) {
    let Some(request) = request else {
        throw(scope, "invalid host service request", false);
        return;
    };
    if scope
        .get_slot::<Sender<HostRequest>>()
        .is_none_or(|sender| sender.send(request).is_err())
    {
        throw(scope, "host services disconnected", false);
    }
}

fn clipboard_read(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue,
) {
    send(
        scope,
        unsigned(args.get(0)).map(|id| HostRequest::ClipboardRead { id }),
    );
}

fn clipboard_write(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue,
) {
    let request = (|| {
        let id = unsigned(args.get(0))?;
        let items = Local::<v8::Array>::try_from(args.get(1)).ok()?;
        let items = (0..items.length())
            .map(|index| {
                let entry = Local::<v8::Array>::try_from(items.get_index(scope, index)?).ok()?;
                let mime = entry.get_index(scope, 0)?;
                Some(ClipboardItem {
                    mime: string(scope, mime)?,
                    bytes: bytes(entry.get_index(scope, 1)?)?.into(),
                })
            })
            .collect::<Option<Box<[_]>>>()?;
        Some(HostRequest::ClipboardWrite { id, items })
    })();
    send(scope, request);
}

fn file_open(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue) {
    let request = (|| {
        let id = unsigned(args.get(0))?;
        let accept = Local::<v8::Array>::try_from(args.get(1)).ok()?;
        let multiple = Local::<v8::Boolean>::try_from(args.get(2)).ok()?.is_true();
        let accept = (0..accept.length())
            .map(|index| {
                let entry = accept.get_index(scope, index)?;
                string(scope, entry)
            })
            .collect::<Option<Box<[_]>>>()?;
        Some(HostRequest::FileOpen {
            id,
            accept,
            multiple,
        })
    })();
    send(scope, request);
}

fn file_save(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue) {
    let request = (|| {
        let id = unsigned(args.get(0))?;
        let url = string(scope, args.get(1))?;
        let suggested_name = string(scope, args.get(2))?;
        // Capture the blob before click() returns so revocation cannot race the dialog.
        let data::BlobAsset { mime, bytes } = data::registry(scope).resolve(&url)?;
        Some(HostRequest::FileSave {
            id,
            suggested_name,
            mime,
            bytes,
        })
    })();
    send(scope, request);
}

fn open_external(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    _: ReturnValue,
) {
    let request = (|| {
        let id = unsigned(args.get(0))?;
        let url = string(scope, args.get(1))?;
        let url = url::Url::parse(&url).ok()?;
        matches!(url.scheme(), "http" | "https" | "mailto" | "tel").then(|| {
            HostRequest::OpenExternal {
                id,
                url: url.into(),
            }
        })
    })();
    send(scope, request);
}

fn fullscreen(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue) {
    let request = unsigned(args.get(0)).and_then(|id| {
        let enter = Local::<v8::Boolean>::try_from(args.get(1)).ok()?.is_true();
        Some(HostRequest::Fullscreen { id, enter })
    });
    send(scope, request);
}

fn window_op(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue) {
    let mut invalid = None;
    let request = (|| {
        let id = unsigned(args.get(0))?;
        let op = string(scope, args.get(1))?;
        let value = args.get(2);
        let op = match op.as_str() {
            "title" => WindowOp::SetTitle(string(scope, value)?),
            "zoom" => {
                let factor = value.number_value(scope)?;
                (factor.is_finite() && factor > 0.0).then_some(WindowOp::SetZoom(factor))?
            }
            "close" => WindowOp::Close,
            "relaunch" => WindowOp::Relaunch,
            "exit" => WindowOp::Exit(value.int32_value(scope).unwrap_or(0)),
            "menu" => match crate::menu::parse_menu(&string(scope, value)?) {
                Ok(menu) => WindowOp::SetMenu(menu),
                Err(message) => {
                    invalid = Some(format!("invalid menu: {message}"));
                    return None;
                }
            },
            _ => return None,
        };
        Some(HostRequest::Window { id, op })
    })();
    if let Some(message) = invalid {
        throw(scope, &message, false);
        return;
    }
    send(scope, request);
}

pub(crate) fn reply<'s>(
    scope: &mut PinScope<'s, '_>,
    id: u32,
    result: Result<HostValue, HostError>,
) -> Option<[Local<'s, Value>; 3]> {
    let request = v8::Integer::new_from_unsigned(scope, id);
    let ok = v8::Boolean::new(scope, result.is_ok());
    let payload = match result {
        Ok(HostValue::Clipboard(items)) => array(scope, items.into_vec(), |scope, item| {
            let mime = v8::String::new(scope, &item.mime)?;
            let bytes = bytes_value(scope, item.bytes.to_vec())?;
            Some(v8::Array::new_with_elements(scope, &[mime.into(), bytes]).into())
        })?
        .into(),
        Ok(HostValue::Files(files)) => array(scope, files.into_vec(), file)?.into(),
        Ok(HostValue::Done) => v8::undefined(scope).into(),
        Ok(HostValue::Cancelled) => v8::null(scope).into(),
        Err(error) => {
            let name = v8::String::new(scope, &error.name)?;
            let message = v8::String::new(scope, &error.message)?;
            v8::Array::new_with_elements(scope, &[name.into(), message.into()]).into()
        }
    };
    Some([request.into(), ok.into(), payload])
}

fn file<'s>(scope: &mut PinScope<'s, '_>, file: FileHandle) -> Option<Local<'s, Value>> {
    let name = v8::String::new(scope, &file.name)?;
    let mime = v8::String::new(scope, &file.mime)?;
    let modified = v8::Number::new(scope, file.last_modified_ms as f64);
    let bytes = bytes_value(scope, file.bytes.to_vec())?;
    Some(
        v8::Array::new_with_elements(scope, &[name.into(), mime.into(), modified.into(), bytes])
            .into(),
    )
}
