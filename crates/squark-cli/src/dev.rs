// SPDX-License-Identifier: MIT
//! `squark dev`: start Vite with `@strange-quark/vite-plugin`, compile the HTML Vite
//! serves into a dev pack, and launch `squark-runtime` against it.
//!
//! The pack carries a `dev` block in `/__squark/manifest.json` (the origin, the
//! HMR URL and the entry module) so the runtime can load modules from the dev
//! server over HTTP and let Vite's HMR client connect to the same origin. No
//! Rust rebuild and no re-packing happens while Vite is running; a save is
//! picked up by Vite's watcher and applied through the HMR client.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use squark_types::pack::{DOCUMENT_PATH, MANIFEST_PATH};
use url::Url;

use crate::config::{ProjectConfig, RuntimePreference, resolve_runtime_binary};
use crate::error::CliError;
use crate::html::compile_html_to_document_bin;
use crate::pack::{MimeSpec, PackBuilder};

const READY_PREFIX: &str = "SQUARK_DEV_READY ";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
const PACK_ENTRY_HTML: &str = "/index.html";

/// Options for `squark dev`.
#[derive(Clone, Debug)]
pub struct DevOptions {
    pub project_dir: PathBuf,
    pub config_path: Option<PathBuf>,
    /// Vite bind port; 0 picks a free port.
    pub port: u16,
    /// Vite bind address; defaults to 127.0.0.1.
    pub host: Option<String>,
    pub runtime_binary: Option<PathBuf>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Directory the dev pack and launcher hand-off are written to.
    pub work_dir: Option<PathBuf>,
    pub test_control: Option<PathBuf>,
    pub headless: bool,
    /// Start Vite, write the dev pack, and stop without launching the runtime.
    pub prepare_only: bool,
}

/// Summary of a `squark dev` run that reached the pack (and, unless
/// `prepare_only`, the runtime).
#[derive(Clone, Debug)]
pub struct DevSummary {
    pub app_name: String,
    pub origin: String,
    pub pack: PathBuf,
    pub entry: Option<String>,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LauncherReport {
    origin: String,
    hmr: String,
    entry: Option<String>,
    #[serde(default = "default_client")]
    client: String,
    #[serde(default = "default_refresh")]
    refresh: String,
    #[serde(default)]
    app_name: Option<String>,
}

fn default_client() -> String {
    "/@vite/client".to_owned()
}

fn default_refresh() -> String {
    "/@react-refresh".to_owned()
}

#[derive(Debug, Serialize)]
struct DevNetworkManifest {
    allow: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DevManifest<'a> {
    origin: &'a str,
    hmr: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry: Option<&'a str>,
    client: &'a str,
    refresh: &'a str,
}

#[derive(Debug, Serialize)]
struct DevPackManifest<'a> {
    version: u32,
    app_name: &'a str,
    entry_html: &'a str,
    window: crate::pack::PackWindow,
    network: DevNetworkManifest,
    dev: DevManifest<'a>,
}

/// Runs `squark dev`.
pub fn run_dev(options: &DevOptions) -> Result<DevSummary, CliError> {
    if options.headless && options.test_control.is_none() {
        return Err(CliError::Usage(
            "--headless requires --test-control <socket>".to_owned(),
        ));
    }
    let cwd = env::current_dir().map_err(|source| CliError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let project = ProjectConfig::load(&cwd, &options.project_dir, options.config_path.as_deref())?;
    let (cfg_width, cfg_height) = project.window_size();
    let width = options.width.unwrap_or(cfg_width);
    let height = options.height.unwrap_or(cfg_height);
    let host = options
        .host
        .clone()
        .unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = if options.port == 0 {
        free_port(&host)?
    } else {
        options.port
    };

    let work_dir = match &options.work_dir {
        Some(dir) => dir.clone(),
        None => env::temp_dir().join(format!("squark-dev-{}", project.app_name)),
    };
    fs::create_dir_all(&work_dir).map_err(|source| CliError::Io {
        path: work_dir.clone(),
        source,
    })?;

    let launcher = find_dev_launcher(&project.dir)?;
    let mut vite = spawn_launcher(&launcher, &project, &work_dir, &host, port)?;

    let origin = match wait_for_ready(&mut vite, STARTUP_TIMEOUT) {
        Ok(origin) => origin,
        Err(error) => {
            stop(&mut vite);
            return Err(error);
        }
    };

    let report: LauncherReport = match read_launcher_report(&work_dir) {
        Ok(report) => report,
        Err(error) => {
            stop(&mut vite);
            return Err(error);
        }
    };
    let html_path = work_dir.join("index.html");
    let html = match fs::read_to_string(&html_path) {
        Ok(html) => html,
        Err(source) => {
            stop(&mut vite);
            return Err(CliError::Io {
                path: html_path,
                source,
            });
        }
    };

    let pack_path = work_dir.join("dev.sqpak");
    let written_pack = match write_dev_pack(
        &pack_path,
        &html,
        DevPackInput {
            app_name: report.app_name.as_deref().unwrap_or(&project.app_name),
            origin: &report.origin,
            hmr: &report.hmr,
            entry: report.entry.as_deref(),
            client: &report.client,
            refresh: &report.refresh,
            width,
            height,
            title: project
                .raw
                .window
                .title
                .clone()
                .unwrap_or_else(|| project.app_name.clone()),
            network_allow: project.raw.network.allow.clone(),
        },
    ) {
        Ok(pack) => pack,
        Err(error) => {
            stop(&mut vite);
            return Err(error);
        }
    };

    if options.prepare_only {
        stop(&mut vite);
        return Ok(DevSummary {
            app_name: project.app_name.clone(),
            origin,
            pack: written_pack,
            entry: report.entry,
            config_path: project.config_path,
        });
    }

    let runtime_path = match resolve_runtime_binary(
        &cwd,
        &project.dir,
        options.runtime_binary.as_deref(),
        project.raw.build.runtime.as_deref(),
        RuntimePreference::Sibling,
    ) {
        Ok(path) => path,
        Err(error) => {
            stop(&mut vite);
            return Err(error);
        }
    };

    println!(
        "squark dev: {} at {} (pack {})",
        project.app_name,
        origin,
        written_pack.display()
    );
    let mut runtime = match spawn_runtime(
        &runtime_path,
        &written_pack,
        width,
        height,
        options,
        &project.dir,
    ) {
        Ok(child) => child,
        Err(error) => {
            stop(&mut vite);
            return Err(error);
        }
    };

    let status = loop {
        match runtime.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(source) => {
                stop(&mut runtime);
                stop(&mut vite);
                return Err(CliError::Io {
                    path: runtime_path,
                    source,
                });
            }
        }
        if let Ok(Some(status)) = vite.try_wait() {
            stop(&mut runtime);
            return Err(CliError::CommandFailed {
                command: "vite dev server".to_owned(),
                detail: format!("exited unexpectedly ({status})"),
            });
        }
        thread::sleep(Duration::from_millis(100));
    };

    stop(&mut vite);
    if !status.success() {
        return Err(CliError::CommandFailed {
            command: format!("{} ({})", runtime_path.display(), written_pack.display()),
            detail: format!("exit status {:?}", status.code()),
        });
    }
    Ok(DevSummary {
        app_name: project.app_name.clone(),
        origin,
        pack: written_pack,
        entry: report.entry,
        config_path: project.config_path,
    })
}

fn stop(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn free_port(host: &str) -> Result<u16, CliError> {
    let listener = TcpListener::bind((host, 0)).map_err(|source| CliError::Io {
        path: PathBuf::from(format!("{host}:0")),
        source,
    })?;
    let port = listener
        .local_addr()
        .map_err(|source| CliError::Io {
            path: PathBuf::from(format!("{host}:0")),
            source,
        })?
        .port();
    Ok(port)
}

/// Finds `@strange-quark/vite-plugin`'s long-running launcher.
///
/// `SQUARK_VITE_PLUGIN` may point at the package directory or at `dev-main.js`
/// directly. Otherwise the package is resolved through `node_modules` from the
/// app, the working directory, and the `squark` binary's directory, in that
/// order: the launcher imports Vite from where it is installed, so the app's
/// own copy keeps the app on its own Vite.
fn find_dev_launcher(app_dir: &Path) -> Result<PathBuf, CliError> {
    if let Ok(value) = env::var("SQUARK_VITE_PLUGIN")
        && !value.trim().is_empty()
    {
        let path = PathBuf::from(value);
        let candidate = if path.is_dir() {
            path.join("dev-main.js")
        } else {
            path
        };
        if candidate.is_file() {
            return Ok(candidate);
        }
        return Err(CliError::Usage(format!(
            "SQUARK_VITE_PLUGIN does not contain dev-main.js: {}",
            candidate.display()
        )));
    }

    let mut starts = vec![app_dir.to_path_buf()];
    if let Ok(cwd) = env::current_dir() {
        starts.push(cwd);
    }
    if let Ok(exe) = env::current_exe()
        && let Some(dir) = exe.parent()
    {
        starts.push(dir.to_path_buf());
    }
    first_dev_launcher(&starts).ok_or_else(|| {
        CliError::Usage(
            "could not locate @strange-quark/vite-plugin: install it in the app, \
             build the workspace (`npm run build --workspaces`), or set SQUARK_VITE_PLUGIN"
                .to_owned(),
        )
    })
}

/// The first `dev-main.js` that Node-style `node_modules` lookup finds from
/// each start directory in turn.
fn first_dev_launcher(starts: &[PathBuf]) -> Option<PathBuf> {
    let relative = Path::new("node_modules/@strange-quark/vite-plugin/dist/dev-main.js");
    starts
        .iter()
        .flat_map(|start| start.ancestors())
        .map(|ancestor| ancestor.join(relative))
        .find(|candidate| candidate.is_file())
}

fn spawn_launcher(
    launcher: &Path,
    project: &ProjectConfig,
    work_dir: &Path,
    host: &str,
    port: u16,
) -> Result<Child, CliError> {
    let mut cmd = Command::new("node");
    cmd.arg(launcher)
        .arg(&project.dir)
        .arg(work_dir)
        .env("SQUARK_DEV_HOST", host)
        .env("SQUARK_DEV_PORT", port.to_string())
        .env("SQUARK_DEV_NAME", &project.app_name)
        // A kept-open pipe, not /dev/null: Vite treats stdin EOF as "the parent
        // went away" and shuts the dev server down.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(config) = &project.config_path {
        cmd.env("SQUARK_DEV_CONFIG", config);
    }
    cmd.spawn().map_err(|source| CliError::CommandFailed {
        command: format!("node {}", launcher.display()),
        detail: source.to_string(),
    })
}

/// Reads the launcher's stdout until `SQUARK_DEV_READY <origin>`.
///
/// A dedicated thread keeps the pipe drained for the whole dev session and
/// forwards Vite's log lines to stderr. It must never stop reading while Vite
/// runs: Vite logs to stdout, and a closed pipe turns its next `console.log`
/// into an unhandled EPIPE that kills the dev server.
fn wait_for_ready(child: &mut Child, timeout: Duration) -> Result<String, CliError> {
    let stdout = child.stdout.take().ok_or_else(|| CliError::CommandFailed {
        command: "vite dev server".to_owned(),
        detail: "stdout was not captured".to_owned(),
    })?;
    let (sender, receiver) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if let Some(origin) = line.strip_prefix(READY_PREFIX) {
                let _ = sender.send(origin.trim().trim_end_matches('/').to_owned());
                continue;
            }
            eprintln!("[vite] {line}");
        }
    });

    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(CliError::CommandFailed {
                command: "vite dev server".to_owned(),
                detail: format!("did not report readiness within {}s", timeout.as_secs()),
            });
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(500))) {
            Ok(origin) => return Ok(origin),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(CliError::CommandFailed {
                        command: "vite dev server".to_owned(),
                        detail: format!("exited before readiness ({status})"),
                    });
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(CliError::CommandFailed {
                    command: "vite dev server".to_owned(),
                    detail: "exited before reporting readiness".to_owned(),
                });
            }
        }
    }
}

fn read_launcher_report(work_dir: &Path) -> Result<LauncherReport, CliError> {
    let path = work_dir.join("squark-dev.json");
    let bytes = fs::read(&path).map_err(|source| CliError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

struct DevPackInput<'a> {
    app_name: &'a str,
    origin: &'a str,
    hmr: &'a str,
    entry: Option<&'a str>,
    client: &'a str,
    refresh: &'a str,
    width: u32,
    height: u32,
    title: String,
    network_allow: Vec<String>,
}

/// Writes the dev pack and returns its path.
fn write_dev_pack(
    pack_path: &Path,
    html: &str,
    input: DevPackInput<'_>,
) -> Result<PathBuf, CliError> {
    let compiled = compile_html_to_document_bin(html, PACK_ENTRY_HTML)?;

    // The dev origin is allowlisted explicitly as well as through the runtime's
    // dev flag, so an app's `squark.config` stays the one place that names origins.
    let mut allow = input.network_allow;
    for candidate in [input.origin.to_owned(), ws_origin(input.origin)] {
        if !allow.iter().any(|entry| entry == &candidate) {
            allow.push(candidate);
        }
    }

    let manifest = DevPackManifest {
        version: 1,
        app_name: input.app_name,
        entry_html: PACK_ENTRY_HTML,
        window: crate::pack::PackWindow {
            width: input.width,
            height: input.height,
            title: input.title,
        },
        network: DevNetworkManifest { allow },
        dev: DevManifest {
            origin: input.origin,
            hmr: input.hmr,
            entry: input.entry,
            client: input.client,
            refresh: input.refresh,
        },
    };
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;

    let mut builder = PackBuilder::new();
    builder.add_entry(DOCUMENT_PATH, MimeSpec::Builtin(0), compiled.bytes)?;
    builder.add_entry(MANIFEST_PATH, MimeSpec::Builtin(4), manifest_bytes)?;
    builder.add_entry(
        PACK_ENTRY_HTML,
        MimeSpec::Builtin(1),
        html.as_bytes().to_vec(),
    )?;
    let sqpak = builder.build()?;
    fs::write(pack_path, sqpak).map_err(|source| CliError::Io {
        path: pack_path.to_path_buf(),
        source,
    })?;
    Ok(pack_path.to_path_buf())
}

fn ws_origin(origin: &str) -> String {
    Url::parse(origin)
        .ok()
        .and_then(|mut url| {
            let scheme = match url.scheme() {
                "https" => "wss",
                _ => "ws",
            };
            url.set_scheme(scheme).ok()?;
            url.set_path("");
            Some(url.to_string().trim_end_matches('/').to_owned())
        })
        .unwrap_or_else(|| origin.to_owned())
}

fn spawn_runtime(
    runtime_path: &Path,
    pack: &Path,
    width: u32,
    height: u32,
    options: &DevOptions,
    project_dir: &Path,
) -> Result<Child, CliError> {
    let mut cmd = Command::new(runtime_path);
    cmd.arg(pack)
        .arg("--width")
        .arg(width.to_string())
        .arg("--height")
        .arg(height.to_string())
        .current_dir(project_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(socket) = &options.test_control {
        cmd.arg("--test-control").arg(socket);
    }
    if options.headless {
        cmd.arg("--headless");
    }
    cmd.spawn().map_err(|source| CliError::CommandFailed {
        command: runtime_path.display().to_string(),
        detail: source.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(origin: &'a str, allow: Vec<String>) -> DevPackInput<'a> {
        DevPackInput {
            app_name: "counter",
            origin,
            hmr: "ws://127.0.0.1:5173/",
            entry: Some("/src/main.tsx"),
            client: "/@vite/client",
            refresh: "/@react-refresh",
            width: 800,
            height: 600,
            title: "Counter".to_owned(),
            network_allow: allow,
        }
    }

    #[test]
    fn dev_launcher_prefers_the_apps_own_plugin() {
        let root = tempfile::tempdir().unwrap();
        let install = |dir: &Path| {
            let dist = dir.join("node_modules/@strange-quark/vite-plugin/dist");
            fs::create_dir_all(&dist).unwrap();
            fs::write(dist.join("dev-main.js"), "").unwrap();
            dist.join("dev-main.js")
        };
        // A monorepo root with its own plugin, and an app under it that
        // installs another one alongside a different Vite.
        let workspace = root.path().join("workspace");
        let app = workspace.join("apps/web");
        fs::create_dir_all(&app).unwrap();
        let workspace_launcher = install(&workspace);
        let app_launcher = install(&app);

        let starts = [app.clone(), workspace.clone()];
        assert_eq!(first_dev_launcher(&starts), Some(app_launcher.clone()));
        // An app without its own copy falls back to an ancestor's.
        fs::remove_dir_all(app.join("node_modules")).unwrap();
        assert_eq!(first_dev_launcher(&starts), Some(workspace_launcher));
        assert_eq!(first_dev_launcher(&[root.path().to_path_buf()]), None);
    }

    #[test]
    fn dev_pack_writes_dev_manifest_network_and_document() {
        let dir = env::temp_dir().join(format!("squark-dev-unit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("dev.sqpak");
        let html = "<!DOCTYPE html><html><head><title>x</title></head><body>\
                    <script type=\"module\" src=\"/@vite/client\"></script>\
                    <script type=\"module\" src=\"/src/main.tsx\"></script></body></html>";

        write_dev_pack(
            &pack,
            html,
            input(
                "http://127.0.0.1:5173",
                vec!["https://api.example.test".to_owned()],
            ),
        )
        .unwrap();

        let info = crate::inspect::inspect_path(&pack).unwrap();
        let entries: Vec<&str> = info
            .pack
            .as_ref()
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert!(entries.contains(&MANIFEST_PATH));
        assert!(entries.contains(&DOCUMENT_PATH));
        assert!(entries.contains(&PACK_ENTRY_HTML));

        let manifest = read_manifest(&pack).unwrap();
        assert_eq!(manifest["app_name"], "counter");
        assert_eq!(manifest["entry_html"], PACK_ENTRY_HTML);
        assert_eq!(manifest["dev"]["origin"], "http://127.0.0.1:5173");
        assert_eq!(manifest["dev"]["hmr"], "ws://127.0.0.1:5173/");
        assert_eq!(manifest["dev"]["entry"], "/src/main.tsx");
        assert_eq!(manifest["dev"]["client"], "/@vite/client");
        assert_eq!(manifest["dev"]["refresh"], "/@react-refresh");
        assert_eq!(manifest["window"]["width"], 800);
        assert_eq!(
            manifest["network"]["allow"],
            serde_json::json!([
                "https://api.example.test",
                "http://127.0.0.1:5173",
                "ws://127.0.0.1:5173"
            ])
        );

        // The compiler kept both module scripts, in browser order.
        let document = info.document.unwrap();
        assert_eq!(document.scripts.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dev_pack_does_not_duplicate_an_already_allowed_origin() {
        let dir = env::temp_dir().join(format!("squark-dev-dup-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pack = dir.join("dev.sqpak");
        write_dev_pack(
            &pack,
            "<html><body></body></html>",
            input(
                "http://127.0.0.1:5173",
                vec!["http://127.0.0.1:5173".to_owned()],
            ),
        )
        .unwrap();
        let manifest = read_manifest(&pack).unwrap();
        assert_eq!(
            manifest["network"]["allow"],
            serde_json::json!(["http://127.0.0.1:5173", "ws://127.0.0.1:5173"])
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ws_origin_maps_https_and_strips_paths() {
        assert_eq!(ws_origin("http://127.0.0.1:5173"), "ws://127.0.0.1:5173");
        assert_eq!(
            ws_origin("https://dev.example.test/app/"),
            "wss://dev.example.test"
        );
    }

    fn read_manifest(pack_path: &Path) -> Result<serde_json::Value, CliError> {
        let bytes = fs::read(pack_path).unwrap();
        let info = crate::inspect::inspect_path(pack_path)?;
        let pack = info.pack.unwrap();
        let entry = pack
            .entries
            .iter()
            .find(|entry| entry.path == MANIFEST_PATH)
            .unwrap();
        let start = pack.data_offset as usize + entry.data_off as usize;
        let end = start + entry.data_len as usize;
        Ok(serde_json::from_slice(&bytes[start..end])?)
    }
}
