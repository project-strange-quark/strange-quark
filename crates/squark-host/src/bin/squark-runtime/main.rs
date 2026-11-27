// SPDX-License-Identifier: MIT

// Shipped Windows apps open no console window, following Tauri: release and
// dist builds are GUI-subsystem executables, and debug builds keep their
// console for development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    squark_host::runtime_main(None)
}
