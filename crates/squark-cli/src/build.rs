// SPDX-License-Identifier: MIT
//! `squark build` orchestration. It packs the app's own build output:
//! 1. Resolve `squark.config.{ts,mjs,js,json}` (or infer defaults from `package.json`).
//! 2. Run the app's own build command unless `--skip-app-build`.
//! 3. Locate `dist/` or `build/` and run `pack_directory` to compile `document.bin`,
//!    pack `.sqpak`, and append the pack + trailer to the prebuilt `squark-runtime` binary,
//!    or, for a macOS runtime, write an `.app` bundle around it (see `bundle`).

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{
    ProjectConfig, RawBuildConfig, RuntimePreference, resolve_path, resolve_runtime_binary,
};
use crate::error::CliError;
use crate::pack::{PackOptions, PackSummary, pack_directory};

/// Options for `squark build`.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub project_dir: PathBuf,
    pub config_path: Option<PathBuf>,
    pub out_binary: Option<PathBuf>,
    pub out_sqpak: Option<PathBuf>,
    pub runtime_binary: Option<PathBuf>,
    pub dist_dir: Option<PathBuf>,
    pub skip_app_build: bool,
    pub include_sourcemaps: bool,
}

/// Summary of a completed `squark build` run.
#[derive(Clone, Debug)]
pub struct BuildSummary {
    pub app_name: String,
    pub config_path: Option<PathBuf>,
    pub web_dir: PathBuf,
    /// The app as shipped: a single binary, or a `.app` bundle on macOS.
    pub out_binary: PathBuf,
    /// The file to execute; inside the bundle on macOS.
    pub executable: PathBuf,
    pub out_bytes: u64,
    pub pack: PackSummary,
}

/// Runs the full `squark build` pipeline.
pub fn build_app(options: &BuildOptions) -> Result<BuildSummary, CliError> {
    let cwd = std::env::current_dir().map_err(|source| CliError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let project = ProjectConfig::load(&cwd, &options.project_dir, options.config_path.as_deref())?;
    let project_dir = project.dir.clone();
    let raw_cfg = &project.raw;
    let app_name = project.app_name.clone();

    if !options.skip_app_build {
        run_app_build(&project_dir, &raw_cfg.build)?;
    }

    let web_dir = resolve_web_dir(
        &cwd,
        &project_dir,
        options.dist_dir.as_deref(),
        raw_cfg.build.dist_dir.as_deref(),
    )?;

    let runtime_binary = resolve_runtime_binary(
        &cwd,
        &project_dir,
        options.runtime_binary.as_deref(),
        raw_cfg.build.runtime.as_deref(),
        RuntimePreference::Dist,
    )?;

    let out_binary = match (&options.out_binary, &raw_cfg.build.out) {
        (Some(path), _) => resolve_path(&cwd, path),
        (None, Some(cfg_out)) => resolve_path(&project_dir, Path::new(cfg_out)),
        (None, None) => project_dir.join("dist").join(&app_name),
    };
    let out_sqpak = options.out_sqpak.as_deref().map(|p| resolve_path(&cwd, p));

    let entry_html = raw_cfg.build.entry_html.as_deref().unwrap_or("index.html");
    let html_path = web_dir.join(entry_html);
    let include_sourcemaps =
        options.include_sourcemaps || raw_cfg.build.include_sourcemaps.unwrap_or(false);

    let pack = pack_directory(&PackOptions {
        web_dir: web_dir.clone(),
        html_path: Some(html_path),
        out_sqpak,
        out_binary: Some(out_binary.clone()),
        runtime_binary: Some(runtime_binary),
        include_sourcemaps,
        app_name: Some(app_name.clone()),
        skip_other_html: false,
        window: Some(project.pack_window()),
        network_allow: raw_cfg.network.allow.clone(),
        extra_assets: project.extra_assets(),
    })?;
    let output = pack
        .output
        .clone()
        .expect("pack_directory writes a runtime output when given out_binary and a runtime");

    Ok(BuildSummary {
        app_name,
        config_path: project.config_path,
        web_dir,
        out_binary: output.path,
        executable: output.executable,
        out_bytes: output.bytes,
        pack,
    })
}

fn resolve_web_dir(
    cwd: &Path,
    project_dir: &Path,
    cli_dist: Option<&Path>,
    cfg_dist: Option<&str>,
) -> Result<PathBuf, CliError> {
    if let Some(dir) = cli_dist {
        let resolved = resolve_path(cwd, dir);
        if resolved.is_dir() {
            return Ok(resolved);
        }
        return Err(CliError::Usage(format!(
            "specified --dist directory does not exist: {}",
            resolved.display()
        )));
    }
    if let Some(dir) = cfg_dist {
        let resolved = resolve_path(project_dir, Path::new(dir));
        if resolved.is_dir() {
            return Ok(resolved);
        }
        return Err(CliError::Usage(format!(
            "configured build.distDir does not exist: {}",
            resolved.display()
        )));
    }
    for candidate in ["dist", "build"] {
        let p = project_dir.join(candidate);
        if p.join("index.html").is_file() {
            return Ok(p);
        }
    }
    Err(CliError::Usage(format!(
        "could not locate built web directory with index.html in {}/dist or {}/build (pass --dist)",
        project_dir.display(),
        project_dir.display()
    )))
}

/// A configured build command, run by the platform's shell: `sh -c` on Unix
/// and cmd on Windows, the shell npm also runs package scripts with there.
///
/// On Windows the line goes to `cmd /D /S /C "<line>"`, as npm does: `/S`
/// makes cmd strip exactly the one outer pair of quotes, so a line that
/// itself starts and ends with quotes (a quoted program path and a quoted
/// argument) survives, and `/D` skips AutoRun commands from the registry.
fn shell(command_line: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{command_line}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command_line]);
        c
    }
}

/// npm and yarn are `.cmd` shims on Windows, which `Command` does not find
/// under the bare name.
fn package_manager(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_owned()
    }
}

fn run_app_build(project_dir: &Path, build_cfg: &RawBuildConfig) -> Result<(), CliError> {
    let mut cmd = if let Some(custom) = &build_cfg.command {
        shell(custom)
    } else if project_dir.join("yarn.lock").is_file() {
        let mut c = Command::new(package_manager("yarn"));
        c.arg("build");
        c
    } else {
        let mut c = Command::new(package_manager("npm"));
        c.args(["run", "build"]);
        c
    };

    cmd.current_dir(project_dir);
    for (k, v) in &build_cfg.env {
        cmd.env(k, v);
    }

    let desc = build_cfg
        .command
        .clone()
        .unwrap_or_else(|| "app build".to_owned());
    let output = cmd.output().map_err(|source| CliError::CommandFailed {
        command: desc.clone(),
        detail: source.to_string(),
    })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let detail = if !stderr.trim().is_empty() {
            stderr.trim().to_owned()
        } else {
            stdout.trim().to_owned()
        };
        return Err(CliError::CommandFailed {
            command: desc,
            detail: format!("exit status {:?}: {detail}", output.status.code()),
        });
    }
    Ok(())
}
