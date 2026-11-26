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
mod module_cache;
mod navigation;
mod network;
mod observer;
mod platform;
mod scroll;
mod shutdown;
mod storage;
mod wasm;
mod xml;

pub mod task_loop;

pub use canvas::CanvasBackend;
pub use data::{BlobAsset, BlobRegistry};
pub use loader::ScriptSources;
pub use shutdown::DocumentShutdown;
