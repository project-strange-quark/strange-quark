// SPDX-License-Identifier: MIT
//! Integration tests for `squark build` orchestration (config loading, app build,
//! `squark pack`, and appending to a placeholder runtime binary).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use squark_cli::inspect::{ContainerKind, inspect_path};

fn squark_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_squark"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn write_placeholder_runtime(path: &Path) {
    let bytes = b"#!/bin/sh\n# SQUARK-PLACEHOLDER-RUNTIME\n".repeat(4);
    fs::write(path, bytes).unwrap();
}

#[test]
fn squark_build_runs_app_build_with_squark_config_ts_and_assembles_binary() {
    let tmp = std::env::temp_dir().join(format!("squark-build-cfg-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let runtime = tmp.join("placeholder-runtime");
    write_placeholder_runtime(&runtime);

    // Mini project whose `squark.config.ts` runs a custom build command emitting into `build/`
    // (mirroring Excalidraw's `build/` outDir) and passing an env flag. The command is a
    // Node script so it runs the same under `sh -c` and Windows' `cmd /C`.
    let app_dir = tmp.join("sample-app");
    fs::create_dir_all(&app_dir).unwrap();
    fs::write(
        app_dir.join("build.mjs"),
        r#"import fs from "node:fs";
fs.mkdirSync("build/assets", { recursive: true });
fs.writeFileSync(
  "build/index.html",
  `<!DOCTYPE html><html><head><link rel="stylesheet" href="/assets/app.css"></head><body><div id="root">${process.env.SQUARK_BUILD_BANNER}</div><script type="module" src="/assets/main.js"></script></body></html>`,
);
fs.writeFileSync("build/assets/app.css", "body{margin:0}\n");
fs.writeFileSync("build/assets/main.js", "export const ok = true;\n");
"#,
    )
    .unwrap();
    fs::write(
        app_dir.join("squark.config.ts"),
        r#"export default {
  name: "sample-native",
  build: {
    command: "node build.mjs",
    env: { SQUARK_BUILD_BANNER: "from-config-env" },
    distDir: "build",
  },
};
"#,
    )
    .unwrap();

    let out = Command::new(squark_bin())
        .args([
            "build",
            app_dir.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "squark build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        fs::read_to_string(app_dir.join("build/index.html"))
            .unwrap()
            .contains("from-config-env"),
        "the config's build env must reach the build command"
    );

    let out_exe = app_dir.join("dist/sample-native");
    assert!(
        out_exe.is_file(),
        "expected output binary at {}",
        out_exe.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&out_exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "built binary must be executable");
    }

    let info = inspect_path(&out_exe).unwrap();
    assert_eq!(info.container, ContainerKind::AppendedBinary);
    let trailer = info.trailer.unwrap();
    assert_eq!(trailer.pack_offset, 4096);
    assert!(trailer.crc_valid);
    let pack = info.pack.unwrap();
    let paths: Vec<&str> = pack.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"/__squark/document.bin"));
    assert!(paths.contains(&"/__squark/manifest.json"));
    assert!(paths.contains(&"/assets/app.css"));
    assert!(paths.contains(&"/assets/main.js"));
    let doc = info.document.unwrap();
    assert_eq!(doc.scripts.len(), 2);

    // Also verify `--skip-app-build --out <custom>` works without re-running build command.
    let custom_out = tmp.join("custom-out-bin");
    let skip_out = Command::new(squark_bin())
        .args([
            "build",
            app_dir.to_str().unwrap(),
            "--skip-app-build",
            "--runtime",
            runtime.to_str().unwrap(),
            "--out",
            custom_out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        skip_out.status.success(),
        "squark build --skip-app-build failed: {}",
        String::from_utf8_lossy(&skip_out.stderr)
    );
    assert_eq!(
        inspect_path(&custom_out).unwrap().container,
        ContainerKind::AppendedBinary
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn squark_build_runs_a_configured_command_that_starts_and_ends_with_quotes() {
    let tmp = std::env::temp_dir().join(format!("squark-build-quoted-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let app_dir = tmp.join("quoted app");
    fs::create_dir_all(&app_dir).unwrap();
    let runtime = tmp.join("placeholder-runtime");
    write_placeholder_runtime(&runtime);
    fs::write(
        app_dir.join("build.mjs"),
        r#"import fs from "node:fs";
fs.mkdirSync("build", { recursive: true });
fs.writeFileSync("build/index.html", `<!DOCTYPE html><p>${process.argv[2]}</p>`);
"#,
    )
    .unwrap();
    // A quoted program and a quoted argument: cmd keeps this line intact only
    // when it is run as `cmd /S /C "<line>"`.
    fs::write(
        app_dir.join("squark.config.ts"),
        r#"export default {
  name: "quoted",
  build: { command: '"node" "build.mjs" "quoted arg"', distDir: "build" },
};
"#,
    )
    .unwrap();

    let out = Command::new(squark_bin())
        .args([
            "build",
            app_dir.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "squark build with a quoted command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        fs::read_to_string(app_dir.join("build/index.html"))
            .unwrap()
            .contains("quoted arg")
    );
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn squark_build_packages_real_vite_counter_app_with_placeholder_runtime() {
    let tmp = std::env::temp_dir().join(format!("squark-build-counter-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();

    let runtime = tmp.join("placeholder-runtime");
    write_placeholder_runtime(&runtime);
    let out_exe = tmp.join("counter-native");
    let counter_dir = repo_root().join("apps/counter");

    let out = Command::new(squark_bin())
        .args([
            "build",
            counter_dir.to_str().unwrap(),
            "--runtime",
            runtime.to_str().unwrap(),
            "--out",
            out_exe.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "squark build apps/counter failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let info = inspect_path(&out_exe).unwrap();
    assert_eq!(info.container, ContainerKind::AppendedBinary);
    assert!(info.trailer.unwrap().crc_valid);
    let pack = info.pack.unwrap();
    assert!(
        pack.entries
            .iter()
            .any(|e| e.path == "/__squark/document.bin")
    );
    assert!(
        pack.entries
            .iter()
            .any(|e| e.path.starts_with("/assets/") && e.path.ends_with(".js"))
    );
    assert!(
        pack.entries
            .iter()
            .any(|e| e.path.starts_with("/assets/") && e.path.ends_with(".css"))
    );

    // No `squark.config` window block: the app name and the documented 1280x800
    // defaults still reach the manifest, instead of the runtime's own title.
    let manifest = read_manifest_from_binary(&out_exe);
    assert_eq!(manifest["window"]["title"], "app-counter");
    assert_eq!(manifest["window"]["width"], 1280);
    assert_eq!(manifest["window"]["height"], 800);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn squark_build_resolves_cli_path_flags_against_cwd_and_writes_network_allow_manifest() {
    let tmp = std::env::temp_dir().join(format!("squark-build-cwd-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("bin")).unwrap();
    fs::create_dir_all(tmp.join("cfg")).unwrap();
    fs::create_dir_all(tmp.join("prebuilt-dist")).unwrap();
    fs::create_dir_all(tmp.join("apps/nested")).unwrap();

    write_placeholder_runtime(&tmp.join("bin/squark-runtime"));
    fs::write(
        tmp.join("prebuilt-dist/index.html"),
        "<!DOCTYPE html><html><body><p>cwd</p></body></html>",
    )
    .unwrap();
    fs::write(
        tmp.join("cfg/squark.config.json"),
        r#"{
  "name": "cwd-app",
  "network": {
    "allow": ["https://api.example.test", "wss://live.example.test:8443"]
  }
}"#,
    )
    .unwrap();

    // Every path flag is relative to `tmp` (CWD), NOT to `apps/nested`.
    let out = Command::new(squark_bin())
        .current_dir(&tmp)
        .args([
            "build",
            "apps/nested",
            "--skip-app-build",
            "--config",
            "cfg/squark.config.json",
            "--dist",
            "prebuilt-dist",
            "--runtime",
            "bin/squark-runtime",
            "--out",
            "out/cwd-app",
            "--sqpak-out",
            "out/cwd-app.sqpak",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "squark build with CWD-relative flags failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let out_exe = tmp.join("out/cwd-app");
    let out_sqpak = tmp.join("out/cwd-app.sqpak");
    assert!(
        out_exe.is_file(),
        "expected binary at CWD-relative out/cwd-app"
    );
    assert!(
        out_sqpak.is_file(),
        "expected sqpak at CWD-relative out/cwd-app.sqpak"
    );

    let info = inspect_path(&out_exe).unwrap();
    assert_eq!(info.container, ContainerKind::AppendedBinary);
    let pack = info.pack.unwrap();
    let manifest_entry = pack
        .entries
        .iter()
        .find(|e| e.path == "/__squark/manifest.json")
        .expect("manifest entry");
    let sqpak_bytes = fs::read(&out_sqpak).unwrap();
    let start = pack.data_offset as usize + manifest_entry.data_off as usize;
    let end = start + manifest_entry.data_len as usize;
    let manifest: serde_json::Value = serde_json::from_slice(&sqpak_bytes[start..end]).unwrap();
    assert_eq!(manifest["app_name"], "cwd-app");
    assert_eq!(
        manifest["network"]["allow"],
        serde_json::json!(["https://api.example.test", "wss://live.example.test:8443"])
    );

    let _ = fs::remove_dir_all(&tmp);
}

/// Reads `/__squark/manifest.json` out of an appended runtime binary.
fn read_manifest_from_binary(path: &Path) -> serde_json::Value {
    let info = inspect_path(path).unwrap();
    let trailer = info.trailer.unwrap();
    let pack = info.pack.unwrap();
    let entry = pack
        .entries
        .iter()
        .find(|entry| entry.path == "/__squark/manifest.json")
        .expect("manifest entry");
    let bytes = fs::read(path).unwrap();
    let start = trailer.pack_offset as usize + pack.data_offset as usize + entry.data_off as usize;
    let end = start + entry.data_len as usize;
    serde_json::from_slice(&bytes[start..end]).unwrap()
}

#[test]
fn squark_build_writes_the_window_block_from_squark_config() {
    let tmp = std::env::temp_dir().join(format!("squark-build-window-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("app/dist")).unwrap();
    write_placeholder_runtime(&tmp.join("runtime"));

    fs::write(
        tmp.join("app/dist/index.html"),
        "<!DOCTYPE html><html><body><p>window</p></body></html>",
    )
    .unwrap();
    fs::write(
        tmp.join("app/squark.config.json"),
        r#"{
  "name": "window-app",
  "window": { "title": "Acme Console", "width": 1024, "height": 768 }
}"#,
    )
    .unwrap();

    let out = Command::new(squark_bin())
        .current_dir(&tmp)
        .args([
            "build",
            "app",
            "--skip-app-build",
            "--runtime",
            "runtime",
            "--out",
            "out/window-app",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "squark build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let manifest = read_manifest_from_binary(&tmp.join("out/window-app"));
    assert_eq!(manifest["app_name"], "window-app");
    assert_eq!(manifest["window"]["title"], "Acme Console");
    assert_eq!(manifest["window"]["width"], 1024);
    assert_eq!(manifest["window"]["height"], 768);
    // The reported build size is part of the CLI output.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MiB"),
        "squark build must report the binary size: {stdout}"
    );

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn squark_build_skip_app_build_never_packs_a_previous_appended_runtime() {
    let tmp = std::env::temp_dir().join(format!("squark-build-repack-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("app/dist")).unwrap();
    write_placeholder_runtime(&tmp.join("runtime"));
    fs::write(
        tmp.join("app/dist/index.html"),
        "<!DOCTYPE html><html><body><div id=\"root\"></div></body></html>",
    )
    .unwrap();
    fs::write(tmp.join("app/dist/main.js"), "export const ok = true;\n").unwrap();

    // First build puts an appended runtime inside the web directory.
    let first = Command::new(squark_bin())
        .args([
            "build",
            tmp.join("app").to_str().unwrap(),
            "--skip-app-build",
            "--dist",
            tmp.join("app/dist").to_str().unwrap(),
            "--runtime",
            tmp.join("runtime").to_str().unwrap(),
            "--out",
            tmp.join("app/dist/app-v1").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "first build failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );

    // The second build must not pack `app-v1` (a 229 MB runtime would triple the output).
    let second = Command::new(squark_bin())
        .args([
            "build",
            tmp.join("app").to_str().unwrap(),
            "--skip-app-build",
            "--dist",
            tmp.join("app/dist").to_str().unwrap(),
            "--runtime",
            tmp.join("runtime").to_str().unwrap(),
            "--out",
            tmp.join("app/dist/app-v2").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "second build failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let info = inspect_path(&tmp.join("app/dist/app-v2")).unwrap();
    let paths: Vec<String> = info
        .pack
        .unwrap()
        .entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert!(
        !paths.iter().any(|path| path == "/app-v1"),
        "previous build output was packed: {paths:?}"
    );
    assert!(paths.iter().any(|path| path == "/main.js"));
    assert!(paths.iter().any(|path| path == "/index.html"));

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn squark_build_packs_declared_assets_at_their_exact_paths() {
    let tmp = std::env::temp_dir().join(format!("squark-build-assets-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(tmp.join("app/dist")).unwrap();
    fs::create_dir_all(tmp.join("cfg")).unwrap();
    write_placeholder_runtime(&tmp.join("runtime"));
    fs::write(
        tmp.join("app/dist/index.html"),
        "<!DOCTYPE html><html><body><p>assets</p></body></html>",
    )
    .unwrap();
    fs::write(tmp.join("cfg/extra.woff2"), b"font-bytes").unwrap();
    // `assets[].file` resolves against the config file's directory.
    fs::write(
        tmp.join("cfg/squark.config.json"),
        r#"{
  "name": "asset-app",
  "assets": [
    { "path": "/assets/Assistant-Bold.woff2", "file": "extra.woff2" }
  ]
}"#,
    )
    .unwrap();

    let out = Command::new(squark_bin())
        .current_dir(&tmp)
        .args([
            "build",
            "app",
            "--skip-app-build",
            "--dist",
            "app/dist",
            "--config",
            "cfg/squark.config.json",
            "--runtime",
            "runtime",
            "--out",
            "out/asset-app",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "squark build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let info = inspect_path(&tmp.join("out/asset-app")).unwrap();
    let trailer = info.trailer.clone().unwrap();
    let pack = info.pack.clone().unwrap();
    let entry = pack
        .entries
        .iter()
        .find(|entry| entry.path == "/assets/Assistant-Bold.woff2")
        .expect("declared asset must be packed at its path")
        .clone();
    assert_eq!(entry.mime, "font/woff2");
    let bytes = fs::read(tmp.join("out/asset-app")).unwrap();
    let start = trailer.pack_offset as usize + pack.data_offset as usize + entry.data_off as usize;
    assert_eq!(
        &bytes[start..start + entry.data_len as usize],
        b"font-bytes"
    );

    let _ = fs::remove_dir_all(&tmp);
}
