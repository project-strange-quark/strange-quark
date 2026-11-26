// SPDX-License-Identifier: MIT

use std::{
    env,
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};

use ring::digest::{Context, SHA256, digest};

const MIN_SOURCE_BYTES: usize = 64 * 1024;
const MAX_CACHE_BYTES: usize = 8 * 1024 * 1024;
const CHECKSUM_BYTES: usize = 32;

pub(crate) fn root(override_root: Option<&Path>) -> Option<PathBuf> {
    let base = match override_root {
        Some(root) => root.to_owned(),
        None => cache_directory_with(|name| env::var_os(name))?.join("strange-quark"),
    };
    let version = v8::script_compiler::cached_data_version_tag();
    Some(base.join(format!("modules-v1-{version:08x}")))
}

fn cache_directory_with(
    mut get_env: impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let xdg = absolute_env("XDG_CACHE_HOME", &mut get_env);
    #[cfg(target_os = "linux")]
    let base = xdg.or_else(|| absolute_env("HOME", &mut get_env).map(|home| home.join(".cache")));
    #[cfg(target_os = "macos")]
    let base =
        xdg.or_else(|| absolute_env("HOME", &mut get_env).map(|home| home.join("Library/Caches")));
    #[cfg(target_os = "windows")]
    let base = xdg.or_else(|| absolute_env("LOCALAPPDATA", &mut get_env));
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let base = xdg;
    base
}

fn absolute_env(
    name: &str,
    get_env: &mut impl FnMut(&str) -> Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    get_env(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

pub(crate) fn entry(root: &Path, url: &str, source: &str) -> Option<PathBuf> {
    if source.len() < MIN_SOURCE_BYTES {
        return None;
    }
    let mut hash = Context::new(&SHA256);
    hash.update(url.as_bytes());
    hash.update(b"\0");
    hash.update(source.as_bytes());
    let mut name = String::with_capacity(CHECKSUM_BYTES * 2);
    for byte in hash.finish().as_ref() {
        write!(name, "{byte:02x}").unwrap();
    }
    Some(root.join(name))
}

pub(crate) fn read(path: &Path) -> Option<Vec<u8>> {
    // Only consume data generated in the per-user cache. Packs provide source
    // text, never serialized V8 data.
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    let mut file = options.open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let size = usize::try_from(metadata.len()).ok()?;
    if !(CHECKSUM_BYTES + 1..=MAX_CACHE_BYTES + CHECKSUM_BYTES).contains(&size) {
        return None;
    }
    let mut checksum = [0; CHECKSUM_BYTES];
    file.read_exact(&mut checksum).ok()?;
    let size = size - CHECKSUM_BYTES;
    let mut bytes = Vec::with_capacity(size);
    file.take((size + 1) as u64).read_to_end(&mut bytes).ok()?;
    if bytes.len() != size {
        return None;
    }
    (digest(&SHA256, &bytes).as_ref() == checksum).then_some(bytes)
}

pub(crate) fn write_entries(entries: Vec<(PathBuf, Vec<u8>)>) {
    if !entries.is_empty() {
        let _ = std::thread::Builder::new()
            .name("squark-module-cache".into())
            .spawn(move || {
                for (path, bytes) in entries {
                    let _ = write(&path, &bytes);
                }
            });
    }
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_CACHE_BYTES {
        return Ok(());
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let mut directory = fs::DirBuilder::new();
    directory.recursive(true);
    #[cfg(unix)]
    directory.mode(0o700);
    directory.create(parent)?;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temporary = parent.join(format!(
        ".pending-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    let result = file
        .write_all(digest(&SHA256, bytes).as_ref())
        .and_then(|()| file.write_all(bytes))
        .and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::env;

    use super::cache_directory_with;

    #[test]
    fn cache_directory_uses_xdg_override() {
        let xdg = env::temp_dir().join("strange-quark-xdg-cache");
        let root = cache_directory_with(|name| match name {
            "XDG_CACHE_HOME" => Some(xdg.clone().into_os_string()),
            _ => None,
        });
        assert_eq!(root, Some(xdg));
    }

    #[test]
    fn cache_directory_uses_platform_default() {
        let home = env::temp_dir().join("strange-quark-test-home");
        let local_appdata = env::temp_dir().join("strange-quark-test-local-appdata");
        let root = cache_directory_with(|name| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "LOCALAPPDATA" => Some(local_appdata.clone().into_os_string()),
            _ => None,
        });

        #[cfg(target_os = "linux")]
        let expected = home.join(".cache");
        #[cfg(target_os = "macos")]
        let expected = home.join("Library/Caches");
        #[cfg(target_os = "windows")]
        let expected = local_appdata;

        assert_eq!(root, Some(expected));
    }
}
