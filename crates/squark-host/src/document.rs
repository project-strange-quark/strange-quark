// SPDX-License-Identifier: MIT

use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex, Once, mpsc},
    thread::{self, JoinHandle, ScopedJoinHandle},
    time::{Duration, Instant},
};

use squark_assets::Pack;
use squark_compositor::Painter;
use squark_js::{
    BlobRegistry, DocumentShutdown, ScriptSources,
    document::{DocumentOptions, DocumentRunner, DocumentTask, InitialViewport},
};
use squark_layout::StaticDocument;
use squark_types::{
    engine::{ColorScheme, MediaState, PointerMedia},
    host::HostRequest,
    pack::{DOCUMENT_PATH, MANIFEST_PATH, is_valid_pack_path},
    render::{FrameDemand, ResourceSender},
};
use url::Url;
use winit::window::Theme;

use crate::services::{ClipboardMode, FileDialogMode, FullscreenMode, HostService};
use crate::{HostError, canvas::PendingCanvasRequests, failure, pipeline::DocumentPipeline};

include!(concat!(env!("OUT_DIR"), "/squark-dom.rs"));

pub(crate) const DOCUMENT_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) type DocumentThread = Arc<Mutex<Option<JoinHandle<()>>>>;

pub(crate) fn request_document_stop(
    shutdown: &DocumentShutdown,
    tasks: &mpsc::Sender<DocumentTask>,
    pending_canvas: &PendingCanvasRequests,
) {
    shutdown.close();
    let _ = tasks.send(DocumentTask::Shutdown);
    pending_canvas.fail_all();
}

pub(crate) fn join_document_thread(document: &DocumentThread) -> Result<(), HostError> {
    let Some(document) = document.lock().unwrap().take() else {
        return Ok(());
    };
    join_thread(document)
}

pub(crate) fn join_scoped_document_thread<T>(
    document: ScopedJoinHandle<'_, T>,
) -> Result<T, HostError> {
    join_thread(document)
}

fn join_thread<T>(document: impl DocumentJoin<T>) -> Result<T, HostError> {
    let deadline = Instant::now() + DOCUMENT_JOIN_TIMEOUT;
    while !document.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if !document.is_finished() {
        eprintln!(
            "squark-runtime: document thread did not stop within {DOCUMENT_JOIN_TIMEOUT:?}; waiting before renderer teardown"
        );
    }
    document
        .join()
        .map_err(|_| failure("ThreadError", "document thread panicked"))
}

trait DocumentJoin<T> {
    fn is_finished(&self) -> bool;
    fn join(self) -> thread::Result<T>;
}

impl<T> DocumentJoin<T> for JoinHandle<T> {
    fn is_finished(&self) -> bool {
        JoinHandle::is_finished(self)
    }

    fn join(self) -> thread::Result<T> {
        JoinHandle::join(self)
    }
}

impl<T> DocumentJoin<T> for ScopedJoinHandle<'_, T> {
    fn is_finished(&self) -> bool {
        ScopedJoinHandle::is_finished(self)
    }

    fn join(self) -> thread::Result<T> {
        ScopedJoinHandle::join(self)
    }
}

pub(crate) struct DocumentChannels {
    pub(crate) resources: ResourceSender,
    pub(crate) host_requests: mpsc::Sender<HostRequest>,
    pub(crate) canvas: Option<crate::canvas::Backend>,
}

pub(crate) struct DocumentControl {
    pub shutdown: DocumentShutdown,
    pub frame_demand: FrameDemand,
}

pub(crate) struct HostRuntime {
    pub(crate) test_control: bool,
    pub(crate) clipboard: ClipboardMode,
    pub(crate) file_dialogs: FileDialogMode,
    pub(crate) fullscreen: FullscreenMode,
}

pub(crate) fn desktop_media(theme: Option<Theme>) -> MediaState {
    MediaState {
        color_scheme: if theme == Some(Theme::Dark) {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        primary_pointer: PointerMedia::Fine,
        primary_hover: true,
        any_fine_pointer: true,
        any_hover: true,
        ..MediaState::default()
    }
}

pub(crate) fn open_pack(path: &Path) -> Result<Pack, HostError> {
    let file = File::open(path)
        .map_err(|error| failure("PackError", format_args!("{}: {error}", path.display())))?;
    Pack::open(&file)
        .map_err(|error| failure("PackError", format_args!("{}: {error}", path.display())))
}

pub(crate) fn initialize_v8() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        v8::V8::initialize_platform(v8::new_default_platform(0, false).make_shared());
        v8::V8::initialize();
    });
}

fn document_location(pack: &Pack) -> Result<(Url, bool), HostError> {
    manifest_location(manifest(pack)?.as_ref())
}

pub(crate) fn manifest(pack: &Pack) -> Result<Option<serde_json::Value>, HostError> {
    pack.entry(MANIFEST_PATH)
        .map(|asset| serde_json::from_slice::<serde_json::Value>(asset.bytes))
        .transpose()
        .map_err(|error| failure("PackError", error))
}

fn manifest_location(manifest: Option<&serde_json::Value>) -> Result<(Url, bool), HostError> {
    let path = match manifest {
        None => "/index.html",
        Some(manifest) => manifest
            .get("entry_html")
            .and_then(serde_json::Value::as_str)
            .filter(|path| is_valid_pack_path(path))
            .ok_or_else(|| failure("PackError", "invalid manifest entry_html"))?,
    };
    let origin = manifest.and_then(|manifest| manifest.pointer("/dev/origin"));
    let mut url = if let Some(origin) = origin {
        origin
            .as_str()
            .and_then(|origin| Url::parse(origin).ok())
            .filter(|url| {
                matches!(url.scheme(), "http" | "https")
                    && url.has_host()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
            .ok_or_else(|| failure("PackError", "invalid manifest dev.origin"))?
    } else {
        Url::parse("http://squark.invalid/").unwrap()
    };
    if path != "/index.html" {
        url.path_segments_mut()
            .unwrap()
            .clear()
            .extend(path[1..].split('/'));
    }
    Ok((url, origin.is_some()))
}

pub(crate) fn runner(
    pack: &Arc<Pack>,
    painter: Painter,
    channels: DocumentChannels,
    initial: impl Into<InitialViewport>,
    test_control: bool,
    control: DocumentControl,
    clock: impl Fn() -> Instant + 'static,
) -> Result<DocumentRunner<'_>, HostError> {
    let DocumentControl {
        shutdown,
        frame_demand,
    } = control;
    let DocumentChannels {
        resources,
        host_requests,
        canvas,
    } = channels;
    let InitialViewport {
        tick: initial_tick,
        media,
    } = initial.into();
    let packed = pack
        .entry(DOCUMENT_PATH)
        .ok_or_else(|| failure("PackError", "missing document.bin"))?;
    let (url, dev) = document_location(pack)?;
    let registry = BlobRegistry::default();
    let engine_registry = registry.clone();
    let engine = StaticDocument::from_pack(pack, url.clone(), resources)
        .map_err(|error| failure("DocumentError", error))?
        .with_image_frame_demand(frame_demand.clone())
        .with_blob_resolver(Arc::new(move |url| {
            engine_registry
                .resolve(url)
                .map(|asset| (asset.bytes, asset.mime))
        }));
    let sources_pack = Arc::clone(pack);
    let sources = ScriptSources::new(
        move |path| {
            sources_pack
                .entry(path)
                .map(|asset| (asset.mime.into(), asset.bytes.into()))
        },
        |path| dom_module(path).map(str::to_owned),
    )
    .with_blob_registry(registry.clone())
    .with_host_requests(host_requests)
    .with_shutdown(shutdown);
    let sources = if let Some(canvas) = canvas {
        sources.with_canvas_backend(canvas)
    } else {
        sources
    };
    let sources = if dev {
        sources.with_dev_server()
    } else {
        sources
    };
    DocumentRunner::new_with_media(
        DocumentOptions {
            url: url.into(),
            initial_tick,
            time_origin: initial_tick.vsync,
            test_control,
            frame_demand,
        },
        packed.bytes,
        sources,
        Box::new(engine),
        Box::new(painter),
        clock,
        media,
    )
    .map_err(|error| failure("DocumentError", error))
}

pub(crate) fn run_packed(
    pack: Arc<Pack>,
    (tasks, wake_tasks): (mpsc::Receiver<DocumentTask>, mpsc::Sender<DocumentTask>),
    pipeline: DocumentPipeline,
    runtime: HostRuntime,
    control: DocumentControl,
    clock: impl Fn() -> Instant + 'static,
    mut committed: impl FnMut(),
) -> Result<(), HostError> {
    let DocumentControl {
        shutdown,
        frame_demand,
    } = control;
    let stop_tasks = wake_tasks.clone();
    let Some(initial) = InitialViewport::receive(&tasks) else {
        shutdown.close();
        let _ = stop_tasks.send(DocumentTask::Shutdown);
        return Ok(());
    };
    let startup = squark_gpu::StartupSpan::new("document-bootstrap-and-first-commit");
    let host_service = match HostService::start(
        wake_tasks.clone(),
        runtime.clipboard,
        runtime.file_dialogs,
        runtime.fullscreen,
    ) {
        Ok(service) => service,
        Err(error) => {
            shutdown.close();
            let _ = stop_tasks.send(DocumentTask::Shutdown);
            return Err(error);
        }
    };
    let mut runner = match runner(
        &pack,
        pipeline.painter,
        DocumentChannels {
            resources: pipeline.resources,
            host_requests: host_service.sender(),
            canvas: pipeline.canvas,
        },
        initial,
        runtime.test_control,
        DocumentControl {
            shutdown: shutdown.clone(),
            frame_demand,
        },
        clock,
    ) {
        Ok(runner) => runner,
        Err(_) if shutdown.is_closed() => return Ok(()),
        Err(error) => {
            shutdown.close();
            let _ = stop_tasks.send(DocumentTask::Shutdown);
            return Err(error);
        }
    };
    runner.set_network_wakeup(wake_tasks);
    if let Err(error) = runner.frame(initial.tick) {
        if shutdown.is_closed() {
            // Render can close the document while startup is still in flight.
            return Ok(());
        }
        shutdown.close();
        let _ = stop_tasks.send(DocumentTask::Shutdown);
        return Err(failure("DocumentError", error));
    }
    drop(startup);
    committed();
    let result = runner.run(tasks, |_| committed());
    if result.is_err() {
        shutdown.close();
        let _ = stop_tasks.send(DocumentTask::Shutdown);
    }
    result.map_err(|error| failure("DocumentError", error))
}

#[cfg(test)]
mod tests {
    use squark_compositor::FrameMailbox;
    use squark_paint::Primitive;
    use squark_types::{
        Size,
        render::{Viewport, resource_channel},
    };

    #[test]
    fn resolves_pack_entries_against_the_declared_dev_origin() {
        use serde_json::json;

        let manifest = json!({
            "entry_html": "/nested/index.html",
            "dev": { "origin": "http://127.0.0.1:5173", "entry": "/src/main.tsx" },
        });
        let (url, dev) = super::manifest_location(Some(&manifest)).unwrap();
        assert!(dev);
        assert_eq!(url.as_str(), "http://127.0.0.1:5173/nested/index.html");
        let (url, dev) =
            super::manifest_location(Some(&json!({ "entry_html": "/check.html" }))).unwrap();
        assert!(!dev);
        assert_eq!(url.as_str(), "http://squark.invalid/check.html");
        let invalid = json!({
            "entry_html": "/index.html",
            "dev": { "origin": "http://127.0.0.1:5173/subdir" },
        });
        assert_eq!(
            super::manifest_location(Some(&invalid)).unwrap_err().name,
            "PackError"
        );
    }

    #[test]
    fn serves_the_root_index_at_the_origin_root() {
        use serde_json::json;

        let packed = json!({ "entry_html": "/index.html" });
        let dev = json!({
            "entry_html": "/index.html",
            "dev": { "origin": "http://127.0.0.1:5173" },
        });
        for (manifest, expected) in [
            (None, "http://squark.invalid/"),
            (Some(&packed), "http://squark.invalid/"),
            (Some(&dev), "http://127.0.0.1:5173/"),
        ] {
            assert_eq!(manifest_location(manifest).unwrap().0.as_str(), expected);
        }
    }

    use super::*;

    #[test]
    fn successive_ticks_update_device_metrics_before_the_resize_handler() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-js.sqpak");
        let pack = Arc::new(open_pack(&fixture).unwrap());
        initialize_v8();
        let mailbox = Arc::new(FrameMailbox::default());
        let (resources, _updates) = resource_channel();
        let painter = Painter::new(Arc::clone(&mailbox), resources.clone());
        let first = crate::static_tick(Viewport {
            css: Size {
                width: 40.0,
                height: 30.0,
            },
            device: [60, 45],
            scale: 1.5,
            epoch: 1,
        });
        let (host_tasks, _host_replies) = mpsc::channel();
        let host_service = HostService::start(
            host_tasks,
            ClipboardMode::Memory(crate::services::MemoryClipboard::default()),
            FileDialogMode::Queued(crate::services::FileDialogQueue::default()),
            FullscreenMode::Recorded(Default::default()),
        )
        .unwrap();
        let mut runner = runner(
            &pack,
            painter,
            DocumentChannels {
                resources,
                host_requests: host_service.sender(),
                canvas: None,
            },
            first,
            false,
            DocumentControl {
                shutdown: DocumentShutdown::default(),
                frame_demand: Default::default(),
            },
            move || first.vsync,
        )
        .unwrap();
        runner.frame(first).unwrap();
        let resized = crate::static_tick(Viewport {
            css: Size {
                width: 60.0,
                height: 30.0,
            },
            device: [120, 60],
            scale: 2.0,
            epoch: 2,
        });
        runner.frame(resized).unwrap();
        let frame = mailbox.take_latest().unwrap();
        assert_eq!(frame.viewport, resized.viewport);
        let Primitive::RoundedRect { color, .. } = frame.frame.display_list[0].primitive else {
            panic!("expected a canvas fill");
        };
        assert_eq!(color.rgba, [36.0 / 255.0, 104.0 / 255.0, 32.0 / 255.0, 1.0]);
    }
}
