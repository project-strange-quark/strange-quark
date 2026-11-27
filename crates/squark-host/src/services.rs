// SPDX-License-Identifier: MIT

use std::{
    borrow::Cow,
    ffi::OsString,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::UNIX_EPOCH,
};

use serde::Deserialize;
use squark_js::document::DocumentTask;
use squark_types::host::{ClipboardItem, FileHandle, HostError, HostRequest, HostValue};
use url::Url;

use crate::{failure, native::WindowSink};

pub(crate) struct HostService {
    requests: Option<mpsc::Sender<HostRequest>>,
    worker: Option<JoinHandle<()>>,
}

impl HostService {
    pub(crate) fn start(
        tasks: mpsc::Sender<DocumentTask>,
        clipboard: ClipboardMode,
        file_dialogs: FileDialogMode,
        fullscreen: FullscreenMode,
        window: WindowSink,
    ) -> Result<Self, HostError> {
        let (sender, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("squark-host-services".into())
            .spawn(move || {
                serve(
                    receiver,
                    tasks,
                    clipboard.backend(),
                    file_dialogs.backend(),
                    fullscreen,
                    window,
                )
            })
            .map_err(|error| failure("ThreadError", error))?;
        Ok(Self {
            requests: Some(sender),
            worker: Some(worker),
        })
    }

    pub(crate) fn sender(&self) -> mpsc::Sender<HostRequest> {
        self.requests.as_ref().unwrap().clone()
    }
}

impl Drop for HostService {
    fn drop(&mut self) {
        drop(self.requests.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The fullscreen state of a run without a real window (headless or test
/// control), shared with test control so drivers can read and change it.
#[derive(Clone, Default)]
pub(crate) struct RecordedFullscreen(Arc<AtomicBool>);

impl RecordedFullscreen {
    pub(crate) fn get(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    /// Sets the state, returning the previous one.
    pub(crate) fn set(&self, fullscreen: bool) -> bool {
        self.0.swap(fullscreen, Ordering::AcqRel)
    }
}

/// Forwards a fullscreen request to the window's event loop, which applies it
/// and replies. Returns false when the event loop has closed.
pub(crate) type ForwardFullscreen = Box<dyn Fn(u32, bool) -> bool + Send>;

pub(crate) enum FullscreenMode {
    Recorded(RecordedFullscreen),
    Window(ForwardFullscreen),
}

pub(crate) trait ClipboardBackend {
    fn read(&mut self) -> Result<Box<[ClipboardItem]>, HostError>;
    fn write(&mut self, items: &[ClipboardItem]) -> Result<(), HostError>;
}

#[derive(Clone, Default)]
pub(crate) struct MemoryClipboard(Arc<Mutex<Vec<ClipboardItem>>>);

impl MemoryClipboard {
    pub(crate) fn items(&self) -> Vec<ClipboardItem> {
        self.0.lock().unwrap().clone()
    }

    pub(crate) fn replace(&self, items: Vec<ClipboardItem>) {
        *self.0.lock().unwrap() = items;
    }
}

pub(crate) enum ClipboardMode {
    System,
    Memory(MemoryClipboard),
}

impl ClipboardMode {
    fn backend(self) -> Box<dyn ClipboardBackend> {
        match self {
            Self::System => Box::new(SystemClipboard::new()),
            Self::Memory(clipboard) => Box::new(MemoryClipboardBackend(clipboard)),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum FileDialogKind {
    Open,
    Save,
}

#[derive(Default)]
struct FileDialogPaths {
    open: std::collections::VecDeque<PathBuf>,
    save: std::collections::VecDeque<PathBuf>,
}

#[derive(Clone, Default)]
pub(crate) struct FileDialogQueue(Arc<Mutex<FileDialogPaths>>);

impl FileDialogQueue {
    pub(crate) fn push(&self, kind: FileDialogKind, path: PathBuf) {
        let mut paths = self.0.lock().unwrap();
        match kind {
            FileDialogKind::Open => paths.open.push_back(path),
            FileDialogKind::Save => paths.save.push_back(path),
        }
    }

    pub(crate) fn pop(&self, kind: FileDialogKind) -> Option<PathBuf> {
        let mut paths = self.0.lock().unwrap();
        match kind {
            FileDialogKind::Open => paths.open.pop_front(),
            FileDialogKind::Save => paths.save.pop_front(),
        }
    }
}

pub(crate) enum FileDialogMode {
    System,
    Queued(FileDialogQueue),
}

impl FileDialogMode {
    fn backend(self) -> Box<dyn FileDialogBackend> {
        match self {
            Self::System => Box::new(SystemFileDialogs),
            Self::Queued(queue) => Box::new(QueuedFileDialogs(queue)),
        }
    }
}

trait FileDialogBackend {
    fn open(&mut self, accept: &[String], multiple: bool) -> Result<HostValue, HostError>;
    fn save(
        &mut self,
        suggested_name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<HostValue, HostError>;
}

struct QueuedFileDialogs(FileDialogQueue);

impl FileDialogBackend for QueuedFileDialogs {
    fn open(&mut self, _accept: &[String], _multiple: bool) -> Result<HostValue, HostError> {
        let Some(path) = self.0.pop(FileDialogKind::Open) else {
            return Ok(HostValue::Cancelled);
        };
        Ok(HostValue::Files(Box::new([read_file(&path)?])))
    }

    fn save(
        &mut self,
        _suggested_name: &str,
        _mime: &str,
        bytes: &[u8],
    ) -> Result<HostValue, HostError> {
        let Some(path) = self.0.pop(FileDialogKind::Save) else {
            return Ok(HostValue::Cancelled);
        };
        fs::write(&path, bytes).map_err(|error| file_error("write", &path, error))?;
        Ok(HostValue::Done)
    }
}

struct SystemFileDialogs;

impl FileDialogBackend for SystemFileDialogs {
    fn open(&mut self, accept: &[String], multiple: bool) -> Result<HostValue, HostError> {
        let mut dialog = rfd::FileDialog::new();
        for (name, extensions) in accept_filters(accept) {
            dialog = dialog.add_filter(name, &extensions);
        }
        let paths = if multiple {
            dialog.pick_files().unwrap_or_default()
        } else {
            dialog.pick_file().into_iter().collect()
        };
        if paths.is_empty() {
            return Ok(HostValue::Cancelled);
        }
        paths
            .into_iter()
            .map(|path| read_file(&path))
            .collect::<Result<Vec<_>, _>>()
            .map(|files| HostValue::Files(files.into_boxed_slice()))
    }

    fn save(
        &mut self,
        suggested_name: &str,
        mime: &str,
        bytes: &[u8],
    ) -> Result<HostValue, HostError> {
        let mut dialog = rfd::FileDialog::new().set_file_name(suggested_name);
        if let Some(extensions) = mime_extensions(mime) {
            dialog = dialog.add_filter(mime, &extensions);
        }
        let Some(path) = dialog.save_file() else {
            return Ok(HostValue::Cancelled);
        };
        fs::write(&path, bytes).map_err(|error| file_error("write", &path, error))?;
        Ok(HostValue::Done)
    }
}

fn accept_filters(accept: &[String]) -> Vec<(String, Vec<String>)> {
    accept
        .iter()
        .filter_map(|value| {
            let value = value.trim();
            if let Some(extension) = value.strip_prefix('.') {
                return (!extension.is_empty() && !extension.contains('/'))
                    .then(|| (value.to_owned(), vec![extension.to_owned()]));
            }
            let (kind, subtype) = value.split_once('/')?;
            let extensions = if subtype == "*" {
                if kind.is_empty()
                    || !kind
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+'))
                {
                    return None;
                }
                let kind = kind.to_ascii_lowercase();
                mime_guess::get_mime_extensions_str(&format!("{kind}/*"))
                    .unwrap_or_default()
                    .iter()
                    .map(|extension| (*extension).to_owned())
                    .collect::<Vec<_>>()
            } else {
                mime_extensions(value).unwrap_or_default()
            };
            (!extensions.is_empty()).then(|| (value.to_owned(), extensions))
        })
        .collect()
}

fn mime_extensions(mime: &str) -> Option<Vec<String>> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    mime.parse::<mime_guess::mime::Mime>().ok()?;
    mime_guess::get_mime_extensions_str(&mime).map(|extensions| {
        extensions
            .iter()
            .map(|extension| (*extension).to_owned())
            .collect()
    })
}

fn read_file(path: &Path) -> Result<FileHandle, HostError> {
    let bytes = fs::read(path).map_err(|error| file_error("read", path, error))?;
    let metadata =
        fs::metadata(path).map_err(|error| file_error("read metadata for", path, error))?;
    let modified = metadata
        .modified()
        .map_err(|error| file_error("read modification time for", path, error))?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| file_error("read modification time for", path, error))?;
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned();
    Ok(FileHandle {
        name,
        mime: mime_guess::from_path(path)
            .first_or_octet_stream()
            .to_string(),
        last_modified_ms: modified.as_millis().try_into().unwrap_or(u64::MAX),
        bytes: Arc::from(bytes),
    })
}

fn file_error(action: &str, path: &Path, error: impl std::fmt::Display) -> HostError {
    failure(
        "NotAllowedError",
        format_args!("could not {action} {}: {error}", path.display()),
    )
}

struct MemoryClipboardBackend(MemoryClipboard);

impl ClipboardBackend for MemoryClipboardBackend {
    fn read(&mut self) -> Result<Box<[ClipboardItem]>, HostError> {
        Ok(self.0.items().into_boxed_slice())
    }

    fn write(&mut self, items: &[ClipboardItem]) -> Result<(), HostError> {
        self.0.replace(items.to_vec());
        Ok(())
    }
}

struct SystemClipboard {
    clipboard: Result<arboard::Clipboard, String>,
}

impl SystemClipboard {
    fn new() -> Self {
        Self {
            clipboard: arboard::Clipboard::new().map_err(|error| error.to_string()),
        }
    }

    fn clipboard(&mut self) -> Result<&mut arboard::Clipboard, HostError> {
        self.clipboard.as_mut().map_err(|error| {
            failure(
                "NotAllowedError",
                format_args!("could not open system clipboard: {error}"),
            )
        })
    }
}

impl ClipboardBackend for SystemClipboard {
    fn read(&mut self) -> Result<Box<[ClipboardItem]>, HostError> {
        let clipboard = self.clipboard()?;
        let mut items = Vec::new();
        if let Ok(text) = clipboard.get_text() {
            items.push(ClipboardItem {
                mime: "text/plain".into(),
                bytes: Arc::from(text.into_bytes()),
            });
        }
        if let Ok(image) = clipboard.get_image() {
            let width =
                u32::try_from(image.width).map_err(|error| failure("NotAllowedError", error))?;
            let height =
                u32::try_from(image.height).map_err(|error| failure("NotAllowedError", error))?;
            let bytes = crate::encode_png([width, height], image.bytes.as_ref())?;
            items.push(ClipboardItem {
                mime: "image/png".into(),
                bytes: Arc::from(bytes),
            });
        }
        Ok(items.into_boxed_slice())
    }

    fn write(&mut self, items: &[ClipboardItem]) -> Result<(), HostError> {
        let clipboard = self.clipboard()?;
        let mut image_error = None;
        if let Some(item) = items.iter().find(|item| item.mime == "image/png") {
            match decode_png(&item.bytes) {
                Ok((width, height, bytes)) => {
                    clipboard
                        .set_image(arboard::ImageData {
                            width,
                            height,
                            bytes: Cow::Owned(bytes),
                        })
                        .map_err(|error| {
                            failure(
                                "NotAllowedError",
                                format_args!("could not write image to system clipboard: {error}"),
                            )
                        })?;
                    return Ok(());
                }
                Err(error) => image_error = Some(error),
            }
        }
        if let Some(item) = items.iter().find(|item| item.mime == "text/plain")
            && let Ok(text) = std::str::from_utf8(&item.bytes)
        {
            clipboard.set_text(text).map_err(|error| {
                failure(
                    "NotAllowedError",
                    format_args!("could not write text to system clipboard: {error}"),
                )
            })?;
            return Ok(());
        }
        let detail = image_error
            .as_deref()
            .unwrap_or("no supported clipboard item");
        Err(failure("NotAllowedError", detail))
    }
}

fn decode_png(bytes: &[u8]) -> Result<(usize, usize, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let mut decoded = vec![0; reader.output_buffer_size().ok_or("PNG size overflow")?];
    let info = reader
        .next_frame(&mut decoded)
        .map_err(|error| error.to_string())?;
    let pixels = &decoded[..info.buffer_size()];
    let pixel_count = (info.width as usize)
        .checked_mul(info.height as usize)
        .ok_or("PNG dimensions overflow")?;
    let mut rgba = Vec::with_capacity(pixel_count.checked_mul(4).ok_or("PNG size overflow")?);
    match info.color_type {
        png::ColorType::Rgba => rgba.extend_from_slice(pixels),
        png::ColorType::Rgb => {
            for [red, green, blue] in pixels.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[*red, *green, *blue, 0xff]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for [gray, alpha] in pixels.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[*gray, *gray, *gray, *alpha]);
            }
        }
        png::ColorType::Grayscale => {
            for &gray in pixels {
                rgba.extend_from_slice(&[gray, gray, gray, 0xff]);
            }
        }
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    }
    if rgba.len() != pixel_count * 4 {
        return Err("PNG pixel data length does not match its dimensions".into());
    }
    Ok((info.width as usize, info.height as usize, rgba))
}

fn serve(
    requests: mpsc::Receiver<HostRequest>,
    tasks: mpsc::Sender<DocumentTask>,
    mut clipboard: Box<dyn ClipboardBackend>,
    mut file_dialogs: Box<dyn FileDialogBackend>,
    fullscreen: FullscreenMode,
    window: WindowSink,
) {
    while let Ok(request) = requests.recv() {
        let (request, result) = match (request, &fullscreen) {
            (HostRequest::Window { id, op }, _) => {
                // The event loop applies it; the page does not wait for the
                // window system.
                if window(op) {
                    (id, Ok(HostValue::Done))
                } else {
                    (id, Err(failure("WindowError", "the window has closed")))
                }
            }
            (HostRequest::Fullscreen { id, enter }, FullscreenMode::Window(forward)) => {
                // The winit window lives on the event-loop thread, which replies.
                if forward(id, enter) {
                    continue;
                }
                (id, Err(failure("WindowError", "the window has closed")))
            }
            (HostRequest::Fullscreen { id, enter }, FullscreenMode::Recorded(state)) => {
                state.set(enter);
                (id, Ok(HostValue::Done))
            }
            (request, _) => handle(request, clipboard.as_mut(), file_dialogs.as_mut()),
        };
        if tasks
            .send(DocumentTask::HostReply { request, result })
            .is_err()
        {
            break;
        }
    }
}

fn handle(
    request: HostRequest,
    clipboard: &mut dyn ClipboardBackend,
    file_dialogs: &mut dyn FileDialogBackend,
) -> (u32, Result<HostValue, HostError>) {
    match request {
        HostRequest::OpenExternal { id, url } => (id, open_external(&url)),
        HostRequest::ClipboardRead { id } => (id, clipboard.read().map(HostValue::Clipboard)),
        HostRequest::ClipboardWrite { id, items } => {
            (id, clipboard.write(&items).map(|()| HostValue::Done))
        }
        HostRequest::FileOpen {
            id,
            accept,
            multiple,
        } => (id, file_dialogs.open(&accept, multiple)),
        HostRequest::FileSave {
            id,
            suggested_name,
            mime,
            bytes,
        } => (id, file_dialogs.save(&suggested_name, &mime, &bytes)),
        HostRequest::Fullscreen { id, .. } | HostRequest::Window { id, .. } => (
            id,
            Err(failure("WindowError", "window requests are handled by the window")),
        ),
    }
}

fn open_external(url: &str) -> Result<HostValue, HostError> {
    open_external_url(url).map(|()| HostValue::Done)
}

/// Opens an http, https or mailto URL with the system's handler.
pub(crate) fn open_external_url(url: &str) -> Result<(), HostError> {
    let url = parse_external_url(url)?;
    let command = external_open_command(std::env::var_os("SQUARK_OPEN_EXTERNAL_COMMAND"));
    let child = Command::new(command)
        .arg(url.as_str())
        .spawn()
        .map_err(|error| {
            failure(
                "NotAllowedError",
                format_args!("could not open external URL: {error}"),
            )
        })?;
    reap(child);
    Ok(())
}

fn external_open_command(override_command: Option<OsString>) -> OsString {
    override_command.unwrap_or_else(|| OsString::from(default_external_command()))
}

fn default_external_command() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        "xdg-open"
    }
    #[cfg(target_os = "macos")]
    {
        "open"
    }
    #[cfg(target_os = "windows")]
    {
        "explorer.exe"
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        "xdg-open"
    }
}

fn parse_external_url(url: &str) -> Result<Url, HostError> {
    let url = Url::parse(url).map_err(|_| not_allowed("invalid external URL"))?;
    let allowed = match url.scheme() {
        "http" | "https" => url.has_host(),
        "mailto" => !url.path().is_empty(),
        _ => false,
    };
    if allowed {
        Ok(url)
    } else {
        Err(not_allowed("external URL scheme is not allowed"))
    }
}

fn reap(child: Child) {
    let child = Arc::new(Mutex::new(Some(child)));
    let thread_child = Arc::clone(&child);
    match thread::Builder::new()
        .name("squark-open-external-reaper".into())
        .spawn(move || {
            if let Some(mut child) = thread_child.lock().unwrap().take() {
                let _ = child.wait();
            }
        }) {
        Ok(_) => {}
        Err(_) => {
            if let Some(mut child) = child.lock().unwrap().take() {
                let _ = child.wait();
            }
        }
    }
}

fn not_allowed(message: &str) -> HostError {
    HostError {
        name: "NotAllowedError".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use squark_types::host::{ClipboardItem, HostRequest, HostValue};

    use super::{
        ClipboardBackend, FileDialogQueue, MemoryClipboard, MemoryClipboardBackend,
        QueuedFileDialogs, accept_filters, decode_png, external_open_command, handle,
        parse_external_url,
    };

    #[test]
    fn external_open_command_uses_platform_default() {
        #[cfg(target_os = "linux")]
        let expected = "xdg-open";
        #[cfg(target_os = "macos")]
        let expected = "open";
        #[cfg(target_os = "windows")]
        let expected = "explorer.exe";

        assert_eq!(
            external_open_command(None),
            std::ffi::OsString::from(expected)
        );
    }

    #[test]
    fn external_open_command_honors_override() {
        let override_command = std::ffi::OsString::from("custom-opener");
        assert_eq!(
            external_open_command(Some(override_command.clone())),
            override_command
        );
    }

    #[test]
    fn external_urls_are_parsed_and_limited_to_supported_schemes() {
        for value in [
            "https://example.com/a",
            "http://example.com/",
            "mailto:person@example.com",
        ] {
            assert!(parse_external_url(value).is_ok(), "{value}");
        }
        for value in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "tel:+15551234567",
            "https://",
            "mailto:",
        ] {
            let error = parse_external_url(value).unwrap_err();
            assert_eq!(error.name, "NotAllowedError", "{value}");
        }
    }

    #[test]
    fn queued_file_dialogs_cancel_when_their_queue_is_empty() {
        let requests = [
            HostRequest::FileOpen {
                id: 1,
                accept: Box::default(),
                multiple: false,
            },
            HostRequest::FileSave {
                id: 2,
                suggested_name: "file.txt".into(),
                mime: "text/plain".into(),
                bytes: Arc::from([]),
            },
        ];
        let mut clipboard = MemoryClipboardBackend(MemoryClipboard::default());
        let mut file_dialogs = QueuedFileDialogs(FileDialogQueue::default());
        for (request, expected_id) in requests.into_iter().zip(1..=2) {
            let (id, result) = handle(request, &mut clipboard, &mut file_dialogs);
            assert_eq!(id, expected_id);
            assert_eq!(result.unwrap(), HostValue::Cancelled);
        }
    }

    #[test]
    fn file_dialog_accept_filters_cover_extensions_exact_mimes_and_mime_groups() {
        let filters = accept_filters(&[".excalidraw".into(), "image/png".into(), "image/*".into()]);
        assert_eq!(
            filters[0],
            (".excalidraw".into(), vec!["excalidraw".into()])
        );
        assert!(filters.iter().any(|(name, extensions)| {
            name == "image/png" && extensions.contains(&"png".into())
        }));
        assert!(
            filters
                .iter()
                .any(|(name, extensions)| name == "image/*" && extensions.contains(&"png".into()))
        );
    }

    #[test]
    fn in_memory_clipboard_round_trips_all_items() {
        let clipboard = MemoryClipboard::default();
        let mut backend = MemoryClipboardBackend(clipboard.clone());
        let items = vec![
            ClipboardItem {
                mime: "text/plain".into(),
                bytes: Arc::from(&b"hello"[..]),
            },
            ClipboardItem {
                mime: "image/png".into(),
                bytes: Arc::from(&b"png"[..]),
            },
        ];
        backend.write(&items).unwrap();
        assert_eq!(backend.read().unwrap().as_ref(), items);
        assert_eq!(clipboard.items(), items);
    }

    #[test]
    fn png_decode_expands_palette_and_grayscale_into_rgba() {
        let mut encoded = Vec::new();
        let mut encoder = png::Encoder::new(&mut encoded, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[0x45]).unwrap();
        writer.finish().unwrap();

        assert_eq!(
            decode_png(&encoded).unwrap(),
            (1, 1, vec![0x45, 0x45, 0x45, 0xff])
        );
    }
}
