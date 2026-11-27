// SPDX-License-Identifier: MIT

//! JavaScript runtime support for Strange Quark.

#![allow(unsafe_code)]

mod bindings;
mod bitmap;
pub mod bootstrap;
mod canvas;
mod clone_value;
mod crypto;
mod css;
mod data;
pub mod document;
mod fonts;
mod input;
mod loader;
mod menu;
mod module_cache;
mod native;
mod navigation;
mod network;
mod observer;
mod platform;
mod scroll;
mod shutdown;
mod storage;
mod wasm;
mod worker;
mod xml;

pub mod task_loop;

/// Initializes V8 once for every unit test in this crate.
#[cfg(test)]
pub(crate) fn initialize_test_v8() {
    static V8: std::sync::Once = std::sync::Once::new();
    V8.call_once(|| {
        v8::V8::initialize_platform(v8::new_default_platform(0, false).make_shared());
        v8::V8::initialize();
    });
}

pub use canvas::CanvasBackend;
pub use data::{BlobAsset, BlobRegistry};
pub use loader::ScriptSources;
pub use shutdown::DocumentShutdown;
