// SPDX-License-Identifier: MIT
//! Project config loading shared by `squark build` and `squark dev`
//! (see `packages/strange-quark`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::CliError;

pub const CONFIG_FILENAMES: [&str; 4] = [
    "squark.config.ts",
    "squark.config.mjs",
    "squark.config.js",
    "squark.config.json",
];

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAssetConfig {
    /// Absolute pack path the app requests, e.g. `/assets/Assistant-Bold.woff2`.
    pub path: String,
    /// File to read the bytes from.
    pub file: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawSquarkConfig {
    pub name: Option<String>,
    #[serde(default)]
    pub assets: Vec<RawAssetConfig>,
    #[serde(default)]
    pub network: RawNetworkConfig,
    #[serde(default)]
    pub window: RawWindowConfig,
    #[serde(default)]
    pub build: RawBuildConfig,
    #[serde(default)]
    pub native: RawNativeConfig,
    #[serde(default)]
    pub compat: RawCompatConfig,
}

/// `native`: the app's native modules (architecture §13).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawNativeConfig {
    /// The app's Rust library crate, whose `modules()` returns its
    /// `squark_native::Module`s. `squark build` links it into an app runtime.
    #[serde(rename = "crate")]
    pub crate_dir: Option<String>,
    /// Modules the app may call. Others are refused at run time.
    #[serde(default)]
    pub modules: Vec<String>,
    /// A Strange Quark source checkout to build the app runtime from
    /// (default: `SQUARK_SOURCE_DIR`).
    pub squark_source: Option<String>,
    /// Cargo features of the native crate to enable.
    #[serde(default)]
    pub features: Vec<String>,
    /// Whether the native crate's default features stay on (default: true).
    pub default_features: Option<bool>,
}

/// `compat`: frontends written for another shell.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawCompatConfig {
    pub tauri: Option<RawTauriCompat>,
}

/// `compat.tauri`: run a Tauri 2 frontend through `@strange-quark/tauri`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawTauriCompat {
    /// The native module Tauri commands go to.
    pub module: Option<String>,
    /// A classic script defining `__STRANGE_QUARK_TAURI_FIXTURE__`, canned
    /// replies for running the frontend without native modules.
    pub fixture: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawNetworkConfig {
    #[serde(default)]
    pub allow: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawWindowConfig {
    pub title: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawBuildConfig {
    pub command: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub dist_dir: Option<String>,
    pub entry_html: Option<String>,
    pub include_sourcemaps: Option<bool>,
    pub runtime: Option<String>,
    pub out: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackageJson {
    name: Option<String>,
}

/// A loaded project: its directory, config file (if any), and resolved app name.
#[derive(Debug)]
pub struct ProjectConfig {
    pub dir: PathBuf,
    pub config_path: Option<PathBuf>,
    pub raw: RawSquarkConfig,
    pub app_name: String,
}

impl ProjectConfig {
    /// Loads `squark.config.{ts,mjs,js,json}` for `project_dir`.
    ///
    /// An explicit `--config` path resolves against `cwd`, like every other
    /// `squark` path flag; config-file values stay relative to `project_dir`.
    pub fn load(cwd: &Path, project_dir: &Path, explicit: Option<&Path>) -> Result<Self, CliError> {
        let project_dir = fs::canonicalize(project_dir).map_err(|source| CliError::Io {
            path: project_dir.to_path_buf(),
            source,
        })?;
        let config_path = find_config_file(cwd, &project_dir, explicit)?;
        let raw = match &config_path {
            Some(path) => load_config_file(path)?,
            None => RawSquarkConfig::default(),
        };
        let app_name = raw
            .name
            .as_deref()
            .map(sanitize_binary_name)
            .unwrap_or_else(|| infer_project_name(&project_dir));
        Ok(Self {
            dir: project_dir,
            config_path,
            raw,
            app_name,
        })
    }

    /// Window size: the config's, else the same 1280x800 default as
    /// `packages/strange-quark`.
    pub fn window_size(&self) -> (u32, u32) {
        (
            self.raw.window.width.unwrap_or(1280),
            self.raw.window.height.unwrap_or(800),
        )
    }

    /// `build.entryHtml`, defaulting to `index.html`.
    pub fn entry_html(&self) -> &str {
        self.raw.build.entry_html.as_deref().unwrap_or("index.html")
    }

    /// The window title: `window.title`, else the app name.
    pub fn window_title(&self) -> String {
        self.raw
            .window
            .title
            .clone()
            .unwrap_or_else(|| self.app_name.clone())
    }

    /// Files the config declares for exact pack paths. `assets[].file` resolves
    /// against the config file's directory, so a repo-owned config can point
    /// into a vendored app without writing files inside it.
    pub fn extra_assets(&self) -> Vec<crate::pack::ExtraAsset> {
        let base = self
            .config_path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or(&self.dir);
        self.raw
            .assets
            .iter()
            .map(|asset| crate::pack::ExtraAsset {
                path: asset.path.clone(),
                source: resolve_path(base, Path::new(&asset.file)),
            })
            .collect()
    }

    /// The window block written to `/__squark/manifest.json`.
    pub fn pack_window(&self) -> crate::pack::PackWindow {
        let (width, height) = self.window_size();
        crate::pack::PackWindow {
            title: self.window_title(),
            width,
            height,
        }
    }
}

pub fn find_config_file(
    cwd: &Path,
    project_dir: &Path,
    explicit: Option<&Path>,
) -> Result<Option<PathBuf>, CliError> {
    if let Some(path) = explicit {
        let resolved = resolve_path(cwd, path);
        if !resolved.is_file() {
            return Err(CliError::Usage(format!(
                "config file not found: {}",
                resolved.display()
            )));
        }
        return Ok(Some(resolved));
    }
    for name in CONFIG_FILENAMES {
        let candidate = project_dir.join(name);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

pub fn load_config_file(config_path: &Path) -> Result<RawSquarkConfig, CliError> {
    if config_path.extension().and_then(|e| e.to_str()) == Some("json") {
        let text = fs::read_to_string(config_path).map_err(|source| CliError::Io {
            path: config_path.to_path_buf(),
            source,
        })?;
        return Ok(serde_json::from_str(&text)?);
    }

    // Node 26 supports ESM and native TypeScript type stripping out of the box.
    let loader_script = r#"
import { pathToFileURL } from "node:url";
const mod = await import(pathToFileURL(process.argv[1]).href);
const exported = mod.default ?? mod;
const value = typeof exported === "function" ? await exported() : exported;
process.stdout.write(JSON.stringify(value ?? {}));
"#;
    let output = Command::new("node")
        .args(["--input-type=module", "-e", loader_script])
        .arg(config_path)
        .output()
        .map_err(|source| CliError::CommandFailed {
            command: format!("node (loading {})", config_path.display()),
            detail: source.to_string(),
        })?;
    if !output.status.success() {
        return Err(CliError::CommandFailed {
            command: format!("node (loading {})", config_path.display()),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

/// Which prebuilt runtime to prefer when the user did not pass one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimePreference {
    /// `squark build`: the stripped `target/dist/squark-runtime` that ships.
    Dist,
    /// `squark dev`: the runtime built beside the CLI, which matches its sources.
    Sibling,
}

/// Locates the prebuilt `squark-runtime` binary: `--runtime`, then
/// `build.runtime`, then `SQUARK_RUNTIME_BIN`, then the profile-dependent default.
pub fn resolve_runtime_binary(
    cwd: &Path,
    project_dir: &Path,
    cli_runtime: Option<&Path>,
    cfg_runtime: Option<&str>,
    preference: RuntimePreference,
) -> Result<PathBuf, CliError> {
    if let Some(path) = cli_runtime {
        let resolved = resolve_path(cwd, path);
        if resolved.is_file() {
            return Ok(resolved);
        }
        return Err(CliError::Usage(format!(
            "runtime binary not found at --runtime {}",
            resolved.display()
        )));
    }
    if let Some(path) = cfg_runtime {
        let resolved = resolve_path(project_dir, Path::new(path));
        if resolved.is_file() {
            return Ok(resolved);
        }
        return Err(CliError::Usage(format!(
            "runtime binary not found at config build.runtime {}",
            resolved.display()
        )));
    }
    if let Ok(env_path) = std::env::var("SQUARK_RUNTIME_BIN") {
        let p = PathBuf::from(&env_path);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(choice) = choose_runtime(&exe, preference)
    {
        if choice.debug_fallback {
            eprintln!(
                "warning: using {} (a debug build); shipped binaries keep debug info and are much \
                 larger. Build a stripped runtime with `cargo build --profile dist -p squark-host \
                 --bin squark-runtime` so it lands in target/dist/squark-runtime, or pass --runtime.",
                choice.path.display()
            );
        }
        return Ok(choice.path);
    }
    Err(CliError::Usage(
        "could not locate prebuilt squark-runtime binary (pass --runtime <path> or set SQUARK_RUNTIME_BIN; \
         ship the stripped target/dist/squark-runtime from `cargo build --profile dist`)"
            .to_owned(),
    ))
}

struct RuntimeChoice {
    path: PathBuf,
    /// The chosen file is a same-profile build, not the stripped dist one.
    debug_fallback: bool,
}

/// Picks the default runtime for a `squark` executable.
///
/// `squark build` prefers the stripped `[profile.dist]` runtime, because it is
/// what ships. `squark dev` prefers the runtime built beside the CLI: it matches
/// these sources, while target/dist can be stale.
///
/// When `squark` runs from an installed platform package (for example
/// `node_modules/@strange-quark/cli-darwin-arm64/bin/squark` or `...\bin\squark.exe`),
/// the shipped `squark-runtime` sits right beside it and is used directly without
/// a debug-build warning. If `exe` is a symlink (such as a shim or `/usr/local/bin/squark`),
/// its canonical target directory is also checked.
fn choose_runtime(exe: &Path, preference: RuntimePreference) -> Option<RuntimeChoice> {
    if let Some(choice) = choose_runtime_at(exe, preference) {
        return Some(choice);
    }
    let canonical = fs::canonicalize(exe).ok()?;
    if canonical == exe {
        return None;
    }
    choose_runtime_at(&canonical, preference)
}

fn choose_runtime_at(exe: &Path, preference: RuntimePreference) -> Option<RuntimeChoice> {
    let sibling = exe.parent().and_then(|dir| find_runtime_in_dir(dir, exe));
    let stripped = dist_runtime_for(exe);
    let sibling_is_debug = is_debug_profile_dir(exe);
    match preference {
        RuntimePreference::Sibling => sibling
            .map(|path| RuntimeChoice {
                path,
                debug_fallback: false,
            })
            .or(stripped.map(|path| RuntimeChoice {
                path,
                debug_fallback: false,
            })),
        RuntimePreference::Dist => {
            if is_cargo_non_dist_profile_dir(exe) {
                stripped
                    .map(|path| RuntimeChoice {
                        path,
                        debug_fallback: false,
                    })
                    .or(sibling.map(|path| RuntimeChoice {
                        path,
                        debug_fallback: sibling_is_debug,
                    }))
            } else {
                sibling
                    .map(|path| RuntimeChoice {
                        path,
                        debug_fallback: false,
                    })
                    .or(stripped.map(|path| RuntimeChoice {
                        path,
                        debug_fallback: false,
                    }))
            }
        }
    }
}

/// Candidate `squark-runtime` filenames matching `exe`'s platform convention.
///
/// Checking `exe`'s `.exe` extension lets cross-platform tests exercise Windows
/// paths on Unix, while `EXE_SUFFIX` covers bare paths on Windows.
fn runtime_candidates(exe: &Path) -> &'static [&'static str] {
    let has_exe_ext = exe
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    if has_exe_ext {
        &["squark-runtime.exe"]
    } else if std::env::consts::EXE_SUFFIX == ".exe" {
        &["squark-runtime.exe", "squark-runtime"]
    } else {
        &["squark-runtime"]
    }
}

fn find_runtime_in_dir(dir: &Path, exe: &Path) -> Option<PathBuf> {
    runtime_candidates(exe)
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

fn is_debug_profile_dir(exe: &Path) -> bool {
    exe.parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str())
        == Some("debug")
}

fn is_cargo_non_dist_profile_dir(exe: &Path) -> bool {
    matches!(
        exe.parent()
            .and_then(|dir| dir.file_name())
            .and_then(|name| name.to_str()),
        Some("debug" | "release")
    )
}

/// `target/dist/squark-runtime` next to a `target/<profile>/squark` executable.
fn dist_runtime_for(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let target_dir = dir.parent()?;
    let dist_dir = target_dir.join("dist");
    find_runtime_in_dir(&dist_dir, exe)
}

pub fn resolve_path(base: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

pub fn infer_project_name(project_dir: &Path) -> String {
    let pkg_path = project_dir.join("package.json");
    if let Ok(text) = fs::read_to_string(&pkg_path)
        && let Ok(pkg) = serde_json::from_str::<PackageJson>(&text)
        && let Some(name) = pkg.name
        && !name.trim().is_empty()
    {
        return sanitize_binary_name(name.trim());
    }
    let dir_name = project_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("squark-app");
    sanitize_binary_name(dir_name)
}

pub fn sanitize_binary_name(raw: &str) -> String {
    let unscoped = raw.rsplit('/').next().unwrap_or(raw);
    let cleaned: String = unscoped
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "squark-app".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_binary_name_keeps_scope_and_strips_bad_characters() {
        assert_eq!(sanitize_binary_name("@acme/My App"), "My-App");
        assert_eq!(sanitize_binary_name("--"), "squark-app");
    }

    #[test]
    fn runtime_preference_picks_dist_for_build_and_sibling_for_dev() {
        let tmp =
            std::env::temp_dir().join(format!("squark-runtime-choice-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("target/debug")).unwrap();
        fs::create_dir_all(tmp.join("target/dist")).unwrap();
        let exe = tmp.join("target/debug/squark");

        assert!(choose_runtime(&exe, RuntimePreference::Dist).is_none());

        fs::write(tmp.join("target/debug/squark-runtime"), b"debug").unwrap();
        let only_debug = choose_runtime(&exe, RuntimePreference::Dist).unwrap();
        assert_eq!(only_debug.path, tmp.join("target/debug/squark-runtime"));
        assert!(only_debug.debug_fallback);

        fs::write(tmp.join("target/dist/squark-runtime"), b"stripped").unwrap();
        let built = choose_runtime(&exe, RuntimePreference::Dist).unwrap();
        assert_eq!(built.path, tmp.join("target/dist/squark-runtime"));
        assert!(!built.debug_fallback);
        let dev = choose_runtime(&exe, RuntimePreference::Sibling).unwrap();
        assert_eq!(dev.path, tmp.join("target/debug/squark-runtime"));
        assert!(!dev.debug_fallback);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dist_profile_runtime_wins_over_the_debug_sibling() {
        let tmp = std::env::temp_dir().join(format!("squark-dist-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("target/debug")).unwrap();
        fs::create_dir_all(tmp.join("target/dist")).unwrap();
        fs::write(tmp.join("target/debug/squark-runtime"), b"debug").unwrap();
        assert_eq!(dist_runtime_for(&tmp.join("target/debug/squark")), None);

        fs::write(tmp.join("target/dist/squark-runtime"), b"stripped").unwrap();
        assert_eq!(
            dist_runtime_for(&tmp.join("target/debug/squark")),
            Some(tmp.join("target/dist/squark-runtime"))
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn platform_package_sibling_runtime_is_used_without_debug_warning() {
        let tmp =
            std::env::temp_dir().join(format!("squark-pkg-runtime-unix-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let bin_dir = tmp.join("node_modules/@strange-quark/cli-darwin-arm64/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join("squark");
        let runtime = bin_dir.join("squark-runtime");
        fs::write(&exe, b"squark").unwrap();
        fs::write(&runtime, b"runtime").unwrap();

        let for_build = choose_runtime(&exe, RuntimePreference::Dist).unwrap();
        assert_eq!(for_build.path, runtime);
        assert!(!for_build.debug_fallback);

        let for_dev = choose_runtime(&exe, RuntimePreference::Sibling).unwrap();
        assert_eq!(for_dev.path, runtime);
        assert!(!for_dev.debug_fallback);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn windows_exe_locates_squark_runtime_exe_beside_it_and_in_dist() {
        let tmp =
            std::env::temp_dir().join(format!("squark-pkg-runtime-win32-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);

        let bin_dir = tmp.join("node_modules/@strange-quark/cli-win32-x64/bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let pkg_exe = bin_dir.join("squark.exe");
        let pkg_runtime = bin_dir.join("squark-runtime.exe");
        fs::write(&pkg_exe, b"squark").unwrap();
        // A non-.exe file should not match squark.exe.
        fs::write(bin_dir.join("squark-runtime"), b"wrong").unwrap();
        assert!(choose_runtime(&pkg_exe, RuntimePreference::Dist).is_none());

        fs::write(&pkg_runtime, b"runtime").unwrap();
        let built = choose_runtime(&pkg_exe, RuntimePreference::Dist).unwrap();
        assert_eq!(built.path, pkg_runtime);
        assert!(!built.debug_fallback);
        let dev = choose_runtime(&pkg_exe, RuntimePreference::Sibling).unwrap();
        assert_eq!(dev.path, pkg_runtime);
        assert!(!dev.debug_fallback);

        // Cargo target/debug/squark.exe -> target/dist/squark-runtime.exe
        let debug_dir = tmp.join("target/debug");
        let dist_dir = tmp.join("target/dist");
        fs::create_dir_all(&debug_dir).unwrap();
        fs::create_dir_all(&dist_dir).unwrap();
        let debug_exe = debug_dir.join("squark.exe");
        let debug_rt = debug_dir.join("squark-runtime.exe");
        let dist_rt = dist_dir.join("squark-runtime.exe");
        fs::write(&debug_rt, b"debug").unwrap();
        let fallback = choose_runtime(&debug_exe, RuntimePreference::Dist).unwrap();
        assert_eq!(fallback.path, debug_rt);
        assert!(fallback.debug_fallback);

        fs::write(&dist_rt, b"dist").unwrap();
        let dist = choose_runtime(&debug_exe, RuntimePreference::Dist).unwrap();
        assert_eq!(dist.path, dist_rt);
        assert!(!dist.debug_fallback);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_squark_locates_runtime_beside_canonical_target() {
        let tmp =
            std::env::temp_dir().join(format!("squark-pkg-runtime-symlink-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let bin_dir = tmp.join("pkg/bin");
        let link_dir = tmp.join("links");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&link_dir).unwrap();
        let target_exe = bin_dir.join("squark");
        let target_rt = bin_dir.join("squark-runtime");
        fs::write(&target_exe, b"squark").unwrap();
        fs::write(&target_rt, b"runtime").unwrap();
        let link = link_dir.join("squark");
        std::os::unix::fs::symlink(&target_exe, &link).unwrap();

        let choice = choose_runtime(&link, RuntimePreference::Dist).unwrap();
        assert_eq!(choice.path, fs::canonicalize(&target_rt).unwrap());
        assert!(!choice.debug_fallback);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn raw_config_reads_network_window_and_build() {
        let raw: RawSquarkConfig = serde_json::from_str(
            r#"{
              "name": "demo",
              "network": {"allow": ["https://api.example.test"]},
              "window": {"width": 1024, "height": 768},
              "build": {"distDir": "build", "entryHtml": "app.html"}
            }"#,
        )
        .unwrap();
        assert_eq!(raw.network.allow, ["https://api.example.test"]);
        assert_eq!(raw.window.width, Some(1024));
        assert_eq!(raw.build.entry_html.as_deref(), Some("app.html"));
    }
}
