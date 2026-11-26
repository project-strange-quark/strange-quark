// SPDX-License-Identifier: MIT
//! Integration tests for `squark screenshot`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use squark_cli::inspect::inspect_path;
use squark_types::pack::DOCUMENT_PATH;

fn squark_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_squark"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// Writes a mock runtime into `dir` and returns the path to launch: a Node
/// script that records its arguments one per line, optionally copies the pack
/// it was given, and writes `MOCKPNG` to `--out`. It is started through `sh`
/// on Unix and a `.cmd` file on Windows, which cannot run shell scripts.
fn write_mock_runtime(dir: &Path, args_log: &Path, copy_pack_to: Option<&Path>) -> PathBuf {
    let copy_line = match copy_pack_to {
        Some(dest) => format!("fs.copyFileSync(args[0], {:?});\n", dest.to_str().unwrap()),
        None => String::new(),
    };
    let script = dir.join("mock-runtime.mjs");
    fs::write(
        &script,
        format!(
            "import fs from \"node:fs\";\n\
             const args = process.argv.slice(2);\n\
             fs.writeFileSync({:?}, args.map((arg) => `${{arg}}\\n`).join(\"\"));\n\
             {copy_line}\
             fs.writeFileSync(args[args.indexOf(\"--out\") + 1], \"MOCKPNG\");\n",
            args_log.to_str().unwrap(),
        ),
    )
    .unwrap();
    if cfg!(windows) {
        let launcher = dir.join("mock-runtime.cmd");
        fs::write(&launcher, format!("@node \"{}\" %*\r\n", script.display())).unwrap();
        launcher
    } else {
        let launcher = dir.join("mock-runtime.sh");
        fs::write(
            &launcher,
            format!("#!/bin/sh\nexec node \"{}\" \"$@\"\n", script.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
        }
        launcher
    }
}

#[test]
fn screenshot_passes_sqpak_directly_to_runtime_with_flags() {
    let tmp = std::env::temp_dir().join(format!("squark-cli-shot-sqpak-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let args_log = tmp.join("args.txt");
    let mock_runtime = write_mock_runtime(&tmp, &args_log, None);

    let sqpak = repo_root().join("tests/fixtures/pack/basic.sqpak");
    let out_png = tmp.join("nested/out.png");

    let output = Command::new(squark_bin())
        .args([
            "screenshot",
            sqpak.to_str().unwrap(),
            "--out",
            out_png.to_str().unwrap(),
            "--width",
            "640",
            "--height",
            "480",
            "--dpr",
            "2",
            "--gpu-adapter",
            "software",
            "--runtime",
            mock_runtime.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "squark screenshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&out_png).unwrap(), b"MOCKPNG");

    let args: Vec<String> = fs::read_to_string(&args_log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        args,
        vec![
            sqpak.canonicalize().unwrap().to_str().unwrap().to_owned(),
            "--screenshot".to_owned(),
            "--out".to_owned(),
            out_png.to_str().unwrap().to_owned(),
            "--width".to_owned(),
            "640".to_owned(),
            "--height".to_owned(),
            "480".to_owned(),
            "--dpr".to_owned(),
            "2".to_owned(),
            "--gpu-adapter".to_owned(),
            "software".to_owned(),
        ]
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn screenshot_packs_r1_html_case_with_assets_only_and_cleans_up_temp_sqpak() {
    let tmp = std::env::temp_dir().join(format!("squark-cli-shot-html-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let args_log = tmp.join("args.txt");
    let captured_sqpak = tmp.join("captured.sqpak");
    let mock_runtime = write_mock_runtime(&tmp, &args_log, Some(&captured_sqpak));

    let html_case =
        repo_root().join("tests/reftests/cases/r1/01-inline/01-whitespace-across-spans.html");
    let out_png = tmp.join("case.png");

    let output = Command::new(squark_bin())
        .args([
            "screenshot",
            html_case.to_str().unwrap(),
            "--out",
            out_png.to_str().unwrap(),
            "--width",
            "800",
            "--height",
            "600",
            "--dpr",
            "1",
            "--runtime",
            mock_runtime.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "squark screenshot on HTML failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let args: Vec<String> = fs::read_to_string(&args_log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let temp_sqpak_path = PathBuf::from(&args[0]);
    assert!(
        !temp_sqpak_path.exists(),
        "temporary .sqpak {} should be removed after screenshot",
        temp_sqpak_path.display()
    );

    let info = inspect_path(&captured_sqpak).unwrap();
    let pack = info.pack.unwrap();
    let paths: Vec<&str> = pack.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&DOCUMENT_PATH));
    assert!(paths.contains(&"/_assets/r1.css"));
    assert!(paths.contains(&"/01-inline/01-whitespace-across-spans.html"));
    let html_entries: Vec<&&str> = paths.iter().filter(|p| p.ends_with(".html")).collect();
    assert_eq!(
        html_entries.len(),
        1,
        "other R1 HTML cases must not be packed into the temporary .sqpak"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn screenshot_rejects_invalid_gpu_adapter() {
    let sqpak = repo_root().join("tests/fixtures/pack/basic.sqpak");
    let output = Command::new(squark_bin())
        .args([
            "screenshot",
            sqpak.to_str().unwrap(),
            "--out",
            "/tmp/should-not-be-written.png",
            "--gpu-adapter",
            "bogus",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid --gpu-adapter"));
}

#[test]
fn screenshot_renders_real_runtime_fixture_when_gpu_tests_enabled() {
    if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
        return;
    }
    let tmp = std::env::temp_dir().join(format!("squark-cli-shot-gpu-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let fixture = repo_root().join("crates/squark-host/tests/fixtures/runtime.sqpak");
    let out_png = tmp.join("runtime.png");

    let mut cmd = Command::new(squark_bin());
    cmd.args([
        "screenshot",
        fixture.to_str().unwrap(),
        "--out",
        out_png.to_str().unwrap(),
        "--width",
        "80",
        "--height",
        "30",
        "--dpr",
        "1.5",
    ]);
    if let Ok(adapter) = std::env::var("SQUARK_GPU_ADAPTER") {
        cmd.args(["--gpu-adapter", &adapter]);
    }
    let output = cmd.output().unwrap();
    assert!(
        output.status.success(),
        "real squark screenshot failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let png_bytes = fs::read(&out_png).unwrap();
    // PNG signature + IHDR width/height bytes (big-endian u32 at offsets 16..20 and 20..24).
    assert!(png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let width = u32::from_be_bytes(png_bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(png_bytes[20..24].try_into().unwrap());
    assert_eq!((width, height), (120, 45));

    let _ = fs::remove_dir_all(&tmp);
}
