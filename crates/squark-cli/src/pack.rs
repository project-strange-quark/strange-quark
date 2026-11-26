// SPDX-License-Identifier: MIT
//! `.sqpak` archive builder, directory packer, and appended-binary assembler
//! (the formats in `squark_types::pack`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use squark_types::pack::{
    BUILTIN_MIMES, DOCUMENT_PATH, MANIFEST_PATH, MIME_CUSTOM, PACK_DATA_ALIGN,
    PACK_ENTRY_DATA_ALIGN, PACK_ENTRY_SIZE, PACK_HEADER_SIZE, PackEntry, PackHeader, PackTrailer,
    TRAILER_ALIGN, TRAILER_MAGIC, TRAILER_SIZE, fnv1a_64, is_valid_pack_path,
    mime_id_for_extension, mime_id_from_str,
};
use walkdir::WalkDir;

use crate::error::CliError;
use crate::html::{CompiledDocument, compile_html_to_document_bin};

/// MIME specification for a `.sqpak` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MimeSpec {
    Builtin(u16),
    Custom(String),
}

impl MimeSpec {
    /// Picks a built-in MIME ID when possible, falling back to a custom string.
    pub fn from_mime(mime: &str) -> Self {
        match mime_id_from_str(mime) {
            Some(id) => Self::Builtin(id),
            None => Self::Custom(mime.to_owned()),
        }
    }

    /// Infers a MIME spec from a filesystem path's extension.
    pub fn from_path(path: &Path) -> Self {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("xml") {
            return Self::Custom("application/xml".to_owned());
        }
        Self::Builtin(mime_id_for_extension(ext))
    }
}

/// Builds a version 1 `.sqpak` binary archive in memory.
#[derive(Debug, Default)]
pub struct PackBuilder {
    entries: BTreeMap<String, (MimeSpec, Vec<u8>)>,
}

impl PackBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts an entry at a canonical pack path (e.g. `"/assets/app.js"`).
    ///
    /// Rejects invalid paths and duplicates.
    pub fn add_entry(
        &mut self,
        path: impl Into<String>,
        mime: MimeSpec,
        data: Vec<u8>,
    ) -> Result<(), CliError> {
        let path = path.into();
        if !is_valid_pack_path(&path) {
            return Err(CliError::InvalidPackPath { path });
        }
        if self.entries.contains_key(&path) {
            return Err(CliError::DuplicatePackPath { path });
        }
        if let MimeSpec::Builtin(id) = mime
            && usize::from(id) >= BUILTIN_MIMES.len()
        {
            return Err(CliError::PackValidation(format!(
                "built-in mime_id {id} out of range"
            )));
        }
        self.entries.insert(path, (mime, data));
        Ok(())
    }

    /// Serializes the `.sqpak` archive with sorted index, UTF-8 string pool,
    /// 64-byte aligned data section, and 8-byte aligned entry payloads.
    pub fn build(self) -> Result<Vec<u8>, CliError> {
        // `BTreeMap<String, _>` iterates in exact lexicographical UTF-8 byte order.
        let entry_count =
            u32::try_from(self.entries.len()).map_err(|_| CliError::SizeOverflow {
                reason: "entry count exceeds u32::MAX",
            })?;
        let index_offset = PACK_HEADER_SIZE as u32;
        let index_bytes =
            self.entries
                .len()
                .checked_mul(PACK_ENTRY_SIZE)
                .ok_or(CliError::SizeOverflow {
                    reason: "index size overflow",
                })?;
        let strings_offset_usize =
            PACK_HEADER_SIZE
                .checked_add(index_bytes)
                .ok_or(CliError::SizeOverflow {
                    reason: "strings_offset overflow",
                })?;

        let mut string_pool: Vec<u8> = Vec::new();
        let mut data_section: Vec<u8> = Vec::new();
        let mut index_entries: Vec<PackEntry> = Vec::with_capacity(self.entries.len());

        for (path, (mime, payload)) in &self.entries {
            let path_off =
                u32::try_from(string_pool.len()).map_err(|_| CliError::SizeOverflow {
                    reason: "string pool exceeds u32::MAX",
                })?;
            let path_len = u16::try_from(path.len()).map_err(|_| CliError::SizeOverflow {
                reason: "pack path exceeds u16::MAX bytes",
            })?;
            string_pool.extend_from_slice(path.as_bytes());

            let (mime_id, mime_off, mime_len) = match mime {
                MimeSpec::Builtin(id) => (*id, 0u32, 0u16),
                MimeSpec::Custom(custom) => {
                    let off =
                        u32::try_from(string_pool.len()).map_err(|_| CliError::SizeOverflow {
                            reason: "string pool exceeds u32::MAX",
                        })?;
                    let len = u16::try_from(custom.len()).map_err(|_| CliError::SizeOverflow {
                        reason: "custom MIME exceeds u16::MAX bytes",
                    })?;
                    string_pool.extend_from_slice(custom.as_bytes());
                    (MIME_CUSTOM, off, len)
                }
            };

            let aligned_data_off = align_up(data_section.len(), PACK_ENTRY_DATA_ALIGN).ok_or(
                CliError::SizeOverflow {
                    reason: "data section offset overflow",
                },
            )?;
            data_section.resize(aligned_data_off, 0);
            let data_off =
                u32::try_from(data_section.len()).map_err(|_| CliError::SizeOverflow {
                    reason: "data section exceeds u32::MAX",
                })?;
            let data_len = u32::try_from(payload.len()).map_err(|_| CliError::SizeOverflow {
                reason: "entry payload exceeds u32::MAX",
            })?;
            data_section.extend_from_slice(payload);

            index_entries.push(PackEntry {
                path_hash: fnv1a_64(path.as_bytes()),
                path_off,
                path_len,
                mime_id,
                data_off,
                data_len,
                mime_off,
                mime_len,
                entry_flags: 0,
            });
        }

        let strings_end =
            strings_offset_usize
                .checked_add(string_pool.len())
                .ok_or(CliError::SizeOverflow {
                    reason: "string pool end overflow",
                })?;
        let data_offset_usize =
            align_up(strings_end, PACK_DATA_ALIGN).ok_or(CliError::SizeOverflow {
                reason: "data_offset alignment overflow",
            })?;
        let total_size =
            data_offset_usize
                .checked_add(data_section.len())
                .ok_or(CliError::SizeOverflow {
                    reason: "pack total size exceeds usize::MAX",
                })?;
        // Ensure total pack size fits in `PackTrailer::pack_len` (u32).
        let _ = u32::try_from(total_size).map_err(|_| CliError::SizeOverflow {
            reason: "pack total size exceeds 4 GiB (u32::MAX)",
        })?;

        let header = PackHeader {
            entry_count,
            index_offset,
            strings_offset: u32::try_from(strings_offset_usize).map_err(|_| {
                CliError::SizeOverflow {
                    reason: "strings_offset exceeds u32::MAX",
                }
            })?,
            strings_len: u32::try_from(string_pool.len()).map_err(|_| CliError::SizeOverflow {
                reason: "strings_len exceeds u32::MAX",
            })?,
            data_offset: u32::try_from(data_offset_usize).map_err(|_| CliError::SizeOverflow {
                reason: "data_offset exceeds u32::MAX",
            })?,
        };

        let mut out = Vec::with_capacity(total_size);
        out.extend_from_slice(&header.encode());
        for entry in &index_entries {
            out.extend_from_slice(&entry.encode());
        }
        out.extend_from_slice(&string_pool);
        out.resize(data_offset_usize, 0);
        out.extend_from_slice(&data_section);
        Ok(out)
    }
}

/// Appends a `.sqpak` archive and 32-byte `PackTrailer` to a runtime binary buffer.
pub fn append_pack_to_binary(
    runtime_bytes: &[u8],
    sqpak_bytes: &[u8],
) -> Result<Vec<u8>, CliError> {
    let pack_len = u32::try_from(sqpak_bytes.len()).map_err(|_| CliError::SizeOverflow {
        reason: "pack size exceeds 4 GiB (u32::MAX)",
    })?;
    let pack_offset =
        align_up_u64(runtime_bytes.len() as u64, TRAILER_ALIGN).ok_or(CliError::SizeOverflow {
            reason: "runtime binary alignment overflow",
        })?;
    let pack_crc32 = crc32fast::hash(sqpak_bytes);
    let trailer = PackTrailer {
        pack_offset,
        pack_len,
        pack_crc32,
    };

    let pack_offset_usize = usize::try_from(pack_offset).map_err(|_| CliError::SizeOverflow {
        reason: "pack_offset exceeds usize::MAX",
    })?;
    let mut out = Vec::with_capacity(pack_offset_usize + sqpak_bytes.len() + 32);
    out.extend_from_slice(runtime_bytes);
    out.resize(pack_offset_usize, 0);
    out.extend_from_slice(sqpak_bytes);
    out.extend_from_slice(&trailer.encode());
    Ok(out)
}

/// Window defaults written to `/__squark/manifest.json`.
#[derive(Clone, Debug, Serialize)]
pub struct PackWindow {
    pub title: String,
    pub width: u32,
    pub height: u32,
}

/// An asset `squark.config` adds to the pack at a path the app requests.
#[derive(Clone, Debug)]
pub struct ExtraAsset {
    /// Absolute pack path, e.g. `/assets/Assistant-Bold.woff2`.
    pub path: String,
    /// File to read the bytes from.
    pub source: PathBuf,
}

/// Options for packing a built web directory.
#[derive(Clone, Debug)]
pub struct PackOptions {
    pub web_dir: PathBuf,
    pub html_path: Option<PathBuf>,
    pub out_sqpak: Option<PathBuf>,
    pub out_binary: Option<PathBuf>,
    pub runtime_binary: Option<PathBuf>,
    pub include_sourcemaps: bool,
    pub app_name: Option<String>,
    /// When packing a single `.html` case from a shared suite directory (e.g. R1),
    /// omit sibling `.html` files other than `html_path` to keep the pack small.
    pub skip_other_html: bool,
    /// Window title and size from `squark.config`, written to the manifest.
    pub window: Option<PackWindow>,
    /// Allowed network origins/patterns from `squark.config` (`network.allow`) written
    /// to `/__squark/manifest.json` when non-empty.
    pub network_allow: Vec<String>,
    /// Files `squark.config` adds at exact pack paths (see `ExtraAsset`).
    pub extra_assets: Vec<ExtraAsset>,
}

/// Summary of a completed `squark pack` run.
#[derive(Clone, Debug)]
pub struct PackSummary {
    /// The runtime output written for `out_binary`, if any.
    pub output: Option<RuntimeOutput>,
    pub entry_count: usize,
    pub sqpak_bytes: usize,
    pub sqpak_crc32: u32,
    pub entry_html: String,
    pub document: CompiledDocument,
}

#[derive(Serialize)]
struct PackNetworkManifest<'a> {
    allow: &'a [String],
}

#[derive(Serialize)]
struct PackManifest<'a> {
    version: u32,
    app_name: &'a str,
    entry_html: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    window: Option<&'a PackWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    network: Option<PackNetworkManifest<'a>>,
}

/// Packs a web directory into a `.sqpak` archive and optionally appends it to a runtime binary.
pub fn pack_directory(options: &PackOptions) -> Result<PackSummary, CliError> {
    if options.out_binary.is_some() && options.runtime_binary.is_none() {
        return Err(CliError::Usage(
            "--binary requires --runtime <path-to-squark-runtime>".to_owned(),
        ));
    }

    let web_dir = fs::canonicalize(&options.web_dir).map_err(|source| CliError::Io {
        path: options.web_dir.clone(),
        source,
    })?;

    let html_fs_path = match &options.html_path {
        Some(path) => fs::canonicalize(path).map_err(|source| CliError::Io {
            path: path.clone(),
            source,
        })?,
        None => web_dir.join("index.html"),
    };
    if !html_fs_path.is_file() {
        return Err(CliError::MissingEntryHtml { path: html_fs_path });
    }

    let entry_html_pack_path = match html_fs_path.strip_prefix(&web_dir) {
        Ok(rel) => relative_to_pack_path(rel)?,
        Err(_) => "/index.html".to_owned(),
    };

    let html_source = fs::read_to_string(&html_fs_path).map_err(|source| CliError::Io {
        path: html_fs_path.clone(),
        source,
    })?;
    let compiled = compile_html_to_document_bin(&html_source, &entry_html_pack_path)?;

    let app_name = options.app_name.clone().unwrap_or_else(|| {
        web_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("squark-app")
            .to_owned()
    });
    let network = (!options.network_allow.is_empty()).then_some(PackNetworkManifest {
        allow: &options.network_allow,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&PackManifest {
        version: 1,
        app_name: &app_name,
        entry_html: &entry_html_pack_path,
        window: options.window.as_ref(),
        network,
    })?;

    // Avoid accidentally packing the output file if it lives inside `web_dir`.
    let skip_paths: Vec<PathBuf> = [options.out_sqpak.as_ref(), options.out_binary.as_ref()]
        .into_iter()
        .flatten()
        .filter_map(|p| normalize_output_path(p).ok())
        .collect();

    let mut builder = PackBuilder::new();
    builder.add_entry(DOCUMENT_PATH, MimeSpec::Builtin(0), compiled.bytes.clone())?;
    builder.add_entry(MANIFEST_PATH, MimeSpec::Builtin(4), manifest_bytes)?;
    // Declared assets win over nothing: a file that the app's own CSS or code
    // asks for, which the app build did not emit (a Vite asset outside dist, a
    // public/ path, or a URL assembled at run time).
    for asset in &options.extra_assets {
        let bytes = fs::read(&asset.source).map_err(|source| CliError::Io {
            path: asset.source.clone(),
            source,
        })?;
        builder.add_entry(
            asset.path.clone(),
            MimeSpec::from_path(&asset.source),
            bytes,
        )?;
    }

    let mut files: Vec<PathBuf> = Vec::new();
    // A previous macOS `squark build` leaves an app bundle in `dist/`; its pack
    // in Contents/Resources identifies it, so none of it is packed again.
    let walk = WalkDir::new(&web_dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            !(entry.file_type().is_dir() && crate::bundle::pack_in_bundle(entry.path()).is_some())
        });
    for entry in walk {
        let entry = entry.map_err(|err| CliError::Io {
            path: err.path().unwrap_or(&web_dir).to_path_buf(),
            source: err
                .into_io_error()
                .unwrap_or_else(|| std::io::Error::other("walkdir error")),
        })?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.into_path();
        if !options.include_sourcemaps && path.extension().and_then(|e| e.to_str()) == Some("map") {
            continue;
        }
        if options.skip_other_html
            && path != html_fs_path
            && matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("html" | "htm")
            )
        {
            continue;
        }
        if skip_paths.iter().any(|skip| skip == &path) {
            continue;
        }
        // A previous `squark build` output can sit in `dist/` under another name.
        // Its 32-byte SQPACK trailer identifies it, so it is never packed as
        // an asset of the next build.
        if has_pack_trailer(&path)? {
            continue;
        }
        files.push(path);
    }
    files.sort();

    for file_path in files {
        let rel = file_path
            .strip_prefix(&web_dir)
            .expect("walkdir yielded path outside web_dir");
        let pack_path = relative_to_pack_path(rel)?;
        if pack_path == DOCUMENT_PATH || pack_path == MANIFEST_PATH {
            return Err(CliError::DuplicatePackPath { path: pack_path });
        }
        let bytes = fs::read(&file_path).map_err(|source| CliError::Io {
            path: file_path.clone(),
            source,
        })?;
        builder.add_entry(pack_path, MimeSpec::from_path(&file_path), bytes)?;
    }

    let entry_count = builder.entries.len();
    let sqpak = builder.build()?;
    let sqpak_crc32 = crc32fast::hash(&sqpak);

    if let Some(out_sqpak) = &options.out_sqpak {
        if let Some(parent) = out_sqpak.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| CliError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(out_sqpak, &sqpak).map_err(|source| CliError::Io {
            path: out_sqpak.clone(),
            source,
        })?;
    }

    let mut output = None;
    if let (Some(out_binary), Some(runtime_path)) = (&options.out_binary, &options.runtime_binary) {
        let runtime_bytes = fs::read(runtime_path).map_err(|source| CliError::Io {
            path: runtime_path.clone(),
            source,
        })?;
        if crate::bundle::is_mach_o(&runtime_bytes) {
            let name = options.app_name.as_deref().unwrap_or("app");
            let display_name = options
                .window
                .as_ref()
                .map_or(name, |window| window.title.as_str());
            let bundle = crate::bundle::write_app_bundle(
                out_binary,
                &runtime_bytes,
                &sqpak,
                &crate::bundle::BundleInfo { name, display_name },
                crate::bundle::Signing::AdHocIfAvailable,
            )?;
            output = Some(RuntimeOutput {
                path: bundle.bundle,
                executable: bundle.executable,
                bytes: (runtime_bytes.len() + sqpak.len()) as u64,
            });
        } else {
            let out_binary = windows_executable_path(out_binary, &runtime_bytes);
            output = Some(write_appended_binary(&out_binary, &runtime_bytes, &sqpak)?);
        }
    }

    Ok(PackSummary {
        output,
        entry_count,
        sqpak_bytes: sqpak.len(),
        sqpak_crc32,
        entry_html: entry_html_pack_path,
        document: compiled,
    })
}

/// What `squark pack --binary` and `squark build` wrote for the runtime.
#[derive(Clone, Debug)]
pub struct RuntimeOutput {
    /// The app as shipped: the binary itself, or the macOS `.app` bundle.
    pub path: PathBuf,
    /// The file to execute (inside the bundle on macOS).
    pub executable: PathBuf,
    /// Total bytes written for the runtime and its pack.
    pub bytes: u64,
}

/// `out` with `.exe` added when the runtime is a Windows PE executable
/// (magic `MZ`) and `out` has no `.exe` yet: Explorer and `cmd` run only
/// files with an executable extension. Judged by the runtime, not the host,
/// so packing for Windows elsewhere names the file the same way.
pub fn windows_executable_path(out: &Path, runtime: &[u8]) -> PathBuf {
    let is_pe = runtime.starts_with(b"MZ");
    let has_exe = out
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    if is_pe && !has_exe {
        let mut name = out.as_os_str().to_owned();
        name.push(".exe");
        PathBuf::from(name)
    } else {
        out.to_path_buf()
    }
}

/// Appends the pack and its trailer to an ELF or PE runtime.
fn write_appended_binary(
    out_binary: &Path,
    runtime_bytes: &[u8],
    sqpak: &[u8],
) -> Result<RuntimeOutput, CliError> {
    {
        let combined = append_pack_to_binary(runtime_bytes, sqpak)?;
        if let Some(parent) = out_binary.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|source| CliError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(out_binary, &combined).map_err(|source| CliError::Io {
            path: out_binary.to_path_buf(),
            source,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(out_binary, fs::Permissions::from_mode(0o755)).map_err(
                |source| CliError::Io {
                    path: out_binary.to_path_buf(),
                    source,
                },
            )?;
        }
        Ok(RuntimeOutput {
            path: out_binary.to_path_buf(),
            executable: out_binary.to_path_buf(),
            bytes: combined.len() as u64,
        })
    }
}

fn relative_to_pack_path(rel: &Path) -> Result<String, CliError> {
    let mut parts = Vec::new();
    for comp in rel.components() {
        let std::path::Component::Normal(seg) = comp else {
            return Err(CliError::InvalidPackPath {
                path: rel.display().to_string(),
            });
        };
        let Some(s) = seg.to_str() else {
            return Err(CliError::InvalidPackPath {
                path: rel.display().to_string(),
            });
        };
        parts.push(s);
    }
    let joined = format!("/{}", parts.join("/"));
    if !is_valid_pack_path(&joined) {
        return Err(CliError::InvalidPackPath { path: joined });
    }
    Ok(joined)
}

/// True when the file ends with a SQPACK trailer (an appended runtime binary).
fn has_pack_trailer(path: &Path) -> Result<bool, CliError> {
    let mut file = fs::File::open(path).map_err(|source| CliError::Io {
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
    if len < TRAILER_SIZE as u64 {
        return Ok(false);
    }
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::End(-(TRAILER_SIZE as i64)))
        .map_err(|source| CliError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let mut magic = [0u8; 8];
    file.read_exact(&mut magic).map_err(|source| CliError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(magic == TRAILER_MAGIC)
}

fn normalize_output_path(path: &Path) -> Result<PathBuf, std::io::Error> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if let (Some(parent), Some(file_name)) = (abs.parent(), abs.file_name())
        && parent.exists()
    {
        return Ok(fs::canonicalize(parent)?.join(file_name));
    }
    Ok(abs)
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value.checked_add(mask).map(|v| v & !mask)
}

fn align_up_u64(value: u64, align: u64) -> Option<u64> {
    debug_assert!(align.is_power_of_two());
    let mask = align - 1;
    value.checked_add(mask).map(|v| v & !mask)
}
