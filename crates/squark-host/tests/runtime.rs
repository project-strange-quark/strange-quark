// SPDX-License-Identifier: MIT

#[path = "support/timeout.rs"]
mod timeout;

use std::{io::Cursor, path::PathBuf, process::Command, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use squark_host::{GpuAdapter, ScreenshotRequest, render_headless};

fn pack() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime.sqpak")
}

fn request() -> ScreenshotRequest {
    ScreenshotRequest {
        input: pack(),
        width: 40,
        height: 30,
        dpr: 1.5,
        gpu_adapter: GpuAdapter::Auto,
        native: None,
    }
}

#[test]
fn rejects_an_invalid_viewport_before_initializing_vulkan() {
    let error = render_headless(ScreenshotRequest {
        dpr: f64::NAN,
        ..request()
    })
    .unwrap_err();
    assert_eq!(error.name, "ViewportError");
}

#[test]
fn rejects_a_non_pack_before_initializing_vulkan() {
    let error = render_headless(ScreenshotRequest {
        input: pack().with_file_name("runtime-pack").join("index.html"),
        ..request()
    })
    .unwrap_err();
    assert_eq!(error.name, "PackError");
}

fn png_pixels(png: &[u8], dimensions: [u32; 2]) -> Vec<[u8; 4]> {
    let mut reader = png::Decoder::new(Cursor::new(png)).read_info().unwrap();
    let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut pixels).unwrap();
    assert_eq!([info.width, info.height], dimensions);
    assert_eq!(info.color_type, png::ColorType::Rgba);
    pixels[..info.buffer_size()].as_chunks::<4>().0.to_vec()
}

#[test]
fn screenshot_uses_the_packed_cascade_and_css_viewport_at_dpr() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let actual = render_headless(request()).unwrap();
    let pixels = png_pixels(&actual, [60, 45]);
    assert_eq!(pixels[22 * 60 + 30], [0x12, 0x34, 0x56, 255]);
    assert_eq!(pixels[3 * 60 + 5], [0xff, 0xc8, 0x00, 255]);
    assert_eq!(pixels[5 * 60 + 5], [0xa0, 0x00, 0x20, 255]);
    assert_eq!(pixels[33 * 60 + 6], [0x48, 0xc0, 0x20, 255]);
    assert!(pixels.as_chunks::<60>().0[5..24].iter().any(|row| {
        row[19..50]
            .iter()
            .any(|&[r, g, b, _]| r > 200 && g > 200 && b > 200)
    }));
    let actual = render_headless(ScreenshotRequest {
        width: 80,
        ..request()
    })
    .unwrap();
    assert_eq!(
        png_pixels(&actual, [120, 45])[22 * 120 + 60],
        [0x21, 0x52, 0x2a, 255]
    );
    let retina = render_headless(ScreenshotRequest {
        dpr: 2.0,
        ..request()
    })
    .unwrap();
    let retina_pixels = png_pixels(&retina, [80, 60]);
    assert_eq!(retina_pixels[30 * 80 + 40], [0x12, 0x34, 0x56, 255]);
    assert_eq!(retina_pixels[4 * 80 + 7], [0xff, 0xc8, 0x00, 255]);
}

#[test]
fn runtime_cli_validates_its_screenshot_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_squark-runtime"))
        .args(["--screenshot"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--out"));
}

#[test]
fn packed_window_presents_a_frame_and_can_exit_cleanly() {
    if std::env::var("SQUARK_WINDOW_TESTS").as_deref() != Ok("1") {
        return;
    }
    let output = timeout::output(
        Command::new(env!("CARGO_BIN_EXE_squark-runtime"))
            .env("SQUARK_STARTUP_PROFILE", "1")
            .arg(pack())
            .args([
                "--width",
                "100",
                "--height",
                "75",
                "--exit-after-first-frame",
            ]),
        Duration::from_secs(25),
    )
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("squark-runtime presented its first frame"),
        "{stderr}"
    );
    let initial_tick = stderr
        .find("squark-startup initial-frame-tick ")
        .expect(&stderr);
    let gpu_setup = stderr.find("squark-startup adapter ").expect(&stderr);
    assert!(initial_tick < gpu_setup, "{stderr}");
    if cfg!(target_os = "linux") {
        let surface = stderr
            .find("squark-startup instance-and-surface ")
            .expect(&stderr);
        assert!(initial_tick < surface, "{stderr}");
    }
}

#[test]
fn window_startup_failure_reports_the_gpu_error() {
    if std::env::var("SQUARK_WINDOW_TESTS").as_deref() != Ok("1") {
        return;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_squark-runtime"));
    #[cfg(not(target_os = "macos"))]
    command
        .env(
            "VK_ICD_FILENAMES",
            pack().with_file_name("missing-icd.json"),
        )
        .env_remove("VK_DRIVER_FILES");
    #[cfg(target_os = "macos")]
    command.args(["--gpu-adapter", "software"]);
    let output = timeout::output(
        command
            .arg(pack().with_file_name("runtime-canvas.sqpak"))
            .arg("--exit-after-first-frame"),
        Duration::from_secs(10),
    )
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("squark-runtime: GpuError:"), "{stderr}");
}

#[test]
fn wayland_presents_through_fifty_native_resizes() {
    if std::env::var("SQUARK_WINDOW_TESTS").as_deref() != Ok("1")
        || std::env::var("SQUARK_WAYLAND_RESIZE_TESTS").as_deref() != Ok("1")
    {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let output = timeout::output(
        Command::new("node")
            .arg(fixtures.join("resize-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(pack()),
        Duration::from_secs(45),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_large_visible_document_reaches_idle_after_late_commits() {
    if std::env::var("SQUARK_WINDOW_TESTS").as_deref() != Ok("1") {
        return;
    }
    let pack = pack();
    let output = timeout::output(
        Command::new("node")
            .arg(pack.with_file_name("window-idle-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(pack),
        Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn screenshot_commits_the_packed_scripts_and_first_animation_frame() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let actual = render_headless(ScreenshotRequest {
        input: pack().with_file_name("runtime-js.sqpak"),
        ..request()
    })
    .unwrap();
    assert_eq!(
        png_pixels(&actual, [60, 45])[22 * 60 + 30],
        [0x24, 0x68, 0xac, 255]
    );
}

#[test]
fn screenshot_reads_and_encodes_canvas_before_the_first_commit() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let actual = render_headless(ScreenshotRequest {
        input: pack().with_file_name("runtime-canvas.sqpak"),
        ..request()
    })
    .unwrap();
    assert_eq!(
        png_pixels(&actual, [60, 45])[22 * 60 + 30],
        [0x24, 0x68, 0xac, 255]
    );
}

fn canvas_readback_driver(mode: &str) {
    let pack = pack().with_file_name("runtime-canvas.sqpak");
    let output = timeout::output(
        Command::new("node")
            .arg(pack.with_file_name("canvas-readback-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(pack)
            .arg(mode),
        Duration::from_secs(35),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn headless_canvas_readback_runs_between_commits() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() == Ok("1") {
        canvas_readback_driver("headless");
    }
}

#[test]
fn windowed_canvas_readback_runs_between_commits() {
    if std::env::var("SQUARK_WINDOW_TESTS").as_deref() == Ok("1") {
        canvas_readback_driver("windowed");
    }
}

#[test]
fn squark_driver_controls_the_real_runtime() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-control-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("control-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-control.sqpak"))
            .arg(&output),
        Duration::from_secs(55),
    )
    .unwrap();
    // On Windows the socket is a named pipe, not a file in the output dir.
    assert!(
        cfg!(windows) || !output.join("squark.sock").exists(),
        "test-control socket was not removed"
    );
    std::fs::remove_dir_all(output).unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn app_quit_stops_a_busy_document_before_render_teardown() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixture = pack();
    let fixtures = fixture.parent().unwrap();
    let output = timeout::output(
        Command::new("node")
            .arg(fixtures.join("shutdown-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-shutdown.sqpak")),
        Duration::from_secs(12),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn headless_ime_control_dispatches_composition_and_end_events() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-ime-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("ime-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-ime.sqpak"))
            .arg(&output),
        Duration::from_secs(55),
    )
    .unwrap();
    // On Windows the socket is a named pipe, not a file in the output dir.
    assert!(
        cfg!(windows) || !output.join("squark.sock").exists(),
        "test-control socket was not removed"
    );
    std::fs::remove_dir_all(output).unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn headless_fullscreen_requests_record_state_and_fire_change_events() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-fullscreen-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("fullscreen-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-fullscreen.sqpak"))
            .arg(&output),
        Duration::from_secs(55),
    )
    .unwrap();
    // On Windows the socket is a named pipe, not a file in the output dir.
    let socket_left_behind = cfg!(unix) && output.join("squark.sock").exists();
    std::fs::remove_dir_all(output).unwrap();
    assert!(!socket_left_behind, "test-control socket was not removed");
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn headless_menu_accelerators_run_unless_the_page_cancels_the_key() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-menu-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("menu-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-menu.sqpak"))
            .arg(&output),
        Duration::from_secs(55),
    )
    .unwrap();
    let socket_left_behind = cfg!(unix) && output.join("squark.sock").exists();
    std::fs::remove_dir_all(output).unwrap();
    assert!(!socket_left_behind, "test-control socket was not removed");
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

fn run_blob_driver(mode: &str) {
    let fixtures = pack().parent().unwrap().to_owned();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-blob-{mode}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("blob-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-blob-test.sqpak"))
            .arg(&output)
            .arg(mode),
        Duration::from_secs(55),
    )
    .unwrap();
    // On Windows the socket is a named pipe, not a file in the output dir.
    assert!(
        cfg!(windows) || !output.join("squark.sock").exists(),
        "test-control socket was not removed"
    );
    std::fs::remove_dir_all(output).unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

fn run_host_service_driver() {
    let fixtures = pack().parent().unwrap().to_owned();
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-host-service-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("host-service-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-host-service.sqpak"))
            .arg(&output),
        Duration::from_secs(55),
    )
    .unwrap();
    // On Windows the socket is a named pipe, not a file in the output dir.
    assert!(
        cfg!(windows) || !output.join("squark.sock").exists(),
        "test-control socket was not removed"
    );
    std::fs::remove_dir_all(output).unwrap();
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn blob_images_load_with_their_intrinsic_size() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    run_blob_driver("load");
}

#[test]
fn host_requests_open_only_external_links() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    run_host_service_driver();
}

#[test]
fn test_control_clipboard_round_trips_text_and_pastes_png() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let png = clipboard_png();
    let output = timeout::output(
        Command::new("node")
            .arg(fixtures.join("clipboard-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-clipboard.sqpak"))
            .arg(STANDARD.encode(png)),
        Duration::from_secs(55),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_control_file_dialogs_open_save_and_cancel() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let fixtures = pack().parent().unwrap().to_owned();
    let output_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../target/squark-host-file-dialog-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&output_dir).unwrap();
    let run = timeout::output(
        Command::new("node")
            .arg(fixtures.join("file-dialog-driver.mjs"))
            .arg(env!("CARGO_BIN_EXE_squark-runtime"))
            .arg(fixtures.join("runtime-file-dialog.sqpak"))
            .arg(fixtures.join("file-dialog/open-me.txt"))
            .arg(&output_dir),
        Duration::from_secs(55),
    )
    .unwrap();
    // On Windows the socket is a named pipe, not a file in the output dir.
    let socket_left_behind = cfg!(unix) && output_dir.join("squark.sock").exists();
    std::fs::remove_dir_all(&output_dir).unwrap();
    assert!(!socket_left_behind, "test-control socket was not removed");
    assert!(
        run.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
}

fn clipboard_png() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().unwrap();
    writer
        .write_image_data(&[0x12, 0x34, 0x56, 0xff, 0x65, 0x43, 0x21, 0xff])
        .unwrap();
    drop(writer);
    bytes
}

// Chromium 159 fires `error` for a new <img> whose src is a revoked blob URL,
// even when the same URL loaded earlier in the document.
#[test]
fn revoked_blob_urls_fail_to_load() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    run_blob_driver("revoke");
}
