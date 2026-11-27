// SPDX-License-Identifier: MIT
//! `squark screenshot` implementation, rendered headlessly by squark-host.
//!
//! Invokes `squark-runtime <pack> --screenshot --out <png> --width W --height H --dpr D --gpu-adapter A`.
//! Accepts a `.sqpak`, an appended runtime binary, a web directory, or a standalone
//! `.html` file (which is packed into a temporary `.sqpak` with its surrounding assets).

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use squark_types::pack::{SQPK_MAGIC, TRAILER_MAGIC, TRAILER_SIZE};

use crate::error::CliError;
use crate::pack::{PackOptions, pack_directory};

/// Options for `squark screenshot`.
#[derive(Clone, Debug)]
pub struct ScreenshotOptions {
    pub input: PathBuf,
    pub out: PathBuf,
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
    pub gpu_adapter: String,
    pub runtime_binary: Option<PathBuf>,
    pub timeout_secs: u64,
}

struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Renders `options.input` headlessly via `squark-runtime` and writes a PNG to `options.out`.
pub fn run_screenshot(options: &ScreenshotOptions) -> Result<(), CliError> {
    if !matches!(
        options.gpu_adapter.as_str(),
        "auto" | "software" | "hardware"
    ) {
        return Err(CliError::Usage(format!(
            "invalid --gpu-adapter {:?}: expected auto, software, or hardware",
            options.gpu_adapter
        )));
    }

    let input = fs::canonicalize(&options.input).map_err(|source| CliError::Io {
        path: options.input.clone(),
        source,
    })?;
    let runtime = resolve_runtime_binary(options.runtime_binary.as_deref())?;

    let (pack_path, _temp_guard) = prepare_pack_input(&input)?;

    if let Some(parent) = options.out.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|source| CliError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut child = Command::new(&runtime)
        .arg(&pack_path)
        .args(["--screenshot", "--out"])
        .arg(&options.out)
        .args([
            "--width",
            &options.width.to_string(),
            "--height",
            &options.height.to_string(),
            "--dpr",
            &options.dpr.to_string(),
            "--gpu-adapter",
            &options.gpu_adapter,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| CliError::CommandFailed {
            command: runtime.display().to_string(),
            detail: source.to_string(),
        })?;

    let deadline = Instant::now() + Duration::from_secs(options.timeout_secs.max(1));
    loop {
        if let Some(status) = child.try_wait().map_err(|source| CliError::CommandFailed {
            command: runtime.display().to_string(),
            detail: source.to_string(),
        })? {
            let output = child
                .wait_with_output()
                .map_err(|source| CliError::CommandFailed {
                    command: runtime.display().to_string(),
                    detail: source.to_string(),
                })?;
            if !status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(CliError::CommandFailed {
                    command: runtime.display().to_string(),
                    detail: format!("exit status {:?}: {}", status.code(), stderr.trim()),
                });
            }
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CliError::CommandFailed {
                command: runtime.display().to_string(),
                detail: format!("timed out after {}s", options.timeout_secs),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn prepare_pack_input(input: &Path) -> Result<(PathBuf, Option<TempFileGuard>), CliError> {
    // A macOS app bundle, or the executable inside one, carries its pack in
    // Contents/Resources rather than appended.
    if let Some(pack) = crate::bundle::pack_in_bundle(input) {
        return Ok((pack, None));
    }
    if input.is_file() && is_packed_file(input)? {
        return Ok((input.to_path_buf(), None));
    }

    let unique = format!(
        "squark-shot-{}-{}.sqpak",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let temp_sqpak = std::env::temp_dir().join(unique);

    if input.is_dir() {
        pack_directory(&PackOptions {
            web_dir: input.to_path_buf(),
            html_path: None,
            out_sqpak: Some(temp_sqpak.clone()),
            out_binary: None,
            runtime_binary: None,
            include_sourcemaps: false,
            app_name: None,
            skip_other_html: false,
            window: None,
            network_allow: Vec::new(),
            extra_assets: Vec::new(),
            native_modules: Vec::new(),
            prelude: None,
        })?;
        return Ok((temp_sqpak.clone(), Some(TempFileGuard(temp_sqpak))));
    }

    // Standalone HTML case (e.g. `tests/reftests/cases/r1/01-inline/01-whitespace-across-spans.html`).
    // If parent's parent contains `_assets/`, root the pack there so `../_assets/...` resolves cleanly.
    let parent = input.parent().ok_or_else(|| {
        CliError::Usage(format!(
            "input HTML has no parent directory: {}",
            input.display()
        ))
    })?;
    let web_dir = match parent.parent() {
        Some(grandparent) if grandparent.join("_assets").is_dir() => grandparent.to_path_buf(),
        _ => parent.to_path_buf(),
    };

    pack_directory(&PackOptions {
        web_dir,
        html_path: Some(input.to_path_buf()),
        out_sqpak: Some(temp_sqpak.clone()),
        out_binary: None,
        runtime_binary: None,
        include_sourcemaps: false,
        app_name: None,
        skip_other_html: true,
        window: None,
        network_allow: Vec::new(),
        extra_assets: Vec::new(),
        native_modules: Vec::new(),
        prelude: None,
    })?;

    Ok((temp_sqpak.clone(), Some(TempFileGuard(temp_sqpak))))
}

fn is_packed_file(path: &Path) -> Result<bool, CliError> {
    let mut file = File::open(path).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let len = file
        .metadata()
        .map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .len();
    if len >= SQPK_MAGIC.len() as u64 {
        let mut head = [0u8; 8];
        file.read_exact(&mut head).map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if head == SQPK_MAGIC {
            return Ok(true);
        }
    }
    if len >= TRAILER_SIZE as u64 {
        file.seek(SeekFrom::End(-(TRAILER_SIZE as i64)))
            .map_err(|source| CliError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut tail_magic = [0u8; 8];
        file.read_exact(&mut tail_magic)
            .map_err(|source| CliError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        if tail_magic == TRAILER_MAGIC {
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolve_runtime_binary(cli_runtime: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(path) = cli_runtime {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(CliError::Usage(format!(
            "runtime binary not found at --runtime {}",
            path.display()
        )));
    }
    if let Ok(env_path) = std::env::var("SQUARK_RUNTIME_BIN") {
        let p = PathBuf::from(env_path);
        if p.is_file() {
            return Ok(p);
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join(RUNTIME_FILE_NAME);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }
    Ok(PathBuf::from(RUNTIME_FILE_NAME))
}

/// `squark-runtime.exe` on Windows, where `is_file` needs the extension.
const RUNTIME_FILE_NAME: &str = if cfg!(windows) {
    "squark-runtime.exe"
} else {
    "squark-runtime"
};

#[cfg(test)]
mod tests {
    use super::RUNTIME_FILE_NAME;

    #[test]
    fn runtime_file_name_carries_the_platform_executable_suffix() {
        assert_eq!(
            RUNTIME_FILE_NAME,
            format!("squark-runtime{}", std::env::consts::EXE_SUFFIX)
        );
    }

    #[test]
    fn a_macos_bundle_or_its_executable_screenshots_the_bundled_pack() {
        let dir = tempfile::tempdir().unwrap();
        let contents = dir.path().join("Counter.app/Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        std::fs::create_dir_all(contents.join("Resources")).unwrap();
        let executable = contents.join("MacOS/Counter");
        let pack = contents.join("Resources/app.sqpak");
        std::fs::write(&executable, b"runtime").unwrap();
        std::fs::write(&pack, b"SQPK").unwrap();
        for input in [dir.path().join("Counter.app"), executable] {
            let (resolved, guard) = super::prepare_pack_input(&input).unwrap();
            assert_eq!(resolved, pack);
            assert!(guard.is_none(), "the bundle's pack is used in place");
        }
    }
}
