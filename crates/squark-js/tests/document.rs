// SPDX-License-Identifier: MIT

#[allow(dead_code)]
mod support;
#[path = "support/timeout.rs"]
mod timeout;

#[path = "document/canvas.rs"]
mod canvas_tests;
#[path = "document/css.rs"]
mod css_tests;
#[path = "document/data.rs"]
mod data_tests;
#[path = "document/domparser.rs"]
mod domparser_tests;
#[path = "document/dompurify.rs"]
mod dompurify_tests;
#[path = "document/editing.rs"]
mod editing_tests;
#[path = "document/fonts.rs"]
mod font_tests;
#[path = "document/idle.rs"]
mod idle_tests;
#[path = "document/input.rs"]
mod input_tests;
#[path = "document/local_storage.rs"]
mod local_storage_tests;
#[path = "document/native.rs"]
mod native_tests;
#[path = "document/navigation.rs"]
mod navigation_tests;
#[path = "document/network.rs"]
mod network_tests;
#[path = "document/platform.rs"]
mod platform_tests;
#[path = "document/shadow_dom.rs"]
mod shadow_dom_tests;
#[path = "document/shutdown.rs"]
mod shutdown_tests;
#[path = "document/worker.rs"]
mod worker_tests;

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    fs::File,
    path::Path,
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use squark_assets::Pack;
use squark_compositor::{Compositor, FrameMailbox, HitSnapshots, Painter};
use squark_journal::BatchWriter;
use squark_js::{
    ScriptSources,
    document::{DocumentError, DocumentOptions, DocumentRunner, DocumentTask},
};
use squark_layout::StaticDocument;
use squark_types::{
    NodeId, Point, Rect, ScrollOffset, Size,
    engine::{
        BoxMetrics, CssLength, DecodedBatch, DocumentEngine, EngineError, FaceId, FlushReport,
        FrameInput, IntersectionEntry, IntersectionOptions, LogicalSize, MeasureText, MediaState,
        ParsedName, ParsedNode, Query, QueryValue, ResizeBox, ResizeDelivery, ResizeEntry,
        SelectMode, TextMetrics,
    },
    fragment::{AtomId, FragmentId, FragmentTree, MirrorRead, NodeRead},
    journal::{AtomKind, Opcode, Record},
    pack::{
        BATCH_DIR_ENTRY_SIZE, BatchDirEntry, DOCUMENT_HEADER_SIZE, DOCUMENT_PATH, DocumentHeader,
        QuirksMode, SCRIPT_ENTRY_SIZE, ScriptEntry, ScriptFlags, ScriptKind,
        encode_utf16_string_table,
    },
    render::{FrameId, FramePainter, FrameTick, PaintError, Viewport, resource_channel},
};

const ROOT: NodeId = NodeId::from_raw(1);
const BODY: NodeId = NodeId::from_raw(3);
const URL: &str = "http://squark.invalid/index.html";
const RECT: Rect = Rect::new(10., 20., 200., 100.);

/// esbuild from the workspace's node_modules. `bin/esbuild` is the native
/// binary on Unix but a Node launcher on Windows, where `.bin/esbuild` is a
/// `.cmd` shim that `Command` cannot start.
fn esbuild() -> std::process::Command {
    if cfg!(windows) {
        let mut command = std::process::Command::new("node");
        command.arg(key("node_modules/esbuild/bin/esbuild"));
        command
    } else {
        std::process::Command::new(key("node_modules/.bin/esbuild"))
    }
}

fn key(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn sources(
    pack: impl Fn(&str) -> Option<(String, Vec<u8>)> + Send + Sync + 'static,
) -> ScriptSources {
    let dist = key("packages/@strange-quark/dom/dist");
    assert!(
        dist.join("index.js").is_file(),
        "Run npm run build --workspace @strange-quark/dom before the V8 document tests"
    );
    ScriptSources::new(pack, move |path| {
        std::fs::read_to_string(dist.join(path.strip_prefix('/')?)).ok()
    })
}

fn tick(origin: Instant, sequence: u64, millis: u64) -> FrameTick {
    let vsync = origin + Duration::from_millis(millis);
    FrameTick {
        sequence,
        vsync,
        present_at: vsync + Duration::from_millis(16),
        commit_deadline: vsync + Duration::from_millis(12),
        viewport: Viewport {
            css: Size {
                width: 320.,
                height: 240.,
            },
            device: [320, 240],
            scale: 1.,
            epoch: 1,
        },
    }
}

fn options(origin: Instant, test_control: bool) -> DocumentOptions {
    DocumentOptions {
        url: URL.into(),
        initial_tick: tick(origin, 0, 0),
        time_origin: origin,
        test_control,
        frame_demand: Default::default(),
    }
}

fn fixture(scripts: &[(ScriptKind, &str, &str)]) -> Vec<u8> {
    fixture_with_style(scripts, None)
}

fn fixture_with_style(scripts: &[(ScriptKind, &str, &str)], css: Option<&str>) -> Vec<u8> {
    let mut writer = BatchWriter::new();
    let namespace = writer
        .intern(
            AtomKind::Namespace,
            &"http://www.w3.org/1999/xhtml"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let mut add = |id, tag: &str, parent| {
        let tag = writer
            .intern(AtomKind::Tag, &tag.encode_utf16().collect::<Vec<_>>())
            .unwrap();
        writer
            .record(Opcode::CreateElement, 0, id, tag, namespace)
            .unwrap();
        writer.record(Opcode::Insert, 0, parent, id, 0).unwrap();
    };
    add(2, "html", ROOT.raw());
    add(BODY.raw(), "body", 2);
    add(4, "head", 2);
    let mut entries = Vec::new();
    let mut strings = Vec::new();
    for (index, &(kind, source, base)) in scripts.iter().enumerate() {
        let id = 5 + index as u32;
        add(id, "script", BODY.raw());
        entries.push(ScriptEntry {
            kind,
            flags: ScriptFlags::NONE,
            node_id: NodeId::from_raw(id),
            payload_str: strings.len() as u32,
            base_url_str: strings.len() as u32 + 1,
        });
        strings.push(source.encode_utf16().collect::<Vec<_>>());
        strings.push(base.encode_utf16().collect::<Vec<_>>());
    }
    if let Some(css) = css {
        let id = 5 + scripts.len() as u32;
        add(id, "style", 4);
        writer
            .record(Opcode::CreateText, 0, id + 1, css, 0)
            .unwrap();
        writer.record(Opcode::Insert, 0, id, id + 1, 0).unwrap();
        writer.record(Opcode::SheetInsert, 0, id, 0, css).unwrap();
    }
    let batch = writer.finish().pop().unwrap();
    let script_strings = encode_utf16_string_table(&strings).unwrap();
    let strings_offset = DOCUMENT_HEADER_SIZE + BATCH_DIR_ENTRY_SIZE;
    let entries_offset = strings_offset + script_strings.len();
    let batch_offset = entries_offset + entries.len() * SCRIPT_ENTRY_SIZE;
    let header = DocumentHeader {
        quirks_mode: QuirksMode::Standards,
        document_node_id: ROOT,
        batch_count: 1,
        batch_dir_offset: DOCUMENT_HEADER_SIZE as u64,
        script_strings_offset: strings_offset as u64,
        script_strings_len: script_strings.len() as u64,
        script_count: entries.len() as u32,
        script_table_offset: entries_offset as u64,
    };
    let mut document = header.encode().to_vec();
    document.extend(
        BatchDirEntry {
            offset: batch_offset as u64,
            len: batch.len() as u64,
        }
        .encode(),
    );
    document.extend(script_strings);
    document.extend(entries.into_iter().flat_map(|entry| entry.encode()));
    document.extend(batch);
    document
}

type Trace = Rc<RefCell<Vec<String>>>;

struct FontRecord {
    id: u32,
    flags: u8,
    family: String,
    descriptor: serde_json::Value,
    bytes: Option<Vec<u8>>,
}

#[derive(Default)]
struct JournalLog {
    atoms: BTreeMap<u32, String>,
    ids: BTreeMap<String, NodeId>,
    records: Vec<Record>,
    sheets: BTreeMap<u32, Vec<String>>,
    canvases: Vec<(NodeId, Vec<u8>)>,
    editing: Vec<(Record, Option<Vec<u16>>)>,
    fonts: Vec<FontRecord>,
}

impl JournalLog {
    fn record(&mut self, batch: DecodedBatch<'_>) {
        for record in batch.records {
            match record.op {
                Opcode::DefineAtom => {
                    self.atoms.insert(
                        record.a,
                        String::from_utf16_lossy(batch.strings[record.b as usize]),
                    );
                }
                Opcode::SetAttr if self.atoms.get(&record.b).is_some_and(|name| name == "id") => {
                    let id = String::from_utf16_lossy(batch.strings[record.c as usize]);
                    self.ids.insert(id, NodeId::from_raw(record.a));
                }
                Opcode::CanvasSubmit => {
                    let bytes = batch
                        .staged_bytes
                        .iter()
                        .find(|(id, _)| *id == record.b)
                        .unwrap()
                        .1;
                    assert_eq!(bytes.len(), record.c as usize);
                    self.canvases
                        .push((NodeId::from_raw(record.a), bytes.to_vec()));
                }
                Opcode::SetSelection => self.editing.push((*record, None)),
                Opcode::SetValue | Opcode::SetPreedit => self
                    .editing
                    .push((*record, Some(batch.strings[record.b as usize].to_vec()))),
                Opcode::FontFace => {
                    let family = String::from_utf16_lossy(batch.strings[record.b as usize]);
                    let json = String::from_utf16_lossy(batch.strings[record.c as usize]);
                    let descriptor: serde_json::Value = serde_json::from_str(&json).unwrap();
                    let bytes = descriptor["source"]["bufferId"].as_u64().and_then(|id| {
                        batch
                            .staged_bytes
                            .iter()
                            .find(|(staged, _)| u64::from(*staged) == id)
                            .map(|(_, data)| data.to_vec())
                    });
                    self.fonts.push(FontRecord {
                        id: record.a,
                        flags: record.flags,
                        family,
                        descriptor,
                        bytes,
                    });
                }
                Opcode::SheetInsert => {
                    self.sheets.entry(record.a).or_default().insert(
                        record.b as usize,
                        String::from_utf16_lossy(batch.strings[record.c as usize]),
                    );
                }
                Opcode::SheetDelete => {
                    self.sheets
                        .get_mut(&record.a)
                        .unwrap()
                        .remove(record.b as usize);
                }
                _ => {}
            }
        }
        self.records.extend(batch.records);
    }
}

#[derive(Default)]
struct CssDouble {
    media: Option<(Viewport, MediaState)>,
    animations: bool,
    registrations: Vec<String>,
    resize: VecDeque<ResizeDelivery>,
    intersections: Vec<IntersectionEntry>,
}

struct Engine {
    trace: Trace,
    snapshot: FragmentTree,
    journal: Rc<RefCell<JournalLog>>,
    css: Rc<RefCell<CssDouble>>,
    scroll_reads: Rc<RefCell<Vec<Vec<ScrollOffset>>>>,
    before_query: Option<Box<dyn FnOnce() -> Result<(), EngineError>>>,
}

impl Engine {
    fn new(trace: Trace) -> Self {
        Self {
            trace,
            journal: Default::default(),
            css: Default::default(),
            scroll_reads: Default::default(),
            before_query: None,
            snapshot: FragmentTree {
                root: FragmentId(0),
                ime_area: None,
                nodes: BTreeMap::new(),
            },
        }
    }
    fn trace(&self, value: impl Into<String>) {
        self.trace.borrow_mut().push(value.into());
    }
}

impl MirrorRead for Engine {
    fn document(&self) -> NodeId {
        ROOT
    }
    fn node(&self, _: NodeId) -> Option<NodeRead<'_>> {
        None
    }
    fn atom(&self, _: AtomId) -> Option<&[u16]> {
        None
    }
}

impl DocumentEngine for Engine {
    fn flush(&mut self, batch: DecodedBatch<'_>) -> Result<FlushReport, EngineError> {
        self.journal.borrow_mut().record(batch);
        self.trace("flush");
        for text in batch.strings {
            self.trace(String::from_utf16_lossy(text));
        }
        Ok(FlushReport {
            applied: batch.records.len() as u32,
            released: Vec::new(),
        })
    }
    fn update_layout(&mut self, input: &FrameInput<'_>) -> Result<(), EngineError> {
        self.trace(format!("layout:{}", input.tick.sequence));
        if !input.scroll.offsets.is_empty() {
            self.scroll_reads
                .borrow_mut()
                .push(input.scroll.offsets.to_vec());
        }
        Ok(())
    }
    fn resize_entries(&mut self, deeper_than: u32) -> ResizeDelivery {
        self.trace(if deeper_than == 0 {
            "resize".into()
        } else {
            format!("resize:{deeper_than}")
        });
        self.css.borrow_mut().resize.pop_front().unwrap_or_default()
    }
    fn intersection_entries(&mut self, _: Instant) -> Vec<IntersectionEntry> {
        self.trace("intersection");
        std::mem::take(&mut self.css.borrow_mut().intersections)
    }
    fn snapshot(&self) -> (&FragmentTree, &dyn MirrorRead) {
        (&self.snapshot, self)
    }
    fn query(
        &mut self,
        input: &FrameInput<'_>,
        query: Query<'_>,
    ) -> Result<QueryValue, EngineError> {
        self.trace(format!("query:{}", input.tick.sequence));
        if let Some(before_query) = self.before_query.take() {
            before_query()?;
        }
        if !input.scroll.offsets.is_empty() {
            self.scroll_reads
                .borrow_mut()
                .push(input.scroll.offsets.to_vec());
        }
        match query {
            Query::Selector { selector: "!", .. } => Err(EngineError::InvalidSelector),
            Query::Selector {
                selector: "body",
                mode: SelectMode::All,
                ..
            } => Ok(QueryValue::Nodes(vec![BODY])),
            Query::Selector {
                selector: "body",
                mode: SelectMode::Match,
                ..
            } => Ok(QueryValue::Bool(true)),
            Query::Selector {
                mode: SelectMode::All,
                ..
            } => Ok(QueryValue::Nodes(Vec::new())),
            Query::Selector { selector, .. } => {
                Ok(QueryValue::Node((selector == "body").then_some(BODY)))
            }
            Query::ComputedStyle { property, .. } => match property {
                "display" => Ok(QueryValue::String("block".into())),
                "visibility" => Ok(QueryValue::String("visible".into())),
                _ => Ok(QueryValue::String(String::new())),
            },
            Query::SheetRules(id) => Ok(QueryValue::Strings(
                self.journal
                    .borrow()
                    .sheets
                    .get(&id.raw())
                    .into_iter()
                    .flatten()
                    .flat_map(|rule| rule.lines())
                    .map(str::to_owned)
                    .collect(),
            )),
            Query::BoundingRect(BODY) => Ok(QueryValue::Rect(RECT)),
            Query::ClientRects(BODY) => Ok(QueryValue::Rects(vec![RECT])),
            Query::BoxMetrics(BODY) => Ok(QueryValue::BoxMetrics(BoxMetrics {
                offset_width: RECT.size.width as i32,
                ..Default::default()
            })),
            _ => Err(EngineError::Unsupported("fixture query")),
        }
    }
    fn parse_fragment(
        &self,
        context: NodeId,
        html: &[u16],
    ) -> Result<Vec<ParsedNode>, EngineError> {
        assert_eq!(context, BODY);
        self.trace("parse");
        Ok(vec![ParsedNode::Element {
            name: ParsedName {
                namespace: "http://www.w3.org/1999/xhtml".into(),
                local: "strong".into(),
                prefix: None,
            },
            attrs: Vec::new(),
            children: vec![ParsedNode::Text(html.to_owned())],
        }])
    }
    fn parse_color(&self, canvas: NodeId, css: &[u16]) -> Option<[u8; 4]> {
        assert!(!canvas.is_none());
        (css == "currentColor".encode_utf16().collect::<Vec<_>>()).then_some([10, 20, 30, 255])
    }

    fn parse_canvas_font(&self, _: NodeId, css: &[u16]) -> Option<Vec<u16>> {
        match String::from_utf16_lossy(css).as_str() {
            "12px serif" | " 12PX    SERIF " => Some("12px serif".encode_utf16().collect()),
            _ => None,
        }
    }

    fn validate_filter(&self, css: &[u16]) -> bool {
        css == "invert(93%) hue-rotate(180deg)"
            .encode_utf16()
            .collect::<Vec<_>>()
    }

    fn measure_text(&mut self, request: MeasureText<'_>) -> Result<TextMetrics, EngineError> {
        assert_eq!(request.font_css, "12px serif");
        assert_eq!(request.text, [0xd800, b'T' as u16]);
        self.trace("measure");
        Ok(TextMetrics {
            width: 14.,
            actual_left: 2.,
            ..Default::default()
        })
    }
    fn css_supports(&self, condition: &[u16]) -> bool {
        matches!(
            String::from_utf16_lossy(condition).as_str(),
            "(display: grid)" | "not (display: impossible)" | "selector(:has(*))"
        )
    }

    fn parse_rule(&self, text: &[u16]) -> Option<Vec<u16>> {
        // The adapter tests forwarding and uses predetermined serializations.
        let rule = String::from_utf16_lossy(text);
        if rule == "broken" {
            return None;
        }
        let rule = match rule.as_str() {
            "em{opacity:0}" => "em { opacity: 0; }",
            _ => rule.as_str(),
        };
        Some(rule.encode_utf16().collect())
    }

    fn set_media_state(&mut self, viewport: &Viewport, media: &MediaState) {
        self.css.borrow_mut().media = Some((*viewport, *media));
    }

    fn evaluate_media(&self, query: &[u16]) -> (Vec<u16>, bool) {
        let (viewport, state) = self.css.borrow().media.unwrap();
        let query = String::from_utf16_lossy(query);
        let (serialized, matched) = match query.as_str() {
            "(min-width:400px)" => ("(min-width: 400px)", viewport.css.width >= 400.0),
            "(prefers-color-scheme: dark)" => (
                "(prefers-color-scheme: dark)",
                state.color_scheme == squark_types::engine::ColorScheme::Dark,
            ),
            _ => ("not all", false),
        };
        (serialized.encode_utf16().collect(), matched)
    }

    fn has_running_animations(&self) -> bool {
        self.css.borrow().animations
    }

    fn observe_resize(&mut self, observer: u32, target: NodeId, box_kind: ResizeBox) {
        self.css
            .borrow_mut()
            .registrations
            .push(format!("resize:{observer}:{}:{box_kind:?}", target.raw()));
    }

    fn unobserve_resize(&mut self, observer: u32, target: NodeId) {
        self.css
            .borrow_mut()
            .registrations
            .push(format!("unresize:{observer}:{}", target.raw()));
    }

    fn disconnect_resize(&mut self, observer: u32) {
        self.css
            .borrow_mut()
            .registrations
            .push(format!("disconnect-resize:{observer}"));
    }

    fn observe_intersection(
        &mut self,
        observer: u32,
        target: NodeId,
        options: IntersectionOptions,
    ) {
        assert_eq!(options.root, Some(ROOT));
        assert_eq!(
            options.root_margin,
            [
                CssLength::Px(4.),
                CssLength::Percent(2.),
                CssLength::Px(4.),
                CssLength::Percent(2.)
            ]
        );
        assert_eq!(options.thresholds, [0., 0.5, 1.]);
        self.css
            .borrow_mut()
            .registrations
            .push(format!("intersection:{observer}:{}", target.raw()));
    }

    fn unobserve_intersection(&mut self, observer: u32, target: NodeId) {
        self.css
            .borrow_mut()
            .registrations
            .push(format!("unintersection:{observer}:{}", target.raw()));
    }

    fn disconnect_intersection(&mut self, observer: u32) {
        self.css
            .borrow_mut()
            .registrations
            .push(format!("disconnect-intersection:{observer}"));
    }

    fn font_load(&mut self, face: FaceId) -> Result<(), EngineError> {
        self.trace(format!("font load:{}", face.0));
        Ok(())
    }

    fn match_font_faces(&self, font: &[u16], text: &[u16]) -> Result<Vec<FaceId>, EngineError> {
        if font == "16px UI".encode_utf16().collect::<Vec<_>>() {
            let face = if text == [0xd83d, 0xdca1] { 2 } else { 1 };
            Ok(vec![FaceId(face)])
        } else {
            Err(EngineError::Unsupported("fixture font shorthand"))
        }
    }
}

struct Paint(Trace);

impl FramePainter for Paint {
    fn commit(
        &mut self,
        tick: &FrameTick,
        _: &FragmentTree,
        _: &dyn MirrorRead,
    ) -> Result<FrameId, PaintError> {
        self.0.borrow_mut().push(format!("paint:{}", tick.sequence));
        Ok(FrameId(tick.sequence))
    }
    fn hit_test(&self, _: &FragmentTree, _: &dyn MirrorRead, point: Point) -> Option<NodeId> {
        self.0.borrow_mut().push("hit".into());
        RECT.contains(point).then_some(BODY)
    }
}

#[test]
fn packed_module_cache_reopens_and_recovers_from_changed_or_corrupt_data() {
    support::initialize_v8();
    let root = key(&format!("target/module-cache-{}", std::process::id()));
    let packed = fixture(&[(ScriptKind::ExternalModule, "/entry.js", "/entry.js")]);
    let run = |value: u8| {
        let source = format!(
            "/*{}*/\n globalThis.loaded = [{value}, import.meta.url];",
            "x".repeat(65536)
        );
        let trace = Trace::default();
        let origin = Instant::now();
        let mut runner = DocumentRunner::new(
            options(origin, true),
            &packed,
            sources(move |path| {
                (path == "/entry.js")
                    .then(|| ("text/javascript".into(), source.as_bytes().to_vec()))
            })
            .with_storage_root(root.clone()),
            Box::new(Engine::new(trace.clone())),
            Box::new(Paint(trace)),
            Instant::now,
        )
        .unwrap();
        assert_eq!(
            runner.eval("loaded").unwrap(),
            format!(r#"[{value},"http://squark.invalid/entry.js"]"#)
        );
        let (send, receive) = mpsc::channel();
        send.send(tick(origin, 1, 16).into()).unwrap();
        drop(send);
        runner.run(receive, |_| {}).unwrap();
    };
    let wait_for_cache = |count: usize| {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let files: Vec<_> = std::fs::read_dir(&root)
                .into_iter()
                .flatten()
                .flatten()
                .flat_map(|version| std::fs::read_dir(version.path()).into_iter().flatten())
                .flatten()
                .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
                .map(|entry| entry.path())
                .collect();
            if files.len() == count && files.iter().all(|path| path.metadata().unwrap().len() > 40)
            {
                return files;
            }
            assert!(
                Instant::now() < deadline,
                "packed module cache did not publish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    run(1);
    let file = wait_for_cache(1).pop().unwrap();
    run(1);
    std::fs::write(&file, b"corrupt cache entry").unwrap();
    run(1);
    wait_for_cache(1);
    run(2);
    wait_for_cache(2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scripts_and_frame_run_on_the_same_realm_with_engine_bindings() {
    use ScriptKind::*;
    support::initialize_v8();
    let packed = fixture(&[
        (
            InlineClassic,
            r#"
          globalThis.order = [document.readyState];
          document.getElementsByTagName("script")[4].addEventListener("error", () => order.push("error"));
          addEventListener("load", () => order.push("load:" + document.readyState));
          addEventListener("resize", () => order.push("resize:" + innerWidth));
        "#,
            "/index.html",
        ),
        (ExternalClassic, "/src/classic.js", "/src/classic.js"),
        (
            InlineModule,
            r#"
          import { dep } from "./dep.js";
          const same = await import("./dep.js");
          const lazy = await import("./lazy.js");
          order.push(import.meta.url, same.dep === dep, lazy.value);
          await new Promise(resolve => setTimeout(resolve, 5));
          order.push("awaited");
        "#,
            "/src/index.html",
        ),
        (ExternalModule, "/src/dep.js", "/src/dep.js"),
        (ExternalModule, "/src/missing.js", "/src/missing.js"),
        (
            InlineClassic,
            r#"
          order.push("last");
          requestAnimationFrame(time => {
            order.push("raf:" + time);
            Promise.resolve().then(() => document.body.setAttribute("data-frame", "paint-visible"));
          });
        "#,
            "/index.html",
        ),
    ]);
    let origin = Instant::now();
    let clock = Rc::new(Cell::new(origin));
    let trace = Trace::default();
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &packed,
        sources(|path| {
            let code = match path {
                "/src/classic.js" => {
                    "order.push('external'); queueMicrotask(() => order.push('checkpoint'));"
                }
                "/src/dep.js" => "order.push('dep'); export const dep = {};",
                "/src/lazy.js" => {
                    "import { dep } from './dep.js'; export const value = dep && 'dynamic';"
                }
                _ => return None,
            };
            Some((
                "text/javascript; charset=utf-8".into(),
                code.as_bytes().to_vec(),
            ))
        }),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace.clone())),
        {
            let clock = clock.clone();
            move || clock.get()
        },
    )
    .unwrap();
    assert_eq!(
        runner.eval("order").unwrap(),
        r#"["loading","external","checkpoint","dep","http://squark.invalid/src/index.html",true,"dynamic"]"#
    );
    clock.set(origin + Duration::from_millis(5));
    runner.run_ready().unwrap();
    assert_eq!(
        runner.eval("order.slice(-4)").unwrap(),
        r#"["awaited","error","last","load:complete"]"#
    );
    assert!(
        trace.borrow().is_empty(),
        "startup does not replay the mirror a second time"
    );

    let mut frame = tick(origin, 1, 8);
    frame.viewport.css.width = 400.;
    assert_eq!(runner.frame(frame).unwrap().frame, FrameId(1));
    assert_eq!(
        runner.eval("order.slice(-2)").unwrap(),
        r#"["resize:400","raf:8"]"#
    );
    let work = trace.borrow();
    assert_eq!(
        &work[work.len() - 5..],
        [
            "paint-visible",
            "layout:1",
            "resize",
            "intersection",
            "paint:1"
        ]
    );
    drop(work);

    runner
        .eval(r#"document.body.setAttribute("id", "force-layout");"#)
        .unwrap();
    assert_eq!(runner.query_rect("body").unwrap(), Some(RECT));
    assert_eq!(runner.query_rect(".missing").unwrap(), None);
    let work = trace.borrow();
    assert!(
        work.windows(3)
            .any(|calls| calls == ["force-layout", "query:1", "query:1"])
    );
    drop(work);

    assert_eq!(runner.eval(r#"
      (() => {
        document.body.innerHTML = "\uD800T";
        const element = document.querySelector("body");
        const metrics = __squark.measureText("12px serif", element.textContent);
        let syntax = false;
        try { document.querySelector("!"); } catch (error) { syntax = error instanceof DOMException && error.name === "SyntaxError"; }
        return [element === document.body, document.querySelectorAll("body").item(0) === element,
          element.matches("body"), element.closest("body") === element,
          element.firstChild.localName, element.firstChild.textContent.charCodeAt(0),
          element.getBoundingClientRect().bottom, element.getClientRects().item(0).right,
          document.elementFromPoint(13, 23) === element, element.offsetWidth,
          syntax, metrics.width, metrics.actualBoundingBoxLeft];
      })()
    "#).unwrap(), r#"[true,true,true,true,"strong",55296,120,210,true,200,true,14,2]"#);
    assert!(
        runner
            .eval("__squark.flush(new ArrayBuffer(16), 16)")
            .is_err()
    );
    runner
        .eval("document.body.setAttribute('data-valid', 'valid batch after invalid flush')")
        .unwrap();
    runner.frame(tick(origin, 2, 16)).unwrap();
    assert!(
        trace
            .borrow()
            .iter()
            .any(|event| event == "valid batch after invalid flush")
    );
}

#[test]
fn channel_coalesces_ticks_and_dispatches_test_control() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let runner = DocumentRunner::new(
        options(origin, true),
        &fixture(&[]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace.clone())),
        Instant::now,
    )
    .unwrap();
    let (send, receive) = mpsc::channel();
    let (eval_tx, eval_rx) = mpsc::channel();
    let (rect_tx, rect_rx) = mpsc::channel();
    send.send(tick(origin, 1, 1).into()).unwrap();
    send.send(tick(origin, 2, 2).into()).unwrap();
    send.send(DocumentTask::Eval {
        script: "document.readyState".into(),
        reply: eval_tx,
    })
    .unwrap();
    send.send(DocumentTask::QueryRect {
        selector: "body".into(),
        reply: rect_tx,
    })
    .unwrap();
    let mut frames = Vec::new();
    runner
        .run(receive, |result| {
            frames.push(result.frame);
            send.send(DocumentTask::Shutdown).unwrap();
        })
        .unwrap();
    assert_eq!(frames, [FrameId(2)]);
    assert_eq!(eval_rx.try_recv().unwrap().unwrap(), "\"complete\"");
    assert_eq!(rect_rx.try_recv().unwrap().unwrap(), Some(RECT));

    let mut disabled = DocumentRunner::new(
        options(origin, false),
        &fixture(&[]),
        sources(|_| None),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap();
    assert!(matches!(
        disabled.eval("42"),
        Err(DocumentError::TestControlDisabled)
    ));
    assert!(matches!(
        disabled.query_rect("body"),
        Err(DocumentError::TestControlDisabled)
    ));
}

#[test]
fn startup_module_exceptions_reach_window_and_console() {
    support::initialize_v8();
    let origin = Instant::now();
    let trace = Trace::default();
    let packed = fixture(&[
        (
            ScriptKind::InlineClassic,
            r#"
            globalThis.reports = [];
            console.error = error => reports.push(["console", error.message]);
            onerror = function(message, source, line, column, error) {
              reports.push(["handler", message, source, line > 0, column > 0, this === window, error.message]);
              return error.message === "handled";
            };
            addEventListener("error", event => {
              reports.push(["event", event.message, event.filename, event.isTrusted, event.defaultPrevented]);
            });
            "#,
            "/index.html",
        ),
        (ScriptKind::ExternalModule, "/module.js", "/module.js"),
        (
            ScriptKind::InlineModule,
            "throw new Error('handled');",
            "/index.html",
        ),
    ]);
    let mut runner = DocumentRunner::new(
        options(origin, true),
        &packed,
        sources(|path| {
            (path == "/module.js").then(|| {
                (
                    "text/javascript".into(),
                    b"throw new Error('uncaught');".to_vec(),
                )
            })
        }),
        Box::new(Engine::new(trace.clone())),
        Box::new(Paint(trace)),
        Instant::now,
    )
    .unwrap();
    assert_eq!(
        runner.eval("reports").unwrap(),
        concat!(
            r#"[["handler","Uncaught Error: uncaught","http://squark.invalid/module.js",true,true,true,"uncaught"],"#,
            r#"["event","Uncaught Error: uncaught","http://squark.invalid/module.js",true,false],["console","uncaught"],"#,
            r#"["handler","Uncaught Error: handled","http://squark.invalid/index.html",true,true,true,"handled"],"#,
            r#"["event","Uncaught Error: handled","http://squark.invalid/index.html",true,true]]"#
        )
    );
}

#[test]
fn real_pack_engine_and_painter_commit_javascript_mutations() {
    support::initialize_v8();
    let origin = Instant::now();
    let pack = Arc::new(
        Pack::open(&File::open(key("tests/fixtures/pack/basic.appended.bin")).unwrap()).unwrap(),
    );
    let bytes = pack.entry(DOCUMENT_PATH).unwrap().bytes;
    let (resources, receiver) = resource_channel();
    let mailbox = Arc::new(FrameMailbox::default());
    let engine = StaticDocument::from_pack(&pack, URL.parse().unwrap(), resources.clone()).unwrap();
    let painter = Painter::new(mailbox.clone(), resources);
    let mut runner = DocumentRunner::new(
        options(origin, true),
        bytes,
        sources({
            let pack = pack.clone();
            move |path| {
                let asset = pack.entry(path)?;
                Some((asset.mime.into(), asset.bytes.to_vec()))
            }
        }),
        Box::new(engine),
        Box::new(painter),
        Instant::now,
    )
    .unwrap();
    assert_eq!(
        runner
            .eval("[__boot, document.getElementById('app').textContent]")
            .unwrap(),
        r#"[1,"Hello 🌍"]"#
    );
    runner
        .eval(r#"document.getElementById("app").setAttribute("style", "width: 55px");"#)
        .unwrap();
    let tick = tick(origin, 1, 3);
    assert_eq!(runner.frame(tick).unwrap().frame, FrameId(1));
    let mut compositor = Compositor::new(mailbox, Arc::new(HitSnapshots::default()));
    let resource_seq = receiver.try_iter().last().unwrap().seq;
    assert!(
        compositor
            .adopt_latest(tick.viewport, resource_seq)
            .is_some()
    );
    runner
        .eval(
            r##"
        const style = document.createElement("style");
        style.textContent = "#observer-box { position: fixed; top: 0; left: 0; width: 10px; height: 12px; }";
        document.head.append(style);
        globalThis.entries = [];
        const box = document.createElement("div"); box.id = "observer-box";
        document.body.append(box);
        new IntersectionObserver(items => entries.push(...items.map(
          item => [item.isIntersecting, item.intersectionRatio, item.boundingClientRect.width]
        ))).observe(box);
    "##,
        )
        .unwrap();
    runner.frame(self::tick(origin, 2, 16)).unwrap();
    runner.run_ready().unwrap();
    assert_eq!(runner.eval("entries").unwrap(), "[[true,1,10]]");
    assert_eq!(runner.eval(r#"
        [CSS.supports("display","grid"), CSS.supports("display","unimplemented"), CSS.supports("not (display: unimplemented)")]
    "#).unwrap(), "[true,false,true]");
    assert_eq!(runner.eval(r##"
        const target = document.getElementById("observer-box");
        const button = document.createElement("button"); button.textContent = "first";
        const inserted = target.insertAdjacentElement("AFTERBEGIN", button);
        target.insertAdjacentText("beforeend", "<inside>");
        target.insertAdjacentHTML("afterend", "<em id='after'>outside</em>");
        target.insertAdjacentHTML("beforebegin", "<strong id='before'>before</strong>");
        const panel = document.createElement("table"); document.body.append(panel);
        panel.insertAdjacentHTML("beforeend", "<tr><td>cell</td></tr>");
        const detached = document.createElement("div");
        const absent = detached.insertAdjacentElement("afterend", document.createElement("span"));
        const failures = [];
        try { detached.insertAdjacentHTML("beforebegin", "x"); } catch(e) { failures.push(e.name); }
        try { target.insertAdjacentText("inbetween", "x"); } catch(e) { failures.push(e.name); }
        [inserted === button, target.previousElementSibling.id,target.nextElementSibling.id,
          target.innerHTML, panel.innerHTML, absent, failures]
    "##).unwrap(), r#" [true,"before","after","<button>first</button>&lt;inside&gt;","<tbody><tr><td>cell</td></tr></tbody>",null,["NoModificationAllowedError","SyntaxError"]]"#.trim());
    runner.eval(r#"
        globalThis.nativeImages = [];
        const good = new Image(); const missing = new Image(); const dataImage = new Image();
        globalThis.imageReferences = [good, missing, dataImage];
        for (const [kind, image, src] of [
          ["pack", good, "/images/dot.png"], ["missing", missing, "/images/missing.png"],
          ["data", dataImage, "data:image/svg+xml,%3Csvg%20xmlns='http://www.w3.org/2000/svg'%20width='9'%20height='7'%3E%3C/svg%3E"]
        ]) {
          image.addEventListener("load", () => nativeImages.push([kind, image.naturalWidth, image.naturalHeight, image.isConnected]));
          image.addEventListener("error", () => nativeImages.push([kind, "error", image.complete]));
          image.src = src;
          void image.decode().then(() => nativeImages.push(kind + " decoded"), e => nativeImages.push(kind + " " + e.name));
        }
    "#).unwrap();
    runner.frame(self::tick(origin, 3, 32)).unwrap();
    assert_eq!(
        runner
            .eval("nativeImages.filter(e=>Array.isArray(e)).sort()")
            .unwrap(),
        r#"[["data",9,7,false],["missing","error",true],["pack",1,1,false]]"#
    );
    assert_eq!(
        runner
            .eval("nativeImages.filter(e=>typeof e==='string').sort()")
            .unwrap(),
        r#"["data decoded","missing EncodingError","pack decoded"]"#
    );
    runner
        .eval(
            r#"
        globalThis.preview = document.createElement("section");
        globalThis.previewMutations = [];
        new MutationObserver(records => previewMutations.push(...records.map(
          record => [record.type, record.addedNodes.length, record.removedNodes.length]
        ))).observe(preview, {childList: true, subtree: true, attributes: true});
        document.body.append(preview);
    "#,
        )
        .unwrap();
    for index in 0..6 {
        runner.eval(r#"
            preview.innerHTML = '<div><select><optgroup label="some"><option value="a">A</option></optgroup><option value="b" selected>B</option></select><div></div></div>'.repeat(60);
            gc();
        "#).unwrap();
        runner.run_ready().unwrap();
        runner
            .frame(self::tick(origin, index + 4, 45 + index * 16))
            .unwrap();
    }
    assert_eq!(runner.eval(r#"[preview.getElementsByTagName("select")[0].value,
      preview.getElementsByTagName("option").length, previewMutations.length, previewMutations.at(-1)]"#).unwrap(),
      r#"["b",120,6,["childList",60,60]]"#);
    assert_eq!(
        runner
            .eval(
                r#"
        (() => {
          const panel = document.createElement("section");
          const children = panel.childNodes;
          panel.innerHTML = "<div><b>old</b></div>";
          const removed = panel.firstChild;
          const descendant = removed.firstChild;
          const observer = new MutationObserver(() => {});
          observer.observe(panel, {childList: true, subtree: true, attributes: true});
          panel.innerHTML = "<i>new</i><!-- marker -->";
          descendant.setAttribute("class", "detached");
          const [replacement, attribute, ...extra] = observer.takeRecords();
          return [children.length, children[1] === panel.lastChild,
            replacement.target === panel, replacement.addedNodes.length,
            replacement.addedNodes[0] === panel.firstChild, replacement.addedNodes[1].data,
            replacement.removedNodes.length, replacement.removedNodes[0] === removed,
            removed.parentNode, descendant.parentNode === removed,
            replacement.previousSibling, replacement.nextSibling,
            attribute.type, attribute.target === descendant, extra.length];
        })()
    "#,
            )
            .unwrap(),
        r#"[2,true,true,2,true," marker ",1,true,null,true,null,null,"attributes",true,0]"#
    );
    assert_eq!(
        runner
            .eval(
                r##"(() => {
        preview.innerHTML = '<template><section><b id="inert">kept</b></section></template>';
        const template = preview.firstChild;
        const section = template.content.firstChild;
        const bold = section.firstChild;
        const detached = [
          section.parentNode === template.content, bold.parentNode === section,
          bold.ownerDocument === document, preview.querySelector("#inert") === null,
        ];
        preview.innerHTML = "";
        gc();
        preview.append(bold);
        return [detached, section.firstChild === null, preview.querySelector("#inert") === bold, bold.textContent];
    })()"##,
            )
            .unwrap(),
        r#"[[true,true,true,true],true,true,"kept"]"#
    );
    assert_eq!(
        runner
            .eval(
                r#"
        const rendered = document.createElement("div");
        document.body.append(rendered);
        const rules = document.createElement("style");
        rules.textContent = ".inner-hidden {display:none} .inner-upper {text-transform:uppercase} .inner-lines {white-space:pre-line}";
        document.head.append(rules);
        rendered.innerHTML = ' a  <span class=inner-hidden>hidden</span> b <p class=inner-upper>next</p><span class=inner-lines> d\n   e </span>';
        rendered.innerText
    "#,
            )
            .unwrap(),
        r#""a b\n\nNEXT\n\nd\ne""#
    );
}
