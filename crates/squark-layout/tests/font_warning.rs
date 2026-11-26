// SPDX-License-Identifier: MIT
//! Off Linux, warn once when the system test fonts are missing: the committed
//! Chromium baselines and the Linux layout numbers were made with them.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Once;

static WARN: Once = Once::new();

const FAMILIES: [(&str, &str); 2] = [
    ("Liberation Sans", "liberationsans"),
    ("DejaVu Sans", "dejavusans"),
];

/// Prints one warning per test process when a family from `FAMILIES` is absent.
pub fn warn_if_missing() {
    WARN.call_once(|| {
        let dirs = font_dirs();
        let missing = missing_fonts(&dirs);
        if missing.is_empty() {
            return;
        }
        let checked = dirs
            .iter()
            .map(|dir| dir.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let message = format!(
            "WARNING: {} not found in {}.\n  \
             squark-layout's system-font tests and the reftests that render text will differ \
             from the Linux numbers/baselines.\n  \
             Install them on macOS with `brew install --cask font-liberation font-dejavu` \
             (or install the Liberation and DejaVu families on Windows).\n",
            missing.join(" and "),
            checked,
        );
        let _ = std::io::stderr().write_all(message.as_bytes());
    });
}

fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if cfg!(target_os = "macos") {
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(PathBuf::from(home).join("Library/Fonts"));
        }
        dirs.push(PathBuf::from("/Library/Fonts"));
    } else if cfg!(target_os = "windows") {
        let windir =
            std::env::var_os("WINDIR").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Microsoft/Windows/Fonts"));
        }
    }
    dirs
}

fn missing_fonts(dirs: &[PathBuf]) -> Vec<&'static str> {
    let mut names = Vec::new();
    for dir in dirs {
        collect(dir, &mut names);
    }
    FAMILIES
        .iter()
        .filter(|(_, needle)| !names.iter().any(|name| name.contains(needle)))
        .map(|(family, _)| *family)
        .collect()
}

fn collect(dir: &Path, names: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, names);
        } else if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            names.push(name.to_ascii_lowercase());
        }
    }
}
