// SPDX-License-Identifier: MIT

// Shipped Windows apps open no console window, following Tauri: release and
// dist builds are GUI-subsystem executables, and debug builds keep their
// console for development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::Parser;
use squark_host::{
    GpuAdapter, ScreenshotRequest, WindowRequest, render_headless, run_headless_control, run_window,
};

#[derive(Debug, Parser)]
#[command(name = "squark-runtime", version)]
struct Args {
    /// Standalone pack or executable with a pack trailer. Defaults to this
    /// executable. An app bundle, or the executable inside one, runs the
    /// bundle's pack.
    input: Option<PathBuf>,
    /// Render the initial frame offscreen.
    #[arg(long, requires = "out")]
    screenshot: bool,
    /// Path to write the screenshot PNG.
    #[arg(long, requires = "screenshot")]
    out: Option<PathBuf>,
    /// Override the initial window width in CSS pixels.
    #[arg(long)]
    width: Option<u32>,
    /// Override the initial window height in CSS pixels.
    #[arg(long)]
    height: Option<u32>,
    /// Override the window title from the pack.
    #[arg(long, conflicts_with_all = ["screenshot", "headless"])]
    title: Option<String>,
    #[arg(long, default_value_t = 1.0)]
    dpr: f64,
    #[arg(long, default_value = "auto")]
    gpu_adapter: GpuAdapter,
    #[arg(long, hide = true, conflicts_with = "screenshot")]
    exit_after_first_frame: bool,
    /// Enable the local test-control socket.
    #[arg(long, conflicts_with = "screenshot")]
    test_control: Option<PathBuf>,
    /// Run an interactive test session without a window.
    #[arg(long, requires = "test_control", conflicts_with_all = ["screenshot", "exit_after_first_frame"])]
    headless: bool,
    /// Print the licences of the third-party software in this runtime, and so
    /// in every app built from it, then exit.
    #[arg(long, exclusive = true)]
    third_party_notices: bool,
}

/// THIRD_PARTY_NOTICES.txt, which scripts/notices/generate.mjs writes. It
/// travels inside every app `squark build` makes from this runtime.
const THIRD_PARTY_NOTICES: &str = include_str!("../../../../../THIRD_PARTY_NOTICES.txt");

fn main() -> ExitCode {
    squark_host::process_start();
    let args = Args::parse();
    if args.third_party_notices {
        // A closed pipe or, for a Windows GUI app run without a redirect, a
        // missing stdout is not an error worth reporting.
        let _ = std::io::Write::write_all(
            &mut std::io::stdout().lock(),
            THIRD_PARTY_NOTICES.as_bytes(),
        );
        return ExitCode::SUCCESS;
    }
    let automated = args.screenshot || args.test_control.is_some();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let message = format!("squark-runtime: {error}");
            eprintln!("{message}");
            if !automated && !has_stderr() {
                show_error(&message);
            }
            ExitCode::FAILURE
        }
    }
}

/// Whether stderr reaches anything. A GUI-subsystem process started from
/// Explorer has no standard handles, so its errors would go nowhere; one
/// started by a harness or a console redirect still has a pipe or file.
#[cfg(windows)]
fn has_stderr() -> bool {
    use std::os::windows::io::AsRawHandle;
    !std::io::stderr().as_raw_handle().is_null()
}

#[cfg(not(windows))]
fn has_stderr() -> bool {
    true
}

/// Shows a startup failure to someone who has no console to read it in.
/// Never shown in automation, where a modal dialog would hang the run.
fn show_error(message: &str) {
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("squark-runtime")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// The pack inside a macOS app bundle, for an input that is the bundle
/// itself or the executable in its `Contents/MacOS`. The pack is
/// `Contents/Resources/app.sqpak`; `squark build` writes bundles on macOS.
fn bundle_pack(input: &Path) -> Option<PathBuf> {
    let contents = if input.is_dir() {
        input.join("Contents")
    } else if cfg!(target_os = "macos") {
        input
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))?
            .parent()?
            .to_path_buf()
    } else {
        return None;
    };
    let pack = contents
        .join("Resources")
        .join(squark_types::pack::BUNDLE_PACK_NAME);
    pack.is_file().then_some(pack)
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let input = match args.input {
        Some(path) => path,
        None => env::current_exe()?,
    };
    let input = bundle_pack(&input).unwrap_or(input);
    if let Some(out) = args.out {
        let png = render_headless(ScreenshotRequest {
            input,
            width: args.width.unwrap_or(800),
            height: args.height.unwrap_or(600),
            dpr: args.dpr,
            gpu_adapter: args.gpu_adapter,
        })?;
        fs::write(out, png)?;
    } else if args.headless {
        run_headless_control(
            ScreenshotRequest {
                input,
                width: args.width.unwrap_or(800),
                height: args.height.unwrap_or(600),
                dpr: args.dpr,
                gpu_adapter: args.gpu_adapter,
            },
            args.test_control
                .as_deref()
                .expect("clap requires --test-control"),
        )?;
    } else {
        run_window(WindowRequest {
            input,
            width: args.width,
            height: args.height,
            title: args.title,
            gpu_adapter: args.gpu_adapter,
            exit_after_first_frame: args.exit_after_first_frame,
            test_control: args.test_control,
        })?;
    }
    Ok(())
}
