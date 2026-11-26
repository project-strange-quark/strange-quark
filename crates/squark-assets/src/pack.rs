// SPDX-License-Identifier: MIT
use std::fs::File;
use std::io::{self, Read};
use std::ops::Range;

#[cfg(target_os = "linux")]
use memmap2::Mmap;
#[cfg(target_os = "linux")]
use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};

use squark_types::pack::{
    DOCUMENT_PATH, MIME_CUSTOM, PACK_ENTRY_SIZE, PACK_HEADER_SIZE, PackEntry, PackHeader,
    PackTrailer, SQPK_MAGIC, TRAILER_SIZE, fnv1a_64, is_valid_pack_path, mime_from_id,
};

use crate::{InitialDocument, PackError, range};

/// The pack's private, immutable copy. Linux maps a sealed memfd so the pages
/// are shared and cannot change under us; other platforms have no sealed
/// anonymous files, so they keep the copy on the heap.
#[cfg(target_os = "linux")]
type PackBytes = Mmap;
#[cfg(not(target_os = "linux"))]
type PackBytes = Box<[u8]>;

#[derive(Debug)]
pub struct Pack {
    bytes: PackBytes,
    index: Range<usize>,
    pool: Range<usize>,
    data: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Asset<'a> {
    pub mime: &'a str,
    pub bytes: &'a [u8],
}

impl Pack {
    /// Open either a standalone pack or an executable with a version 1 trailer.
    /// Copy only the pack payload, leaving the caller's file cursor untouched.
    pub fn open(runtime: &File) -> Result<Self, PackError> {
        let file_len = runtime.metadata()?.len();
        let mut magic = [0; 8];
        read_exact_at(runtime, &mut magic, 0)?;
        let (offset, len, crc) = if magic == SQPK_MAGIC {
            if file_len > u64::from(u32::MAX) {
                return Err(PackError::Invalid("standalone pack length"));
            }
            (0, file_len, None)
        } else {
            let end = file_len
                .checked_sub(TRAILER_SIZE as u64)
                .ok_or(PackError::Invalid("trailer length"))?;
            let mut bytes = [0; TRAILER_SIZE];
            read_exact_at(runtime, &mut bytes, end)?;
            let trailer = PackTrailer::decode(&bytes)?;
            let offset = trailer.pack_offset;
            let len = u64::from(trailer.pack_len);
            if offset.checked_add(len) != Some(end) {
                return Err(PackError::Invalid("trailer pack range"));
            }
            (offset, len, Some(trailer.pack_crc32))
        };
        if len < PACK_HEADER_SIZE as u64 {
            return Err(PackError::Invalid("pack length"));
        }

        let bytes = stable_copy(runtime, offset, len)?;
        if crc.is_some_and(|expected| crc32fast::hash(&bytes) != expected) {
            return Err(PackError::Invalid("pack CRC32"));
        }

        Self::validate(bytes)
    }

    fn validate(bytes: PackBytes) -> Result<Self, PackError> {
        let header = PackHeader::decode(
            bytes
                .first_chunk()
                .ok_or(PackError::Invalid("pack header"))?,
        )?;
        let index = range(
            u64::from(header.index_offset),
            u64::from(header.entry_count) * PACK_ENTRY_SIZE as u64,
            bytes.len(),
        )
        .ok_or(PackError::Invalid("pack index range"))?;
        let pool = range(
            u64::from(header.strings_offset),
            u64::from(header.strings_len),
            bytes.len(),
        )
        .ok_or(PackError::Invalid("pack string pool range"))?;
        let data = header.data_offset as usize;
        if index.end > pool.start || pool.end > data || data > bytes.len() {
            return Err(PackError::Invalid("pack section order or alignment"));
        }
        let pack = Self {
            bytes,
            index,
            pool,
            data,
        };
        let mut previous = None;
        for raw in pack.entries() {
            let entry = PackEntry::decode(raw)?;
            let path = pack
                .path(entry)
                .ok_or(PackError::Invalid("entry path range or UTF-8"))?;
            if !is_valid_pack_path(path)
                || entry.path_hash != fnv1a_64(path.as_bytes())
                || previous.is_some_and(|previous| previous >= path)
            {
                return Err(PackError::Invalid("entry path, hash or ordering"));
            }
            previous = Some(path);
            if pack.payload(entry).is_none() || pack.mime(entry).is_none() {
                return Err(PackError::Invalid("entry data or MIME"));
            }
        }
        Ok(pack)
    }

    fn entries(&self) -> &[[u8; PACK_ENTRY_SIZE]] {
        self.bytes[self.index.clone()].as_chunks().0
    }

    fn pool_str(&self, offset: u32, len: u16) -> Option<&str> {
        let pool = &self.bytes[self.pool.clone()];
        str::from_utf8(&pool[range(offset.into(), len.into(), pool.len())?]).ok()
    }

    fn path(&self, entry: PackEntry) -> Option<&str> {
        self.pool_str(entry.path_off, entry.path_len)
    }

    fn mime(&self, entry: PackEntry) -> Option<&str> {
        if entry.mime_id == MIME_CUSTOM {
            self.pool_str(entry.mime_off, entry.mime_len)
                .filter(|s| !s.is_empty() && !s.contains(['\0', '\r', '\n']))
        } else {
            mime_from_id(entry.mime_id)
        }
    }

    fn payload(&self, entry: PackEntry) -> Option<&[u8]> {
        let data = &self.bytes[self.data..];
        Some(&data[range(entry.data_off.into(), entry.data_len.into(), data.len())?])
    }

    pub fn entry(&self, canonical_path: &str) -> Option<Asset<'_>> {
        let entries = self.entries();
        let i = entries
            .binary_search_by(|raw| {
                let entry = PackEntry::decode(raw).expect("validated entry");
                self.path(entry)
                    .expect("validated path")
                    .cmp(canonical_path)
            })
            .ok()?;
        let entry = PackEntry::decode(&entries[i]).expect("validated entry");
        Some(Asset {
            mime: self.mime(entry)?,
            bytes: self.payload(entry)?,
        })
    }

    pub fn document(&self) -> Result<InitialDocument<'_>, PackError> {
        let asset = self
            .entry(DOCUMENT_PATH)
            .ok_or(PackError::MissingDocument)?;
        InitialDocument::parse(asset.bytes)
    }
}

/// Copies `len` bytes at `offset` into a sealed, read-only memfd and maps it.
#[cfg(target_os = "linux")]
fn stable_copy(runtime: &File, offset: u64, len: u64) -> Result<PackBytes, PackError> {
    let mut sealed = File::from(
        memfd_create(
            "squark-pack",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )
        .map_err(io::Error::from)?,
    );
    if io::copy(
        &mut FileAt {
            file: runtime,
            offset,
        }
        .take(len),
        &mut sealed,
    )? != len
    {
        return Err(PackError::Invalid("truncated pack"));
    }
    fcntl_add_seals(
        &sealed,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )
    .map_err(io::Error::from)?;
    Mmap::map_if_safe(&sealed).map_err(PackError::Map)
}

/// Copies `len` bytes at `offset` onto the heap.
#[cfg(not(target_os = "linux"))]
fn stable_copy(runtime: &File, offset: u64, len: u64) -> Result<PackBytes, PackError> {
    let len = usize::try_from(len).map_err(|_| PackError::Invalid("pack length"))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(len)
        .map_err(|_| PackError::Invalid("pack length"))?;
    FileAt {
        file: runtime,
        offset,
    }
    .take(len as u64)
    .read_to_end(&mut bytes)?;
    if bytes.len() != len {
        return Err(PackError::Invalid("truncated pack"));
    }
    Ok(bytes.into_boxed_slice())
}

/// Reads at `offset` without moving the caller's cursor.
#[cfg(unix)]
fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, bytes, offset)
}

/// Reads at `offset`. Windows' positional read moves the shared cursor, so
/// it is put back afterwards.
#[cfg(windows)]
fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::io::{Seek, SeekFrom};
    let mut cursor = file;
    let saved = cursor.stream_position()?;
    let read = std::os::windows::fs::FileExt::seek_read(file, bytes, offset);
    cursor.seek(SeekFrom::Start(saved))?;
    read
}

fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match read_at(file, bytes, offset) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => {
                bytes = &mut bytes[read..];
                offset += read as u64;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

struct FileAt<'a> {
    file: &'a File,
    offset: u64,
}

impl Read for FileAt<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = read_at(self.file, bytes, self.offset)?;
        self.offset += read as u64;
        Ok(read)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn source(bytes: &[u8]) -> File {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(bytes).unwrap();
        file
    }

    fn fixture() -> Vec<u8> {
        let path = "/Case/Site.css";
        let mut pack = SQPK_MAGIC.to_vec();
        pack.extend(
            [1u32, 32, 64, path.len() as u32, 128, 0]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        pack.extend(fnv1a_64(path.as_bytes()).to_le_bytes());
        pack.extend(0u32.to_le_bytes());
        pack.extend((path.len() as u16).to_le_bytes());
        pack.extend(3u16.to_le_bytes());
        pack.extend([0u32, 3, 0, 0].into_iter().flat_map(u32::to_le_bytes));
        pack.extend_from_slice(path.as_bytes());
        pack.resize(128, 0);
        pack.extend_from_slice(b"a{}");
        pack
    }

    #[test]
    fn opens_appended_executable_and_keeps_a_stable_copy() {
        let bytes = fixture();
        let mut executable = vec![0; 4096];
        executable.extend_from_slice(&bytes);
        executable.extend_from_slice(&squark_types::pack::TRAILER_MAGIC);
        executable.extend([1u32, 0].into_iter().flat_map(u32::to_le_bytes));
        executable.extend(4096u64.to_le_bytes());
        executable.extend(
            [bytes.len() as u32, crc32fast::hash(&bytes)]
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );

        let file = source(&executable);
        let pack = Pack::open(&file).unwrap();
        file.set_len(0).unwrap();
        let asset = pack.entry("/Case/Site.css").unwrap();
        assert_eq!(asset.mime, "text/css; charset=utf-8");
        assert_eq!(asset.bytes, b"a{}");
        assert!(pack.entry("/case/site.css").is_none());
        assert!(matches!(pack.document(), Err(PackError::MissingDocument)));

        executable[4096 + 130] ^= 1;
        assert!(matches!(
            Pack::open(&source(&executable)),
            Err(PackError::Invalid("pack CRC32"))
        ));
    }

    #[test]
    fn rejects_out_of_range_or_noncanonical_entries_before_lookup() {
        let mut pack = fixture();
        assert!(Pack::open(&source(&pack)).is_ok());
        pack[48..52].copy_from_slice(&0xfffffff8u32.to_le_bytes());
        assert!(Pack::open(&source(&pack)).is_err());
        let mut pack = fixture();
        let path = b"/Case/../a.css";
        pack[64..64 + path.len()].copy_from_slice(path);
        pack[32..40].copy_from_slice(&fnv1a_64(path).to_le_bytes());
        assert!(Pack::open(&source(&pack)).is_err());
        let mut pack = fixture();
        pack[5] = 2;
        assert!(Pack::open(&source(&pack)).is_err());
    }

    #[test]
    fn rejects_duplicate_paths() {
        let original = fixture();
        let mut pack = original[..32].to_vec();
        pack[8..12].copy_from_slice(&2u32.to_le_bytes());
        pack[16..20].copy_from_slice(&96u32.to_le_bytes());
        pack[24..28].copy_from_slice(&192u32.to_le_bytes());
        pack.extend_from_slice(&original[32..64]);
        pack.extend_from_slice(&original[32..64]);
        pack.extend_from_slice(&original[64..128]);
        pack.resize(192, 0);
        pack.extend_from_slice(b"a{}");
        assert!(matches!(
            Pack::open(&source(&pack)),
            Err(PackError::Invalid("entry path, hash or ordering"))
        ));
    }
}

#[cfg(test)]
mod golden {
    use super::*;
    use squark_types::pack::{QuirksMode, ScriptKind};

    const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/pack");

    fn load(name: &str) -> Pack {
        Pack::open(&File::open(format!("{FIXTURES}/{name}")).unwrap()).unwrap()
    }

    #[test]
    fn reads_the_writer_golden_packs_and_document_modes() {
        let bytes = std::fs::read(format!("{FIXTURES}/basic.document.bin")).unwrap();
        for name in ["basic.sqpak", "basic.appended.bin"] {
            let pack = load(name);
            let asset = pack.entry(DOCUMENT_PATH).unwrap();
            assert_eq!(asset.bytes, bytes);
            let doc = pack.document().unwrap();
            assert_eq!(doc.quirks, QuirksMode::Standards);
            assert_eq!(doc.batches().len(), 1);
            let sheet = doc
                .scripts()
                .find(|script| script.kind == ScriptKind::ExternalStylesheet)
                .unwrap();
            assert_eq!(sheet.payload.to_string_lossy(), "/styles/app.css");
            assert_eq!(
                pack.entry("/styles/app.css").unwrap().mime,
                "text/css; charset=utf-8"
            );
        }

        let pack = load("custom-mime.sqpak");
        assert_eq!(
            pack.entry("/data/sample.custom").unwrap().mime,
            "application/vnd.squark.test"
        );
        let quirks = [
            ("quirks.document.bin", QuirksMode::Quirks),
            ("limited-quirks.document.bin", QuirksMode::LimitedQuirks),
        ];
        for (name, mode) in quirks {
            let bytes = std::fs::read(format!("{FIXTURES}/{name}")).unwrap();
            assert_eq!(InitialDocument::parse(&bytes).unwrap().quirks, mode);
        }
    }
}
