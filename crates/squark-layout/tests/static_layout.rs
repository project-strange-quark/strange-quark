// SPDX-License-Identifier: MIT
use std::io::Write;

use squark_assets::Pack;
use squark_journal::BatchWriter;
use squark_layout::LayoutEngine;
use squark_style::{Cascade, screen};
use squark_types::{
    NodeId, Rect, Size,
    fragment::{
        BackgroundImage, DecorationKind, DecorationStyle, Fragment, FragmentContent, FragmentKind,
        FragmentTree, PaintColor,
    },
    journal::{AtomKind, Opcode},
    pack::{
        BatchDirEntry, DOCUMENT_PATH, DocumentHeader, MANIFEST_PATH, PACK_ENTRY_SIZE,
        PACK_HEADER_SIZE, PackEntry, PackHeader, QuirksMode, fnv1a_64,
    },
};
use url::Url;

#[path = "static_layout/canvas_fonts.rs"]
mod canvas_font_tests;
#[path = "static_layout/controls.rs"]
mod control_tests;
#[path = "static_layout/images.rs"]
mod image_tests;
#[path = "static_layout/queries.rs"]
mod query_tests;
#[path = "static_layout/shadow.rs"]
mod shadow_tests;
#[path = "static_layout/tables.rs"]
mod table_tests;

#[cfg(not(target_os = "linux"))]
#[path = "font_warning.rs"]
mod font_warning;

const VIEWPORT: Size = Size {
    width: 640.0,
    height: 480.0,
};
const HTML: u32 = 2;
const BODY: u32 = 5;

#[test]
fn positions_decorations_and_keeps_the_decorating_ancestors_color() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0; font: 20px/32px monospace }
        #outer { color: blue; text-decoration: underline dotted; text-decoration-thickness: 10%;
                 text-underline-offset: 5px }
        #outer::before { content: 'Todos'; text-decoration: overline green }
        #nested { color: red; font-size: 17px; text-decoration: line-through double }
        #atomic { display: inline-block; text-decoration: none }
        #absolute { position: absolute; text-decoration: none }
        #clear { text-decoration: underline }",
    );
    let outer = fixture.element(BODY, "div", &[("id", "outer")]);
    let nested = fixture.element(outer, "span", &[("id", "nested")]);
    fixture.text(nested, "Write reftests");
    let atomic = fixture.element(outer, "span", &[("id", "atomic")]);
    fixture.text(atomic, "independent");
    let absolute = fixture.element(outer, "span", &[("id", "absolute")]);
    fixture.text(absolute, "out of flow");
    let button = fixture.element(BODY, "button", &[("id", "clear")]);
    fixture.text(button, "Clear completed");

    let tree = layout(&fixture.pack());
    let runs = |owner| {
        tree.nodes
            .values()
            .filter(|fragment| fragment.owner.raw() == owner)
            .find_map(|fragment| match &fragment.content {
                FragmentContent::Text(runs) => Some((fragment, &runs[0])),
                _ => None,
            })
            .expect("text fragment")
    };
    let (text, run) = runs(nested + 1);
    let [strike, underline] = run.decoration_spans.as_ref() else {
        panic!("expected both line-through and ancestor underline");
    };
    assert_eq!(strike.kind, DecorationKind::LineThrough);
    assert_eq!(strike.style, DecorationStyle::Double);
    assert_eq!(strike.color, PaintColor::srgb(1.0, 0.0, 0.0, 1.0));
    assert_eq!(strike.thickness, 1.0);
    assert!(strike.y < run.baseline);
    assert_eq!(underline.kind, DecorationKind::Underline);
    assert_eq!(underline.style, DecorationStyle::Dotted);
    assert_eq!(underline.color, PaintColor::srgb(0.0, 0.0, 1.0, 1.0));
    assert_eq!(underline.thickness, 2.0);
    assert_eq!(underline.y, run.baseline + 5.0);
    assert_eq!(
        underline.x,
        [text.border_box.origin.x, text.border_box.max_x()]
    );
    assert!(runs(atomic + 1).1.decoration_spans.is_empty());
    assert!(runs(absolute + 1).1.decoration_spans.is_empty());
    let (_, generated) = runs(outer);
    assert_eq!(generated.decoration_spans[0].kind, DecorationKind::Overline);
    assert_eq!(
        generated.decoration_spans[1].kind,
        DecorationKind::Underline
    );
    let (_, run) = runs(button + 1);
    assert_eq!(run.decoration_spans[0].kind, DecorationKind::Underline);
}

#[test]
fn lays_out_block_flex_grid_and_hoisted_positions() {
    let css = "
        body { margin: 0 }
        #outer { position: relative; width: 300px; padding: 10px }
        #flex { display: flex; width: 220px; gap: 10px }
        #flex > div { flex: 1; height: 20px }
        #grid { display: grid; width: 200px; grid-template-columns: 50px 1fr; gap: 5px }
        #grid > div { height: 30px }
        #abs { position: absolute; top: 8px; right: 12px; width: 30px; height: 10px }
        #transform { transform: translateX(20px); width: 120px; height: 25px }
        #fixed { position: fixed; right: 8px; bottom: 2px; width: 15px; height: 10px }
        #viewport-fixed { position: fixed; left: 25px; top: 30px; width: 20px; height: 15px }
        #calc { width: min(60%, 150px); height: 10px; padding-top: env(safe-area-inset-top, 100px) }";
    let mut fixture = Fixture::new(css);
    let outer = fixture.element(BODY, "div", &[("id", "outer")]);
    let flex = fixture.element(outer, "div", &[("id", "flex")]);
    let first = fixture.element(flex, "div", &[]);
    let second = fixture.element(flex, "div", &[]);
    let grid = fixture.element(outer, "div", &[("id", "grid")]);
    let left = fixture.element(grid, "div", &[]);
    let right = fixture.element(grid, "div", &[]);
    let absolute = fixture.element(left, "div", &[("id", "abs")]);
    let transformed = fixture.element(outer, "div", &[("id", "transform")]);
    let fixed = fixture.element(transformed, "div", &[("id", "fixed")]);
    let viewport_fixed = fixture.element(outer, "div", &[("id", "viewport-fixed")]);
    let calculated = fixture.element(outer, "div", &[("id", "calc")]);

    let tree = layout(&fixture.pack());
    assert_eq!(
        find(&tree, first).border_box,
        Rect::new(10.0, 10.0, 105.0, 20.0)
    );
    assert_eq!(
        find(&tree, second).border_box,
        Rect::new(125.0, 10.0, 105.0, 20.0)
    );
    assert_eq!(
        find(&tree, left).border_box,
        Rect::new(10.0, 30.0, 50.0, 30.0)
    );
    assert_eq!(
        find(&tree, right).border_box,
        Rect::new(65.0, 30.0, 145.0, 30.0)
    );
    assert_eq!(
        find(&tree, absolute).border_box,
        Rect::new(278.0, 8.0, 30.0, 10.0)
    );
    assert_eq!(
        find(&tree, absolute).containing_block,
        Some(find(&tree, outer).id)
    );
    let fixed = find(&tree, fixed);
    assert_eq!(fixed.border_box, Rect::new(107.0, 73.0, 15.0, 10.0));
    assert_eq!(fixed.containing_block, Some(find(&tree, transformed).id));
    assert_eq!(
        find(&tree, viewport_fixed).border_box,
        Rect::new(25.0, 30.0, 20.0, 15.0)
    );
    assert_eq!(
        find(&tree, viewport_fixed).containing_block,
        Some(tree.root)
    );
    assert_eq!(
        find(&tree, calculated).border_box,
        Rect::new(10.0, 85.0, 150.0, 10.0)
    );
}

#[test]
fn captures_sticky_scroll_geometry_and_propagates_body_background() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0; background: linear-gradient(red, blue), #123456 }
        #scroller { overflow: auto; height: 60px; width: 100px }
        #sticky { position: sticky; top: 10%; height: 20px }
        #tall { height: 160px }",
    );
    let scroller = fixture.element(BODY, "div", &[("id", "scroller")]);
    let sticky = fixture.element(scroller, "div", &[("id", "sticky")]);
    fixture.element(scroller, "div", &[("id", "tall")]);

    let tree = layout(&fixture.pack());
    let root = &tree.nodes[&tree.root];
    assert_eq!(
        root.style.art.background_color,
        PaintColor::srgb(18.0 / 255.0, 52.0 / 255.0, 86.0 / 255.0, 1.0)
    );
    assert!(matches!(
        root.style.box_paint.backgrounds[0].image,
        BackgroundImage::Gradient(_)
    ));
    assert!(find(&tree, BODY).style.box_paint.background_propagated);
    assert!(!find(&tree, HTML).style.box_paint.background_propagated);
    let scroll = find(&tree, scroller).scroll.unwrap();
    assert_eq!(scroll.scrollport, Rect::new(0.0, 0.0, 100.0, 60.0));
    assert_eq!(scroll.content, Rect::new(0.0, 0.0, 100.0, 180.0));
    assert!(scroll.scroll_y && scroll.clip_y);
    let constraint = find(&tree, sticky).sticky.unwrap();
    assert_eq!(constraint.containing_rect, scroll.scrollport);
    assert_eq!(constraint.scroll_owner.raw(), scroller);
    assert_eq!(constraint.insets.top, Some(6.0));
    assert_eq!(find(&tree, sticky).border_box.origin.y, 0.0);
}

#[test]
fn keeps_intrinsic_sizes_and_font_instances_in_packed_fragments() {
    let mut fixture = Fixture::new("
        body { margin: 0; font: 16px/24px monospace }
        #png { width: 20px; vertical-align: middle }
        #svg-bg { background: url('data:image/svg+xml,%3Csvg%20xmlns=%22http://www.w3.org/2000/svg%22%20viewBox=%220%200%209%203%22/%3E') }");
    let paragraph = fixture.element(BODY, "div", &[("id", "svg-bg")]);
    fixture.text(paragraph, "A  ");
    let span = fixture.element(paragraph, "span", &[]);
    fixture.text(span, "  B ");
    let image = fixture.element(paragraph, "img", &[
        ("id", "png"),
        ("src", "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aPIcAAAAASUVORK5CYII="),
    ]);
    let pack = fixture.pack();
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        &pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    let mut engine = LayoutEngine::default();
    let tree = engine.layout(&doc, &cascade, VIEWPORT);
    let src = find(&tree, image);
    assert_eq!(
        src.border_box.size,
        Size {
            width: 20.0,
            height: 20.0
        }
    );
    assert!(matches!(
        src.content,
        FragmentContent::Image {
            intrinsic_size: Size {
                width: 1.0,
                height: 1.0
            },
            ..
        }
    ));
    assert!(matches!(
        find(&tree, paragraph).style.box_paint.backgrounds[0].image,
        BackgroundImage::Image(_)
    ));
    let mut glyphs = 0;
    for fragment in tree.nodes.values() {
        if let FragmentContent::Text(runs) = &fragment.content {
            for run in runs {
                let instance = engine.fonts().instance(run.font).unwrap();
                assert_eq!(instance.size, 16.0);
                assert!(!instance.font.data.as_ref().is_empty());
                assert!(
                    run.glyphs
                        .iter()
                        .all(|glyph| (glyph.cluster as usize) < run.source_ranges.len())
                );
                glyphs += run.glyphs.len();
            }
        }
    }
    assert!(glyphs >= 2);
    let repeated = engine.layout(&doc, &cascade, VIEWPORT);
    assert_eq!(find(&tree, image).id, find(&repeated, image).id);
    assert_eq!(tree.root, repeated.root);
}

fn layout(pack: &Pack) -> FragmentTree {
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    LayoutEngine::default().layout(&doc, &cascade, VIEWPORT)
}

fn find(tree: &FragmentTree, owner: u32) -> &Fragment {
    tree.nodes
        .values()
        .find(|item| {
            item.owner.raw() == owner
                && matches!(item.kind, FragmentKind::Box | FragmentKind::Replaced)
        })
        .expect("box for node")
}

struct Fixture {
    journal: BatchWriter,
    ns: u32,
    next: u32,
}

impl Fixture {
    fn new(css: &str) -> Self {
        #[cfg(not(target_os = "linux"))]
        font_warning::warn_if_missing();
        let mut journal = BatchWriter::new();
        let ns = journal
            .intern(
                AtomKind::Namespace,
                &"http://www.w3.org/1999/xhtml"
                    .encode_utf16()
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let mut fixture = Self {
            journal,
            ns,
            next: 1,
        };
        assert_eq!(fixture.element(1, "html", &[]), HTML);
        let style = fixture.element(HTML, "style", &[]);
        fixture.text(style, css);
        assert_eq!(fixture.element(HTML, "body", &[]), BODY);
        fixture
    }

    fn element(&mut self, parent: u32, tag: &str, attrs: &[(&str, &str)]) -> u32 {
        self.element_ns(parent, tag, attrs, self.ns)
    }

    fn element_ns(
        &mut self,
        parent: u32,
        tag: &str,
        attrs: &[(&str, &str)],
        namespace: u32,
    ) -> u32 {
        self.next += 1;
        let id = self.next;
        let name = self
            .journal
            .intern(AtomKind::Tag, &tag.encode_utf16().collect::<Vec<_>>())
            .unwrap();
        self.journal
            .record(Opcode::CreateElement, 0, id, name, namespace)
            .unwrap();
        for &(name, value) in attrs {
            let name = self
                .journal
                .intern(
                    AtomKind::Attribute,
                    &name.encode_utf16().collect::<Vec<_>>(),
                )
                .unwrap();
            self.journal
                .record(Opcode::SetAttr, 0, id, name, value)
                .unwrap();
        }
        self.journal
            .record(Opcode::Insert, 0, parent, id, 0)
            .unwrap();
        id
    }

    fn text(&mut self, parent: u32, value: &str) {
        self.next += 1;
        self.journal
            .record(Opcode::CreateText, 0, self.next, value, 0)
            .unwrap();
        self.journal
            .record(Opcode::Insert, 0, parent, self.next, 0)
            .unwrap();
    }

    fn pack(self) -> Pack {
        self.pack_with_manifest(None)
    }

    fn pack_with_manifest(self, manifest: Option<&[u8]>) -> Pack {
        match manifest {
            Some(manifest) => self.pack_with_assets(&[(MANIFEST_PATH, 4, manifest)]),
            None => self.pack_with_assets(&[]),
        }
    }

    fn pack_with_assets(self, extra: &[(&str, u16, &[u8])]) -> Pack {
        let batches = self.journal.finish();
        assert_eq!(batches.len(), 1);
        let doc_header = DocumentHeader {
            quirks_mode: QuirksMode::Standards,
            document_node_id: NodeId::from_raw(1),
            batch_count: 1,
            batch_dir_offset: 64,
            script_strings_offset: 80,
            script_strings_len: 12,
            script_count: 0,
            script_table_offset: 92,
        };
        let mut document = doc_header.encode().to_vec();
        document.extend(
            BatchDirEntry {
                offset: 92,
                len: batches[0].len() as u64,
            }
            .encode(),
        );
        document.extend([0; 12]);
        document.extend(&batches[0]);
        let mut assets = vec![(DOCUMENT_PATH, 0, document.as_slice())];
        assets.extend_from_slice(extra);
        assets.sort_unstable_by_key(|(path, _, _)| *path);
        let mut strings = Vec::new();
        let mut index = Vec::new();
        let mut data = Vec::new();
        for (path, mime_id, payload) in assets {
            data.resize(data.len().next_multiple_of(8), 0);
            index.push(PackEntry {
                path_hash: fnv1a_64(path.as_bytes()),
                path_off: strings.len() as u32,
                path_len: path.len() as u16,
                mime_id,
                data_off: data.len() as u32,
                data_len: payload.len() as u32,
                mime_off: 0,
                mime_len: 0,
                entry_flags: 0,
            });
            strings.extend_from_slice(path.as_bytes());
            data.extend_from_slice(payload);
        }
        let strings_offset = PACK_HEADER_SIZE + index.len() * PACK_ENTRY_SIZE;
        let data_offset = (strings_offset + strings.len()).next_multiple_of(64) as u32;
        let header = PackHeader {
            entry_count: index.len() as u32,
            index_offset: PACK_HEADER_SIZE as u32,
            strings_offset: strings_offset as u32,
            strings_len: strings.len() as u32,
            data_offset,
        };
        let mut bytes = header.encode().to_vec();
        for entry in index {
            bytes.extend(entry.encode());
        }
        bytes.extend(strings);
        bytes.resize(data_offset as usize, 0);
        bytes.extend(data);
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&bytes).unwrap();
        Pack::open(&file).unwrap()
    }
}

#[test]
fn constructs_generated_fragments_and_promotes_display_contents() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        #contents { display: contents }
        #item::before { content: ''; display: block; width: 13px; height: 7px }
        #item::after { content: 'after'; display: inline-block; width: 24px; height: 18px }
        li { font: 16px/20px sans-serif }",
    );
    let contents = fixture.element(BODY, "div", &[("id", "contents")]);
    let item = fixture.element(contents, "div", &[("id", "item")]);
    fixture.text(item, "text");
    let list = fixture.element(BODY, "ul", &[]);
    let li = fixture.element(list, "li", &[]);
    fixture.text(li, "item");
    let tree = layout(&fixture.pack());
    assert!(
        !tree
            .nodes
            .values()
            .any(|fragment| fragment.owner.raw() == contents)
    );
    assert_eq!(find(&tree, item).parent, Some(find(&tree, BODY).id));
    let generated = |owner, kind| {
        tree.nodes
            .values()
            .find(|fragment| fragment.owner.raw() == owner && fragment.kind == kind)
            .unwrap()
    };
    assert_eq!(
        generated(item, FragmentKind::Before).border_box.size,
        Size {
            width: 13.0,
            height: 7.0
        }
    );
    assert_eq!(
        generated(item, FragmentKind::After).border_box.size,
        Size {
            width: 24.0,
            height: 18.0
        }
    );
    assert!(
        generated(li, FragmentKind::Marker).border_box.origin.x
            < find(&tree, li).border_box.origin.x
    );
}

#[test]
fn reserves_an_ellipsis_for_the_clipped_end_of_a_line() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0; font: 16px/24px monospace }
        div { width: 90px; overflow: hidden; white-space: nowrap; text-overflow: ellipsis }
    ",
    );
    let node = fixture.element(BODY, "div", &[]);
    fixture.text(node, "An overflowing line of text");
    let tree = layout(&fixture.pack());
    let line = &tree.nodes[&find(&tree, node).children[0]];
    let text = &tree.nodes[line.children.last().unwrap()];
    assert_eq!(text.owner.raw(), node);
    assert!(
        matches!(&text.content, FragmentContent::Text(runs) if runs.len() == 1 && runs[0].glyphs.len() == 1)
    );
    assert!((text.border_box.max_x() - 90.0).abs() < 0.01);
}

#[test]
fn gives_absolutely_positioned_inlines_a_static_line_anchor() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0; font: 16px/24px monospace }
        #anchor { position: relative; padding: 12px }
        #label { position: absolute; width: 20px; height: 10px; }",
    );
    let anchor = fixture.element(BODY, "div", &[("id", "anchor")]);
    fixture.text(anchor, "Prefix");
    let label = fixture.element(anchor, "span", &[("id", "label")]);
    fixture.text(anchor, " suffix");
    let tree = layout(&fixture.pack());
    let label = find(&tree, label);
    assert_eq!(label.containing_block, Some(find(&tree, anchor).id));
    assert!(label.border_box.origin.x > 30.0);
    assert_eq!(label.border_box.origin.y, 12.0);
}

#[test]
fn publishes_snapshot_and_exact_resource_keys_before_paint() {
    use std::{collections::HashSet, time::Instant};

    use squark_layout::StaticDocument;
    use squark_types::{
        engine::{DocumentEngine, FrameInput},
        fragment::{BorderStyle, Filter, Transform},
        render::{
            BitmapFormat, FrameTick, GlyphKey, ResourceSeq, ResourceUpdate, Viewport,
            resource_channel,
        },
    };

    const IMAGE: &str = "data:image/svg+xml,%3Csvg%20xmlns=%22http://www.w3.org/2000/svg%22%20width=%228%22%20height=%224%22%3E%3Crect%20width=%228%22%20height=%224%22%20fill=%22red%22/%3E%3C/svg%3E";
    let mut fixture = Fixture::new(&format!(
        "
        body {{ margin: 0; font: 16px/24px monospace }}
        #panel {{ width: 100px; opacity: 0.5; transform: translateX(20%); filter: invert(1);
                  border: 3px solid red; box-shadow: 0 0 2px blue; background: url('{IMAGE}'); }}
    "
    ));
    let panel = fixture.element(
        BODY,
        "div",
        &[("id", "panel"), ("style", "width: 100px; float: left")],
    );
    fixture.text(panel, " A A");
    let image = fixture.element(panel, "img", &[("src", IMAGE)]);
    let pack = fixture.pack();
    let (sender, queue) = resource_channel();
    let url = Url::parse("https://app.test/index.html").unwrap();
    let mut document = StaticDocument::from_pack(&pack, url.clone(), sender.clone()).unwrap();
    let now = Instant::now();
    let input = |sequence, scale| FrameInput {
        tick: FrameTick {
            sequence,
            vsync: now,
            present_at: now,
            commit_deadline: now,
            viewport: Viewport {
                css: VIEWPORT,
                device: [
                    (f64::from(VIEWPORT.width) * scale) as u32,
                    (f64::from(VIEWPORT.height) * scale) as u32,
                ],
                scale,
                epoch: sequence,
            },
        },
        media: Default::default(),
        scroll: Default::default(),
    };
    document.update_layout(&input(1, 1.0)).unwrap();
    let (snapshot, mirror) = document.snapshot();
    let fragment = find(snapshot, panel);
    assert!(mirror.node(fragment.owner).is_some());
    assert_eq!(
        fragment.style.box_paint.border.top.style,
        BorderStyle::Solid
    );
    assert_eq!(fragment.style.art.box_shadows.len(), 1);
    assert_eq!(fragment.style.properties.opacity, 0.5);
    assert_eq!(
        fragment.style.properties.filters.as_ref(),
        &[Filter::Invert(1.0)]
    );
    let [Transform::Matrix(matrix)] = fragment.style.properties.transform.as_ref() else {
        panic!("missing CSS transform");
    };
    assert!((matrix[3][0] - fragment.border_box.size.width * 0.2).abs() < 0.01);
    let FragmentContent::Image { key: image_key, .. } = find(snapshot, image).content else {
        panic!("missing replaced image");
    };
    assert!(
        matches!(fragment.style.box_paint.backgrounds[0].image, BackgroundImage::Image(key) if key == image_key)
    );
    let glyph_keys: HashSet<_> = snapshot
        .nodes
        .values()
        .flat_map(|fragment| {
            let FragmentContent::Text(runs) = &fragment.content else {
                return Vec::new();
            };
            runs.iter()
                .flat_map(|run| {
                    run.glyphs.iter().map(|glyph| {
                        GlyphKey::for_position(run.font, glyph.glyph_id, glyph.position, 1.0)
                    })
                })
                .collect()
        })
        .collect();
    let upload_count = glyph_keys.len() + 1;
    let mut rasterized = HashSet::new();
    let mut has_ink = false;
    for (index, resource) in queue.try_iter().enumerate() {
        assert_eq!(resource.seq, ResourceSeq(index as u64 + 1));
        match resource.update {
            ResourceUpdate::PutGlyph {
                key,
                bitmap,
                bearing,
            } => {
                assert!(rasterized.insert(key));
                assert_eq!(bitmap.format, BitmapFormat::R8Coverage);
                assert_eq!(
                    bitmap.pixels.len(),
                    (bitmap.stride * bitmap.size[1]) as usize
                );
                if bitmap.pixels.iter().any(|&pixel| pixel > 0) {
                    assert!(bearing[1] > 0);
                    has_ink = true;
                }
            }
            ResourceUpdate::PutImage { key, bitmap } => {
                assert_eq!(key, image_key);
                assert_eq!(bitmap.size, [8, 4]);
                assert_eq!(bitmap.pixels.as_ref(), [255, 0, 0, 255].repeat(32));
            }
            _ => panic!("unexpected static resource"),
        }
    }
    assert_eq!(rasterized, glyph_keys);
    assert!(has_ink);
    assert_eq!(sender.watermark(), ResourceSeq(upload_count as u64));
    let warning = document
        .css_warnings()
        .first()
        .expect("ignored inline float");
    assert_eq!(warning.property, "float");
    assert_eq!(warning.value, "left");
    assert_eq!(warning.stylesheet, url);
    assert_eq!(warning.inline_owner, Some(NodeId::from_raw(panel)));
    assert_eq!((warning.line, warning.column), (1, 15));
    assert!(warning.style_attribute);

    document.update_layout(&input(2, 1.0)).unwrap();
    assert!(queue.try_recv().is_err());
    document.update_layout(&input(3, 2.0)).unwrap();
    let resized: Vec<_> = queue.try_iter().collect();
    assert!(!resized.is_empty());
    assert!(resized.iter().all(|entry| matches!(
        entry.update, ResourceUpdate::PutGlyph { key, .. } if key.scale == 2048
    )));
    assert_eq!(resized[0].seq, ResourceSeq(upload_count as u64 + 1));
}

#[test]
fn paints_both_slices_of_a_wrapped_inline_box() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0; font: 16px/32px monospace }
        #container { width: 75px }
        span { background: #ff0; padding: 2px 6px; border: 2px solid black; border-radius: 4px }
    ",
    );
    let container = fixture.element(BODY, "div", &[("id", "container")]);
    let span = fixture.element(container, "span", &[]);
    fixture.text(span, "hello world");
    let tree = layout(&fixture.pack());
    let slices: Vec<_> = tree
        .nodes
        .values()
        .filter(|node| node.kind == FragmentKind::Box && node.owner.raw() == span)
        .collect();
    assert_eq!(slices.len(), 2);
    assert_eq!(
        [slices[0].padding.left, slices[0].padding.right],
        [6.0, 0.0]
    );
    assert_eq!(
        [slices[1].padding.left, slices[1].padding.right],
        [0.0, 6.0]
    );
    assert_eq!([slices[0].border.left, slices[0].border.right], [2.0, 0.0]);
    assert_eq!([slices[1].border.left, slices[1].border.right], [0.0, 2.0]);
    assert_eq!(
        slices[1].border_box.origin.y - slices[0].border_box.origin.y,
        32.0
    );
    assert!(
        slices
            .iter()
            .all(|node| node.style.art.background_color == PaintColor::srgb(1.0, 1.0, 0.0, 1.0))
    );
}

#[test]
fn lays_out_separate_table_borders_with_proportional_columns() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0; font: 20px/24px monospace }
        table { width: 200px; box-sizing: border-box; border-spacing: 4px 6px }
        td { padding: 0 }
    ",
    );
    let table = fixture.element(BODY, "table", &[]);
    let first = fixture.element(table, "tr", &[]);
    let a = fixture.element(first, "td", &[]);
    fixture.text(a, "aa");
    let b = fixture.element(first, "td", &[]);
    fixture.text(b, "bbbbbb");
    let second = fixture.element(table, "tr", &[]);
    let c = fixture.element(second, "td", &[]);
    fixture.text(c, "c");
    let d = fixture.element(second, "td", &[]);
    fixture.text(d, "ddd");

    let tree = layout(&fixture.pack());
    assert_eq!(
        find(&tree, table).border_box,
        Rect::new(0.0, 0.0, 200.0, 66.0)
    );
    assert_eq!(find(&tree, table).padding, Default::default());
    for (id, expected) in [
        (a, Rect::new(4.0, 6.0, 47.0, 24.0)),
        (b, Rect::new(55.0, 6.0, 141.0, 24.0)),
        (c, Rect::new(4.0, 36.0, 47.0, 24.0)),
        (d, Rect::new(55.0, 36.0, 141.0, 24.0)),
    ] {
        let actual = find(&tree, id).border_box;
        assert!((actual.origin.x - expected.origin.x).abs() < 0.01);
        assert!((actual.size.width - expected.size.width).abs() < 0.01);
        assert_eq!(actual.origin.y, expected.origin.y);
        assert_eq!(actual.size.height, expected.size.height);
    }
}

#[test]
fn projects_inherited_and_overridden_image_rendering() {
    use squark_types::fragment::ImageRendering;

    let mut fixture = Fixture::new(
        "
        #outer { image-rendering: pixelated }
        #edges { image-rendering: crisp-edges }
        #auto { image-rendering: auto }
    ",
    );
    let outer = fixture.element(BODY, "div", &[("id", "outer")]);
    let inherited = fixture.element(outer, "img", &[]);
    let crisp = fixture.element(outer, "img", &[("id", "edges")]);
    let auto = fixture.element(outer, "img", &[("id", "auto")]);
    let tree = layout(&fixture.pack());
    assert_eq!(
        find(&tree, inherited).style.image_rendering,
        ImageRendering::Pixelated
    );
    assert_eq!(
        find(&tree, crisp).style.image_rendering,
        ImageRendering::CrispEdges
    );
    assert_eq!(
        find(&tree, auto).style.image_rendering,
        ImageRendering::Auto
    );
}

#[test]
fn projects_inherited_pointer_events_and_cursor_with_an_explicit_override() {
    use squark_types::fragment::{Cursor, PointerEvents};

    let mut fixture = Fixture::new(
        "#overlay { pointer-events: none; cursor: grab }
         #active { pointer-events: auto; cursor: url(foo.cur), crosshair }",
    );
    let overlay = fixture.element(BODY, "div", &[("id", "overlay")]);
    let passive = fixture.element(overlay, "div", &[]);
    let active = fixture.element(overlay, "div", &[("id", "active")]);
    fixture.text(active, "click target");
    let label = fixture.next;
    let tree = layout(&fixture.pack());
    for id in [overlay, passive] {
        let interaction = find(&tree, id).style.interaction;
        assert_eq!(interaction.pointer_events, PointerEvents::None);
        assert_eq!(interaction.cursor, Cursor::Grab);
    }
    let interaction = find(&tree, active).style.interaction;
    assert_eq!(interaction.pointer_events, PointerEvents::Auto);
    assert_eq!(interaction.cursor, Cursor::Crosshair);
    let text = tree
        .nodes
        .values()
        .find(|fragment| fragment.owner.raw() == label && fragment.kind == FragmentKind::Text)
        .unwrap();
    assert_eq!(text.style.interaction, interaction);
}

#[test]
fn preserves_preformatted_breaks_without_an_extra_final_line() {
    let mut fixture = Fixture::new("pre { margin: 0; font: 16px/17px monospace }");
    let mut examples = Vec::new();
    // html5ever has already removed an initial newline from PRE's text.
    for (text, height) in [
        ("one", 17.0),
        ("one\n", 17.0),
        ("one\n\n", 34.0),
        ("\n", 17.0),
    ] {
        let pre = fixture.element(BODY, "pre", &[]);
        fixture.text(pre, text);
        examples.push((pre, height));
    }
    let tree = layout(&fixture.pack());
    for (id, height) in examples {
        assert_eq!(find(&tree, id).border_box.size.height, height);
    }
}

#[test]
fn hides_noscript_in_the_scripting_enabled_document() {
    let mut fixture = Fixture::new("body { margin: 0 } noscript { display: block !important }");
    let fallback = fixture.element(BODY, "noscript", &[]);
    fixture.text(fallback, "Enable JavaScript");
    let shell = fixture.element(BODY, "div", &[]);
    fixture.text(shell, "Shell");
    let tree = layout(&fixture.pack());
    assert!(!tree.nodes.values().any(|f| f.owner.raw() == fallback));
    assert_eq!(find(&tree, shell).border_box.origin.y, 0.0);
}

#[test]
fn sizes_viewbox_only_inline_svg_and_resolves_current_color() {
    use squark_types::fragment::SvgPaint;

    let mut fixture = Fixture::new(
        "body { margin: 0 }
         .icon { width: 24px; color: blue }
         .welcome { font-size: 16px; width: 1em; height: 1em; display: inline-block }
         .em-icon { width: 1em }
         .shrink { display: inline-block }
         svg { vertical-align: top }",
    );
    let ns = fixture
        .journal
        .intern(
            AtomKind::Namespace,
            &"http://www.w3.org/2000/svg"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let icon = fixture.element(BODY, "div", &[("class", "icon")]);
    let svg = fixture.element_ns(icon, "svg", &[("viewBox", "0 0 48 24")], ns);
    fixture.element_ns(
        svg,
        "rect",
        &[("width", "48"), ("height", "24"), ("fill", "currentColor")],
        ns,
    );
    let container = fixture.element(BODY, "div", &[("class", "shrink")]);
    let percent = fixture.element_ns(
        icon,
        "svg",
        &[("viewBox", "0 0 48 24"), ("width", "50%")],
        ns,
    );
    let indefinite = fixture.element_ns(container, "svg", &[("viewBox", "0 0 48 24")], ns);
    let welcome = fixture.element(BODY, "span", &[("class", "welcome")]);
    let welcome_icon = fixture.element_ns(welcome, "svg", &[("viewBox", "0 0 20 20")], ns);
    let em_icon = fixture.element_ns(
        welcome,
        "svg",
        &[("viewBox", "0 0 32 16"), ("class", "em-icon")],
        ns,
    );
    let tree = layout(&fixture.pack());
    let outer = find(&tree, svg);
    assert_eq!(
        outer.border_box.size,
        Size {
            width: 24.0,
            height: 12.0
        }
    );
    let FragmentContent::Svg(geometry) = &outer.content else {
        panic!("expected SVG geometry");
    };
    assert_eq!(
        geometry.paths[0].fill,
        Some(SvgPaint::Color(PaintColor::srgb(0.0, 0.0, 1.0, 1.0)))
    );
    assert_eq!(
        find(&tree, percent).border_box.size,
        Size {
            width: 12.0,
            height: 6.0
        }
    );
    assert_eq!(find(&tree, indefinite).border_box.size, Size::default());
    assert_eq!(
        find(&tree, welcome_icon).border_box.size,
        Size {
            width: 16.0,
            height: 16.0
        }
    );
    assert_eq!(
        find(&tree, em_icon).border_box.size,
        Size {
            width: 16.0,
            height: 8.0
        }
    );
}

#[test]
fn resolves_svg_paints_from_presentation_attributes_and_css() {
    use squark_types::fragment::SvgPaint;

    let mut fixture = Fixture::new(
        ".popup { --surface: white }
         .accent { fill: var(--surface) !important; color: blue }
         .inherit { stroke: currentColor }",
    );
    let ns = fixture
        .journal
        .intern(
            AtomKind::Namespace,
            &"http://www.w3.org/2000/svg"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let popup = fixture.element(BODY, "div", &[("class", "popup")]);
    let svg = fixture.element_ns(
        popup,
        "svg",
        &[
            ("width", "20"),
            ("height", "10"),
            ("fill", "red"),
            ("class", "accent"),
        ],
        ns,
    );
    fixture.element_ns(
        svg,
        "path",
        &[("d", "M0 0H10L5 10Z"), ("class", "inherit")],
        ns,
    );
    fixture.element_ns(
        svg,
        "path",
        &[
            ("d", "M10 0H20L15 10Z"),
            ("fill", "red"),
            ("style", "fill: var(--surface) !important"),
        ],
        ns,
    );
    let tree = layout(&fixture.pack());
    let FragmentContent::Svg(geometry) = &find(&tree, svg).content else {
        panic!("expected SVG geometry");
    };
    let white = Some(SvgPaint::Color(PaintColor::srgb(1.0, 1.0, 1.0, 1.0)));
    assert_eq!(geometry.paths[0].fill, white);
    assert_eq!(geometry.paths[1].fill, white);
    assert_eq!(
        geometry.paths[0].stroke.as_ref().unwrap().paint,
        SvgPaint::Color(PaintColor::srgb(0.0, 0.0, 1.0, 1.0)),
    );
}

#[test]
fn leaves_the_range_box_at_the_authored_track_height() {
    let mut fixture = Fixture::new(
        "body { margin: 0 }
         div { width: 176px }
         input { appearance: none; width: 100%; height: 4px; vertical-align: top }",
    );
    let container = fixture.element(BODY, "div", &[]);
    let range = fixture.element(container, "input", &[("type", "range"), ("value", "100")]);
    let tree = layout(&fixture.pack());
    assert_eq!(
        find(&tree, range).border_box,
        Rect::new(2.0, 2.0, 176.0, 4.0)
    );
}

/// Whether this is Windows without Liberation Mono, the test font the Linux
/// and macOS CI jobs install, in the system or the per-user font folder.
fn liberation_mono_missing_on_windows() -> bool {
    if !cfg!(target_os = "windows") {
        return false;
    }
    let dirs = [
        std::env::var_os("WINDIR").map(|dir| std::path::PathBuf::from(dir).join("Fonts")),
        std::env::var_os("LOCALAPPDATA")
            .map(|dir| std::path::PathBuf::from(dir).join("Microsoft/Windows/Fonts")),
    ];
    !dirs.into_iter().flatten().any(|dir| {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .to_ascii_lowercase()
                    .starts_with("liberationmono-regular")
            })
        })
    })
}

#[test]
fn fits_positioned_flex_hints_to_min_content_when_the_anchor_is_narrow() {
    let mut fixture = Fixture::new(
        "body { margin: 0; font: 16px/20px 'Liberation Mono', monospace }
         .anchor { position: relative; width: 36px; height: 40px; margin: 120px 0 0 200px }
         .hint { position: absolute; right: 0; bottom: 100%; display: flex }
         .min { width: min-content }
         svg { width: 85px; height: 71px; margin-top: 8px }",
    );
    let ns = fixture
        .journal
        .intern(
            AtomKind::Namespace,
            &"http://www.w3.org/2000/svg"
                .encode_utf16()
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let anchor = fixture.element(BODY, "div", &[("class", "anchor")]);
    let mut hints = Vec::new();
    for (class, overflow) in [
        ("hint", ""),
        ("hint min", ""),
        ("hint", "overflow: visible"),
    ] {
        let hint = fixture.element(anchor, "div", &[("class", class)]);
        let label = fixture.element(hint, "div", &[]);
        fixture.text(label, "Shortcuts & help");
        let svg = fixture.element_ns(
            hint,
            "svg",
            &[("viewBox", "0 0 85 71"), ("style", overflow)],
            ns,
        );
        hints.push([hint, label, svg]);
    }
    let tree = layout(&fixture.pack());
    let actual = find(&tree, hints[0][0]).border_box;
    let reference = find(&tree, hints[1][0]).border_box;
    assert_eq!(actual, reference);
    // Without Liberation Mono installed, Windows falls back to Consolas for
    // `monospace`, as pinned Chromium 159.0.8154.6 does there; these are its
    // widths for the hint, the label and the SVG on Windows x64.
    let (hint_width, expected_label, expected_svg) = if liberation_mono_missing_on_windows() {
        (164.171_88, 102.359_375, 61.8125)
    } else {
        (171.421_88, 110.359_375, 61.0625)
    };
    assert!((actual.size.width - hint_width).abs() < 0.1, "{actual:?}");
    assert_eq!(actual.max_x(), 236.0);
    assert_eq!(actual.max_y(), 120.0);
    for [_, label, svg] in &hints[..2] {
        // Pinned Chromium shares the shrink between the SVG and the label,
        // leaving enough room for "Shortcuts &" (105.62px) on the first line.
        let label_width = find(&tree, *label).border_box.size.width;
        let svg_width = find(&tree, *svg).border_box.size.width;
        assert!((label_width - expected_label).abs() < 0.1, "{label_width}");
        assert!((svg_width - expected_svg).abs() < 0.1, "{svg_width}");
        let baseline_at = |offset| {
            tree.nodes
                .values()
                .filter_map(|fragment| match &fragment.content {
                    FragmentContent::Text(runs) if fragment.owner.raw() == label + 1 => Some(runs),
                    _ => None,
                })
                .flatten()
                .find(|run| {
                    run.source_ranges
                        .iter()
                        .any(|range| range.start_utf16 == offset)
                })
                .expect("glyph at text offset")
                .baseline
        };
        assert_eq!(baseline_at(0), baseline_at(10));
        assert_eq!(baseline_at(12) - baseline_at(10), 20.0);
    }
    assert_eq!(find(&tree, hints[2][2]).border_box.size.width, 85.0);
}

#[test]
fn applies_letter_spacing_to_flex_labels_without_widening_inline_boxes() {
    let mut fixture = Fixture::new(
        "body { margin: 0; font: 16px/20px 'Liberation Mono', monospace }
         .links { display: flex; gap: 12px }
         .link { display: flex; gap: 8px }
         .spaced { letter-spacing: 4px }
         .icon { display: inline-block; width: 16px; height: 16px }",
    );
    let links = fixture.element(BODY, "div", &[("class", "links")]);
    let mut items = Vec::new();
    for class in ["link", "link spaced", "link spaced"] {
        let link = fixture.element(links, "a", &[("class", class)]);
        let icon_wrap = fixture.element(link, "span", &[]);
        fixture.element(icon_wrap, "span", &[("class", "icon")]);
        fixture.text(link, "XX");
        items.push((link, icon_wrap));
    }
    let tree = layout(&fixture.pack());
    for (_, icon_wrap) in &items {
        assert_eq!(find(&tree, *icon_wrap).border_box.size.width, 16.0);
    }
    let unspaced = find(&tree, items[0].0).border_box;
    let spaced = find(&tree, items[1].0).border_box;
    let last = find(&tree, items[2].0).border_box;
    assert!((spaced.size.width - unspaced.size.width - 8.0).abs() < 0.001);
    assert!((last.origin.x - spaced.max_x() - 12.0).abs() < 0.001);
}

#[test]
fn keeps_intrinsically_sized_flex_labels_on_one_line() {
    let mut fixture = Fixture::new(
        "body { margin: 0; font: 12px/14px 'Liberation Mono', monospace }
         header { display: flex; align-items: start }
         .link { display: flex; align-items: center; gap: 8px;
                 padding: 10px 16px; border: 1px solid; letter-spacing: .4px }
         .icon-wrap { line-height: 0 }
         .icon { display: inline-block; width: 16px; height: 16px }",
    );
    let header = fixture.element(BODY, "header", &[]);
    let mut links = Vec::new();
    for size in ["", "box-sizing: border-box; width: 138px"] {
        let link = fixture.element(header, "a", &[("class", "link"), ("style", size)]);
        let icon_wrap = fixture.element(link, "span", &[("class", "icon-wrap")]);
        fixture.element(icon_wrap, "span", &[("class", "icon")]);
        fixture.text(link, "Read our blog");
        links.push(link);
    }
    let tree = layout(&fixture.pack());
    assert_eq!(find(&tree, links[0]).border_box.size.height, 38.0);
    assert_eq!(find(&tree, links[1]).border_box.size.height, 50.0);
}

#[test]
fn sizes_percentage_controls_and_legends_without_forcing_a_wider_track() {
    let mut fixture = Fixture::new(
        "body { margin: 0 }
         .grid { display: grid; grid-template-columns: 1fr 28px; gap: 8px; width:120px }
         input { width:100%; box-sizing:border-box; border:0; padding:0 }
         #min { min-width:100px }
         fieldset { width:150px; margin:0; border:0; padding:0 }
         legend span { display:inline-block; width:72px; height:12px }",
    );
    let grid = fixture.element(BODY, "div", &[("class", "grid")]);
    let field = fixture.element(grid, "input", &[("value", "Text")]);
    let end = fixture.element(grid, "span", &[]);
    let grid = fixture.element(BODY, "div", &[("class", "grid")]);
    let minimum = fixture.element(grid, "input", &[("id", "min")]);
    fixture.element(grid, "span", &[]);
    let fieldset = fixture.element(BODY, "fieldset", &[]);
    let legend = fixture.element(fieldset, "legend", &[]);
    fixture.element(legend, "span", &[]);
    let tree = layout(&fixture.pack());
    assert_eq!(find(&tree, field).border_box.size.width, 84.0);
    assert_eq!(find(&tree, end).border_box.origin.x, 92.0);
    assert_eq!(find(&tree, minimum).border_box.size.width, 100.0);
    assert_eq!(find(&tree, legend).border_box.size.width, 72.0);
}

#[test]
fn reuses_paragraph_shapes_without_reusing_paint_owners() {
    let mut fixture = Fixture::new(
        "body { margin: 0; font: 16px/20px monospace }
         #blue { color: blue } #red { color: red }",
    );
    let p1 = fixture.element(BODY, "p", &[("id", "red")]);
    let span1 = fixture.element(p1, "span", &[]);
    fixture.text(span1, "same text");
    let p2 = fixture.element(BODY, "p", &[("id", "blue")]);
    let span2 = fixture.element(p2, "span", &[]);
    fixture.text(span2, "same text");
    let tree = layout(&fixture.pack());
    for (owner, color) in [
        (span1 + 1, PaintColor::srgb(1.0, 0.0, 0.0, 1.0)),
        (span2 + 1, PaintColor::srgb(0.0, 0.0, 1.0, 1.0)),
    ] {
        let fragment = tree
            .nodes
            .values()
            .find(|f| f.owner.raw() == owner && f.kind == FragmentKind::Text)
            .unwrap();
        let FragmentContent::Text(runs) = &fragment.content else {
            panic!("expected glyphs")
        };
        assert!(
            runs[0]
                .source_ranges
                .iter()
                .all(|range| range.node.raw() == owner)
        );
        assert_eq!(fragment.style.art.color, color);
    }
}

#[test]
fn matches_stylesheet_faces_by_their_descriptors_and_unicode_ranges() {
    let regular: &[u8] = include_bytes!(
        "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
    );
    let bold: &[u8] =
        include_bytes!("../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Bold.woff2");
    let italic: &[u8] = include_bytes!(
        "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Italic.woff2"
    );
    let mono: &[u8] = include_bytes!(
        "../../../tests/reftests/cases/r1/_assets/fonts/LiberationMono-Regular.woff2"
    );
    // KaTeX's pattern: one family, a face per weight and style. The
    // descriptors, not the files, decide which face is which, so these swap
    // the regular and bold files. The split family is @fontsource's pattern:
    // one face per unicode-range subset.
    let mut fixture = Fixture::new(
        "@font-face { font-family: Swap; src: url(/regular.woff2); font-weight: 700 }
         @font-face { font-family: Swap; src: url(/missing.woff2), url(/bold.woff2) }
         @font-face { font-family: Swap; src: url(/italic.woff2); font-style: italic;
                      font-weight: 100 900 }
         @font-face { font-family: Split; src: url(/mono.woff2); unicode-range: U+30-39 }
         @font-face { font-family: Split; src: url(/regular.woff2);
                      unicode-range: U+0-2F, U+3A-FF }
         @font-face { font-family: Missing; src: local(Strange Quark Missing Font) }
         body { margin: 0; font: 16px/20px Swap, monospace }
         #bold { font-weight: 600 } #italic { font-style: italic; font-weight: 700 }
         #split { font-family: Split } #missing { font-family: Missing, Split }",
    );
    let mut paragraphs = Vec::new();
    for (id, text) in [
        ("regular", "Regular"),
        ("bold", "Bold"),
        ("italic", "Italic"),
        ("split", "ab12"),
        ("missing", "x"),
    ] {
        let p = fixture.element(BODY, "p", &[("id", id)]);
        fixture.text(p, text);
        paragraphs.push(p + 1);
    }
    let pack = fixture.pack_with_assets(&[
        ("/regular.woff2", 5, regular),
        ("/bold.woff2", 5, bold),
        ("/italic.woff2", 5, italic),
        ("/mono.woff2", 5, mono),
    ]);
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        &pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    let mut engine = LayoutEngine::default();
    let tree = engine.layout(&doc, &cascade, VIEWPORT);
    // Faces register with their cmap cut to their coverage, so identify the
    // file a run used by the family, weight and style it declares itself.
    let identify = |data: &[u8]| {
        use parley::fontique::{Blob, Collection, CollectionOptions, FontStyle};
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let registered =
            collection.register_fonts(Blob::new(std::sync::Arc::new(data.to_vec())), None);
        let (family, fonts) = registered.first()?;
        let font = fonts.first()?;
        let name = collection.family_name(*family)?.to_owned();
        Some(
            match (
                name.as_str(),
                font.weight().value(),
                font.style() == FontStyle::Italic,
            ) {
                ("Liberation Sans", 400.0, false) => "regular",
                ("Liberation Sans", 700.0, false) => "bold",
                ("Liberation Sans", 400.0, true) => "italic",
                ("Liberation Mono", 400.0, false) => "mono",
                _ => "other",
            },
        )
    };
    let fonts_of = |owner: u32| {
        tree.nodes
            .values()
            .filter(|fragment| fragment.owner.raw() == owner)
            .filter_map(|fragment| match &fragment.content {
                FragmentContent::Text(runs) => Some(runs),
                _ => None,
            })
            .flatten()
            .map(|run| {
                let data = engine.fonts().instance(run.font).unwrap().font.data.data();
                identify(data).unwrap_or("unreadable")
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(fonts_of(paragraphs[0]), ["bold"]);
    assert_eq!(fonts_of(paragraphs[1]), ["regular"]);
    assert_eq!(fonts_of(paragraphs[2]), ["italic"]);
    assert_eq!(fonts_of(paragraphs[3]), ["regular", "mono"]);
    // A face whose only source is missing gives way to the next family.
    assert_eq!(fonts_of(paragraphs[4]), ["regular"]);
    // Faces other than the first available font do not size the line.
    for text in paragraphs {
        assert_eq!(find(&tree, text - 1).border_box.size.height, 20.0);
    }
}

#[test]
fn synthesizes_bold_and_italic_for_faces_that_lack_them() {
    let regular: &[u8] = include_bytes!(
        "../../../tests/reftests/cases/r1/_assets/fonts/LiberationSans-Regular.woff2"
    );
    let mut fixture = Fixture::new(
        "@font-face { font-family: Regular; src: url(/regular.woff2) }
         @font-face { font-family: Medium; src: url(/regular.woff2); font-weight: 500 }
         @font-face { font-family: Range; src: url(/regular.woff2); font-weight: 100 900 }
         @font-face { font-family: Shared; src: url(/regular.woff2) }
         @font-face { font-family: Shared; src: url(/regular.woff2); font-weight: 700 }
         body { margin: 0; font: 16px/20px Regular, monospace }
         p { margin: 0; white-space: nowrap }",
    );
    // Chromium 159's choices: a web face is emboldened from weight 600 when
    // none of its weights reaches 600, and slanted for italic and obliques
    // from 14deg when it is upright.
    let cases = [
        ("", [false, false]),
        ("font-weight: 500", [false, false]),
        ("font-weight: 600", [true, false]),
        ("font-weight: 700", [true, false]),
        ("font-style: italic", [false, true]),
        ("font-style: oblique", [false, true]),
        ("font-style: oblique 13deg", [false, false]),
        ("font-style: oblique 30deg", [false, true]),
        ("font-weight: 700; font-style: italic", [true, true]),
        ("font-weight: 700; font-synthesis: none", [false, false]),
        ("font-weight: 700; font-synthesis: style", [false, false]),
        (
            "font-weight: 700; font-synthesis-weight: none",
            [false, false],
        ),
        (
            "font-style: italic; font-synthesis-style: none",
            [false, false],
        ),
        ("font-family: Medium; font-weight: 700", [true, false]),
        ("font-family: Range; font-weight: 700", [false, false]),
        // One file as two faces: matching picks the 700 one.
        ("font-family: Shared; font-weight: 600", [false, false]),
        ("font-family: Shared; font-weight: 400", [false, false]),
    ];
    let paragraphs: Vec<u32> = cases
        .iter()
        .map(|(style, _)| {
            let p = fixture.element(BODY, "p", &[("style", style)]);
            fixture.text(p, "Illl");
            p + 1
        })
        .collect();
    let pack = fixture.pack_with_assets(&[("/regular.woff2", 5, regular)]);
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        &pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    let mut engine = LayoutEngine::default();
    let tree = engine.layout(&doc, &cascade, VIEWPORT);
    let runs = |owner: u32| {
        tree.nodes
            .values()
            .filter(|fragment| fragment.owner.raw() == owner)
            .filter_map(|fragment| match &fragment.content {
                FragmentContent::Text(runs) => Some((fragment.border_box.size.width, runs)),
                _ => None,
            })
            .flat_map(|(width, runs)| runs.iter().map(move |run| (width, run.font)))
            .collect::<Vec<_>>()
    };
    let advance = runs(paragraphs[0])[0].0;
    for ((style, expected), text) in cases.iter().zip(paragraphs) {
        let runs = runs(text);
        assert_eq!(runs.len(), 1, "{style}");
        let (width, font) = runs[0];
        let synthesis = engine.fonts().instance(font).unwrap().synthesis;
        assert_eq!([synthesis.bold, synthesis.italic], *expected, "{style}");
        // The synthesis leaves the advances alone.
        assert_eq!(width, advance, "{style}");
    }
}

#[test]
fn shapes_text_with_font_variation_settings_and_optical_sizing() {
    let mut fixture = Fixture::new(
        "body { font: 20px monospace }
         #axes { font-variation-settings: 'wght' 700, 'wdth' 80 }
         #fixed { font-optical-sizing: none; font-variation-settings: 'wght' 300 }",
    );
    let axes = fixture.element(BODY, "p", &[("id", "axes")]);
    let fixed = fixture.element(BODY, "p", &[("id", "fixed")]);
    let pack = fixture.pack();
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        &pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    let variations = |id: u32| {
        let css = cascade
            .computed_style(&doc, NodeId::from_raw(id), None)
            .unwrap();
        let parley::FontVariations::List(list) =
            squark_text::style(NodeId::NONE, &css).font_variations
        else {
            panic!("expected a variation list");
        };
        list.iter()
            .map(|variation| (variation.tag.to_bytes(), variation.value))
            .collect::<Vec<_>>()
    };
    // Stylo sorts the settings by tag. Optical sizing adds an `opsz` axis
    // at the font size unless it is off.
    assert_eq!(
        variations(axes),
        [(*b"opsz", 20.0), (*b"wdth", 80.0), (*b"wght", 700.0)]
    );
    assert_eq!(variations(fixed), [(*b"wght", 300.0)]);
}

#[test]
fn projects_overscroll_behavior_from_css() {
    use squark_types::fragment::OverscrollBehavior;
    let mut fixture = Fixture::new(
        "div { overflow: auto; height: 20px }
         #contain { overscroll-behavior: contain none }
         #block { overscroll-behavior-y: contain }",
    );
    let contain = fixture.element(BODY, "div", &[("id", "contain")]);
    let block = fixture.element(BODY, "div", &[("id", "block")]);
    let plain = fixture.element(BODY, "div", &[]);
    let tree = layout(&fixture.pack());
    let overscroll = |id| find(&tree, id).style.box_paint.overscroll;
    assert_eq!(
        overscroll(contain),
        [OverscrollBehavior::Contain, OverscrollBehavior::None]
    );
    assert_eq!(
        overscroll(block),
        [OverscrollBehavior::Auto, OverscrollBehavior::Contain]
    );
    assert_eq!(overscroll(plain), [OverscrollBehavior::Auto; 2]);
}

#[test]
fn projects_backdrop_filters_from_css() {
    use squark_types::fragment::Filter;
    let mut fixture = Fixture::new(
        "#glass { width: 20px; height: 20px; backdrop-filter: invert(1) grayscale(50%) }",
    );
    let glass = fixture.element(BODY, "div", &[("id", "glass")]);
    let pack = fixture.pack();
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        &pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    let units: Vec<u16> = "backdrop-filter: blur(2px)".encode_utf16().collect();
    assert!(cascade.css_supports(&units));
    let tree = LayoutEngine::default().layout(&doc, &cascade, VIEWPORT);
    assert_eq!(
        *find(&tree, glass).style.properties.backdrop_filters,
        [Filter::Invert(1.0), Filter::Grayscale(0.5)]
    );
}

#[test]
fn sizes_lays_out_and_clips_contained_boxes() {
    let mut fixture = Fixture::new(
        "body { margin: 0; font: 16px/20px monospace }
         div { padding: 2px; border: 1px solid }
         .tall { height: 50px; width: 70px }
         #size { contain: size; width: 100px }
         #inline { display: inline-block; contain: inline-size }
         #float { position: absolute; top: 300px; contain: size; min-height: 12px }
         #strict { contain: strict; width: 30px; height: 10px }
         #paint { contain: paint; width: 40px; height: 10px }
         #layout { contain: layout; width: 40px; height: 10px }
         span { contain: paint }",
    );
    let mut boxes = Vec::new();
    for id in ["size", "inline", "float", "strict", "paint", "layout"] {
        let contained = fixture.element(BODY, "div", &[("id", id)]);
        let child = fixture.element(contained, "div", &[("class", "tall")]);
        fixture.text(child, "contents");
        boxes.push(contained);
    }
    let inline = fixture.element(BODY, "span", &[]);
    fixture.text(inline, "inline");
    let pack = fixture.pack();
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        &pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    for condition in ["contain: strict", "contain: inline-size layout"] {
        let units: Vec<u16> = condition.encode_utf16().collect();
        assert!(cascade.css_supports(&units), "{condition}");
    }
    let tree = LayoutEngine::default().layout(&doc, &cascade, VIEWPORT);
    let size = |id: u32| {
        let size = find(&tree, id).border_box.size;
        [size.width, size.height]
    };
    // Size containment sizes the box as if it were empty: padding and
    // border only, in both axes or the inline one.
    assert_eq!(size(boxes[0]), [106.0, 6.0]);
    assert_eq!(size(boxes[1])[0], 6.0);
    assert_eq!(size(boxes[1])[1], 62.0);
    assert_eq!(size(boxes[2]), [6.0, 18.0]);
    assert_eq!(size(boxes[3]), [36.0, 16.0]);
    // The contents still lay out, overflowing the contained box.
    assert_eq!(size(boxes[0] + 1), [76.0, 56.0]);
    // Paint containment clips without scrolling; layout containment alone
    // does not clip. Neither applies to a non-atomic inline box.
    for (id, clips) in [(boxes[3], true), (boxes[4], true), (boxes[5], false)] {
        let scroll = find(&tree, id).scroll;
        assert_eq!(
            scroll.map(|frame| (frame.clip_x, frame.clip_y, frame.scroll_x, frame.scroll_y)),
            clips.then_some((true, true, false, false)),
            "{id}"
        );
    }
    assert!(
        tree.nodes
            .values()
            .filter(|fragment| fragment.owner.raw() == inline)
            .all(|fragment| fragment.scroll.is_none())
    );
}
