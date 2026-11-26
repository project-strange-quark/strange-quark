// SPDX-License-Identifier: MIT
//! macOS app bundles.
//!
//! Appending a pack to a Mach-O runtime puts bytes after its code signature,
//! so the result fails `codesign --verify --strict`, cannot be notarized,
//! and Gatekeeper blocks it once downloaded. On macOS `squark build` writes an
//! app bundle instead: the runtime's code in `Contents/MacOS`, the pack in
//! `Contents/Resources`, an `Info.plist`, and a signature over the whole
//! bundle, which replaces the runtime's own linker signature. squark-runtime
//! finds the pack there (`BUNDLE_PACK_NAME`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use squark_types::pack::BUNDLE_PACK_NAME;

use crate::error::CliError;

/// `MH_MAGIC_64`, as the little-endian bytes of a 64-bit Mach-O file.
const MACH_O_64: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];
/// `FAT_MAGIC`, stored big-endian, for a universal binary.
const MACH_O_FAT: [u8; 4] = [0xca, 0xfe, 0xba, 0xbe];

/// True when `runtime` is a 64-bit or universal Mach-O executable, so the
/// app ships as a bundle rather than a single file.
pub fn is_mach_o(runtime: &[u8]) -> bool {
    matches!(runtime.first_chunk::<4>(), Some(&MACH_O_64 | &MACH_O_FAT))
}

/// What the bundle's `Info.plist` says about the app.
#[derive(Clone, Debug)]
pub struct BundleInfo<'a> {
    /// The app name from config; names the executable and the bundle ID.
    pub name: &'a str,
    /// The name Finder, the Dock and the menu bar show.
    pub display_name: &'a str,
}

/// A written bundle.
#[derive(Clone, Debug)]
pub struct AppBundle {
    /// The `.app` directory.
    pub bundle: PathBuf,
    /// `Contents/MacOS/<name>`, the file to launch directly.
    pub executable: PathBuf,
}

/// How a written bundle gets its signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signing {
    /// Ad-hoc sign with `codesign` when it is available (on macOS), which
    /// seals the pack and Info.plist into the signature.
    AdHocIfAvailable,
    /// Leave the runtime's own linker signature (for tests on fake runtimes).
    Skip,
}

/// The bundle path for `out`: `out` itself when it ends in `.app`, otherwise
/// `out` with `.app` appended (`dist/my-app` becomes `dist/my-app.app`).
pub fn bundle_path(out: &Path) -> PathBuf {
    if out.extension().is_some_and(|ext| ext == "app") {
        out.to_path_buf()
    } else {
        let mut name = out.as_os_str().to_owned();
        name.push(".app");
        PathBuf::from(name)
    }
}

/// The pack inside an app bundle, for a path that is the bundle itself or
/// the executable in its `Contents/MacOS`, matching how squark-runtime finds it.
pub fn pack_in_bundle(input: &Path) -> Option<PathBuf> {
    let contents = if input.is_dir() {
        input.join("Contents")
    } else {
        input
            .parent()
            .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))?
            .parent()?
            .to_path_buf()
    };
    let pack = contents.join("Resources").join(BUNDLE_PACK_NAME);
    pack.is_file().then_some(pack)
}

/// Writes the bundle for `out`, replacing an earlier bundle at that path.
pub fn write_app_bundle(
    out: &Path,
    runtime: &[u8],
    sqpak: &[u8],
    info: &BundleInfo<'_>,
    signing: Signing,
) -> Result<AppBundle, CliError> {
    let bundle = bundle_path(out);
    let executable_name = executable_name(&bundle)?;
    let contents = bundle.join("Contents");
    if bundle.exists() {
        // Only ever replace something that is an app bundle.
        if !contents.join("Info.plist").is_file() {
            return Err(CliError::Usage(format!(
                "{} exists and is not an app bundle; refusing to replace it",
                bundle.display()
            )));
        }
        fs::remove_dir_all(&bundle).map_err(|source| io(&bundle, source))?;
    }
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    for dir in [&macos, &resources] {
        fs::create_dir_all(dir).map_err(|source| io(dir, source))?;
    }

    let executable = macos.join(&executable_name);
    write(&executable, runtime)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755))
            .map_err(|source| io(&executable, source))?;
    }
    write(&resources.join(BUNDLE_PACK_NAME), sqpak)?;
    write(
        &contents.join("Info.plist"),
        info_plist(info, &executable_name).as_bytes(),
    )?;
    write(&contents.join("PkgInfo"), b"APPL????")?;

    if signing == Signing::AdHocIfAvailable {
        sign_ad_hoc(&bundle)?;
    }
    Ok(AppBundle { bundle, executable })
}

fn executable_name(bundle: &Path) -> Result<String, CliError> {
    bundle
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            CliError::Usage(format!(
                "cannot name an app bundle executable from {}",
                bundle.display()
            ))
        })
}

/// `CFBundleIdentifier` allows only letters, digits, `-` and `.`.
fn bundle_identifier(name: &str) -> String {
    let id: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = id.trim_matches('-');
    format!(
        "app.strange-quark.{}",
        if id.is_empty() { "app" } else { id }
    )
}

fn info_plist(info: &BundleInfo<'_>, executable: &str) -> String {
    let entries = [
        ("CFBundleDevelopmentRegion", Value::Str("en")),
        ("CFBundleDisplayName", Value::Str(info.display_name)),
        ("CFBundleExecutable", Value::Str(executable)),
        (
            "CFBundleIdentifier",
            Value::Str(&bundle_identifier(info.name)),
        ),
        ("CFBundleInfoDictionaryVersion", Value::Str("6.0")),
        ("CFBundleName", Value::Str(info.name)),
        ("CFBundlePackageType", Value::Str("APPL")),
        ("CFBundleShortVersionString", Value::Str("1.0")),
        ("CFBundleVersion", Value::Str("1")),
        // docs/architecture.md §14: macOS 12+.
        ("LSMinimumSystemVersion", Value::Str("12.0")),
        ("NSHighResolutionCapable", Value::Bool(true)),
        // Lets dual-GPU Macs stay on the integrated GPU.
        ("NSSupportsAutomaticGraphicsSwitching", Value::Bool(true)),
    ]
    .map(|(key, value)| {
        let value = match value {
            Value::Str(text) => format!("<string>{}</string>", xml_escape(text)),
            Value::Bool(true) => "<true/>".to_owned(),
            Value::Bool(false) => "<false/>".to_owned(),
        };
        format!("\t<key>{key}</key>\n\t{value}\n")
    })
    .concat();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n{entries}</dict>\n</plist>\n"
    )
}

enum Value<'a> {
    Str(&'a str),
    Bool(bool),
}

fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Signs the bundle ad hoc. Without `codesign` (packing on Linux for a Mac),
/// the runtime keeps its own linker signature, which is still valid because
/// the executable is unchanged; the pack is then not sealed.
fn sign_ad_hoc(bundle: &Path) -> Result<(), CliError> {
    let status = match Command::new("codesign")
        // `--` ends the options, so a bundle path starting with `-` is not
        // read as one.
        .args(["--force", "--sign", "-", "--"])
        .arg(bundle)
        .status()
    {
        Ok(status) => status,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "warning: codesign not found; {} keeps the runtime's own signature",
                bundle.display()
            );
            return Ok(());
        }
        Err(source) => return Err(io(bundle, source)),
    };
    if status.success() {
        Ok(())
    } else {
        Err(CliError::Usage(format!(
            "codesign failed to sign {} ({status})",
            bundle.display()
        )))
    }
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    fs::write(path, bytes).map_err(|source| io(path, source))
}

fn io(path: &Path, source: std::io::Error) -> CliError {
    CliError::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_mach_o_and_universal_binaries_only() {
        assert!(is_mach_o(&[0xcf, 0xfa, 0xed, 0xfe, 0x0c, 0, 0, 1]));
        assert!(is_mach_o(&[0xca, 0xfe, 0xba, 0xbe, 0, 0, 0, 2]));
        assert!(!is_mach_o(b"\x7fELF\x02\x01\x01\x00"));
        assert!(!is_mach_o(b"MZ\x90\x00"));
        assert!(!is_mach_o(b"\xcf\xfa"));
    }

    #[test]
    fn bundle_path_appends_app_unless_present() {
        assert_eq!(
            bundle_path(Path::new("dist/my-app")),
            Path::new("dist/my-app.app")
        );
        assert_eq!(
            bundle_path(Path::new("dist/My App.app")),
            Path::new("dist/My App.app")
        );
    }

    #[test]
    fn info_plist_escapes_names_and_sanitises_the_identifier() {
        let plist = info_plist(
            &BundleInfo {
                name: "my app_1",
                display_name: "Tom & Jerry <beta>",
            },
            "my app_1",
        );
        assert!(plist.contains("<string>Tom &amp; Jerry &lt;beta&gt;</string>"));
        assert!(plist.contains("<string>app.strange-quark.my-app-1</string>"));
        assert!(plist.contains("<key>CFBundleExecutable</key>\n\t<string>my app_1</string>"));
        assert!(plist.contains("<key>NSHighResolutionCapable</key>\n\t<true/>"));
    }

    #[test]
    fn writes_the_bundle_layout_and_replaces_only_bundles() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("dist/counter");
        let runtime = [&MACH_O_64[..], b"runtime"].concat();
        let info = BundleInfo {
            name: "counter",
            display_name: "Counter",
        };
        let written = write_app_bundle(&out, &runtime, b"SQPK", &info, Signing::Skip).unwrap();
        assert_eq!(written.bundle, dir.path().join("dist/counter.app"));
        assert_eq!(
            written.executable,
            dir.path().join("dist/counter.app/Contents/MacOS/counter")
        );
        assert_eq!(fs::read(&written.executable).unwrap(), runtime);
        let contents = written.bundle.join("Contents");
        assert_eq!(
            fs::read(contents.join("Resources").join(BUNDLE_PACK_NAME)).unwrap(),
            b"SQPK"
        );
        assert_eq!(fs::read(contents.join("PkgInfo")).unwrap(), b"APPL????");
        let pack = contents.join("Resources").join(BUNDLE_PACK_NAME);
        assert_eq!(pack_in_bundle(&written.bundle), Some(pack.clone()));
        assert_eq!(pack_in_bundle(&written.executable), Some(pack));
        assert_eq!(pack_in_bundle(&contents.join("PkgInfo")), None);

        // Rebuilding replaces the old bundle and leaves no stale files.
        fs::write(contents.join("Resources/stale"), b"x").unwrap();
        write_app_bundle(&out, &runtime, b"SQPK2", &info, Signing::Skip).unwrap();
        assert!(!contents.join("Resources/stale").exists());

        // A directory that is not a bundle is never removed.
        let other = dir.path().join("dist/other.app");
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join("keep"), b"x").unwrap();
        assert!(write_app_bundle(&other, &runtime, b"SQPK", &info, Signing::Skip).is_err());
        assert!(other.join("keep").exists());
    }
}
