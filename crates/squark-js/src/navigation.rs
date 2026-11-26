// SPDX-License-Identifier: MIT

use url::{Url, quirks};
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue};

use crate::bindings::{set, string};

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    let parse = Function::new(scope, parse_url)?;
    set(scope, host, "parseURL", parse.into())?;
    let update = Function::new(scope, update_url)?;
    set(scope, host, "updateURL", update.into())?;
    let warn = Function::new(scope, warn)?;
    set(scope, host, "navigationWarning", warn.into())
}

fn warn(scope: &mut PinScope<'_, '_>, args: FunctionCallbackArguments<'_>, _: ReturnValue<'_>) {
    if let Some(message) = string(scope, args.get(0)) {
        eprintln!("squark-js: {message}");
    }
}

fn parse_url(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let Some(input) = string(scope, args.get(0)) else {
        return;
    };
    let url = match string(scope, args.get(1)) {
        Some(base) => Url::parse(&base).and_then(|base| base.join(&input)),
        None => Url::parse(&input),
    };
    let Ok(url) = url else {
        return;
    };
    if let Some(object) = parsed(scope, &url) {
        result.set(object.into());
    }
}

fn update_url(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let (Some(href), Some(field), Some(value)) = (
        string(scope, args.get(0)),
        string(scope, args.get(1)),
        string(scope, args.get(2)),
    ) else {
        return;
    };
    let Ok(mut url) = Url::parse(&href) else {
        return;
    };
    // WHATWG's component setters ignore values invalid for the current URL.
    match field.as_str() {
        "protocol" => _ = quirks::set_protocol(&mut url, &value),
        "username" => _ = quirks::set_username(&mut url, &value),
        "password" => _ = quirks::set_password(&mut url, &value),
        "host" => _ = quirks::set_host(&mut url, &value),
        "hostname" => _ = quirks::set_hostname(&mut url, &value),
        "port" => _ = quirks::set_port(&mut url, &value),
        "pathname" => quirks::set_pathname(&mut url, &value),
        "search" => quirks::set_search(&mut url, &value),
        "hash" => quirks::set_hash(&mut url, &value),
        _ => return,
    }
    if let Some(object) = parsed(scope, &url) {
        result.set(object.into());
    }
}

fn parsed<'s>(scope: &mut PinScope<'s, '_>, url: &Url) -> Option<Local<'s, Object>> {
    let object = Object::new(scope);
    for (key, value) in [
        ("href", url.as_str()),
        ("origin", &quirks::origin(url)),
        ("protocol", quirks::protocol(url)),
        ("host", quirks::host(url)),
        ("hostname", quirks::hostname(url)),
        ("port", quirks::port(url)),
        ("pathname", quirks::pathname(url)),
        ("search", quirks::search(url)),
        ("hash", quirks::hash(url)),
        ("username", url.username()),
        ("password", url.password().unwrap_or_default()),
    ] {
        let value = v8::String::new(scope, value)?;
        set(scope, object, key, value.into())?;
    }
    Some(object)
}
