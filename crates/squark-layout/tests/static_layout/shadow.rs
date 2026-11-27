// SPDX-License-Identifier: MIT
//! Shadow trees: style scoping, :host, ::slotted, slots and the flat tree.

use squark_assets::Pack;
use squark_layout::{LayoutEngine, StaticDocument};
use squark_mirror::Document;
use squark_style::{Cascade, screen};
use squark_types::{
    NodeId,
    engine::{DecodedBatch, DocumentEngine, FrameInput, Query, QueryValue, ScrollSync},
    fragment::FragmentTree,
    journal::{NO_STRING, Opcode, Record},
    render::{FrameTick, Viewport, resource_channel},
};
use url::Url;

use super::{BODY, Fixture, VIEWPORT, find};

impl Fixture {
    fn attach_shadow(&mut self, host: u32) -> u32 {
        self.next += 1;
        self.journal
            .record(Opcode::AttachShadow, 0, self.next, host, 0)
            .unwrap();
        self.next
    }

    fn assign(&mut self, slot: u32, nodes: &[u32]) {
        self.journal
            .record(Opcode::AssignSlot, 0, slot, 0, 0)
            .unwrap();
        for &node in nodes {
            self.journal
                .record(Opcode::AssignSlot, 0, slot, node, 0)
                .unwrap();
        }
    }

    fn sheet(&mut self, rules: &str) -> u32 {
        self.next += 1;
        self.journal
            .record(Opcode::CreateSheet, 0, self.next, NO_STRING, 0)
            .unwrap();
        self.journal
            .record(Opcode::SheetInsert, 0, self.next, 0, rules)
            .unwrap();
        self.next
    }

    fn adopt(&mut self, scope: u32, sheets: &[u32]) {
        self.journal
            .record(Opcode::AdoptSheet, 0, scope, 0, 0)
            .unwrap();
        for &sheet in sheets {
            self.journal
                .record(Opcode::AdoptSheet, 0, scope, sheet, 0)
                .unwrap();
        }
    }
}

fn styled(pack: &Pack) -> (Document, FragmentTree) {
    let media = screen(VIEWPORT.width, VIEWPORT.height, 1.0);
    let (mut doc, _, mut cascade) = Cascade::from_pack(
        pack,
        Url::parse("https://app.test/index.html").unwrap(),
        media.clone(),
    )
    .unwrap();
    cascade.update(&mut doc, media).unwrap();
    let tree = LayoutEngine::default().layout(&doc, &cascade, VIEWPORT);
    (doc, tree)
}

fn color(doc: &Document, node: u32) -> [f32; 3] {
    let css = doc.computed_style(NodeId::from_raw(node)).unwrap();
    let color = css
        .get_inherited_text()
        .color
        .to_color_space(style::color::ColorSpace::Srgb);
    [color.components.0, color.components.1, color.components.2]
}

const RED: [f32; 3] = [1.0, 0.0, 0.0];
const GREEN: [f32; 3] = [0.0, 1.0, 0.0];
const BLUE: [f32; 3] = [0.0, 0.0, 1.0];

#[test]
fn shadow_styles_scope_to_their_tree_and_slots_render_the_flat_tree() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        .inner { height: 99px; width: 99px }
        #host { color: rgb(255, 0, 0) }
        #light { height: 7px }
        slot { height: 1000px }",
    );
    let outside = fixture.element(BODY, "div", &[("class", "inner")]);
    let host = fixture.element(BODY, "div", &[("id", "host")]);
    let light = fixture.element(host, "div", &[("id", "light"), ("slot", "a")]);
    let unslotted = fixture.element(host, "div", &[("slot", "nowhere")]);
    let root = fixture.attach_shadow(host);
    let style = fixture.element(root, "style", &[]);
    fixture.text(
        style,
        "
        :host { display: block; width: 300px; padding-left: 4px }
        :host(#other) { width: 1px }
        .inner { height: 10px; width: 50px }
        ::slotted(#light) { width: 120px }
        ::slotted(div) { height: 2px }
        slot[name=a] { color: rgb(0, 255, 0) }
        #fallback { height: 5px; color: rgb(0, 0, 255) }",
    );
    let inner = fixture.element(root, "div", &[("class", "inner")]);
    let named = fixture.element(root, "slot", &[("name", "a")]);
    let unnamed = fixture.element(root, "slot", &[]);
    let fallback = fixture.element(unnamed, "div", &[("id", "fallback")]);
    fixture.assign(named, &[light]);
    let (doc, tree) = styled(&fixture.pack());

    let host_box = find(&tree, host);
    assert_eq!(host_box.border_box.size.width, 304.0, ":host applies");
    let inner_box = find(&tree, inner);
    assert_eq!(
        (
            inner_box.border_box.size.width,
            inner_box.border_box.size.height
        ),
        (50.0, 10.0),
        "document rules stay out of the shadow tree"
    );
    let outside_box = find(&tree, outside);
    assert_eq!(
        outside_box.border_box.size.height, 99.0,
        "shadow rules stay inside it"
    );
    let light_box = find(&tree, light);
    assert_eq!(
        (
            light_box.border_box.size.width,
            light_box.border_box.size.height
        ),
        (120.0, 7.0),
        "::slotted applies under the outer tree's rules"
    );
    assert_eq!(
        light_box.parent,
        Some(host_box.id),
        "slots are display: contents"
    );
    assert_eq!(
        color(&doc, light),
        GREEN,
        "slotted nodes inherit from the slot"
    );
    assert_eq!(
        color(&doc, inner),
        RED,
        "shadow children inherit from the host"
    );
    assert_eq!(find(&tree, fallback).border_box.size.height, 5.0);
    assert_eq!(color(&doc, fallback), BLUE);
    assert!(
        !tree
            .nodes
            .values()
            .any(|fragment| fragment.owner.raw() == unslotted),
        "an unassigned light child is not in the flat tree"
    );
    let order: Vec<_> = tree
        .nodes
        .values()
        .filter(|fragment| fragment.parent == Some(host_box.id))
        .map(|fragment| fragment.owner.raw())
        .collect();
    assert_eq!(order, [inner, light, fallback]);
}

#[test]
fn adopted_sheets_follow_tree_sheets_in_their_scope() {
    let mut fixture = Fixture::new("body { margin: 0 } .box { width: 10px; height: 10px }");
    let host = fixture.element(BODY, "div", &[]);
    let root = fixture.attach_shadow(host);
    let style = fixture.element(root, "style", &[]);
    fixture.text(style, ".box { width: 20px; height: 20px }");
    let inside = fixture.element(root, "div", &[("class", "box")]);
    let outside = fixture.element(BODY, "div", &[("class", "box")]);
    let shadow_sheet = fixture.sheet(".box { width: 30px }");
    let document_sheet = fixture.sheet(".box { height: 40px }");
    fixture.adopt(root, &[shadow_sheet]);
    fixture.adopt(1, &[document_sheet]);
    let (_, tree) = styled(&fixture.pack());
    let size = |node| {
        let size = find(&tree, node).border_box.size;
        (size.width, size.height)
    };
    assert_eq!(size(inside), (30.0, 20.0));
    assert_eq!(size(outside), (10.0, 40.0));
}

#[test]
fn part_rules_reach_exported_parts_across_shadow_trees() {
    let mut fixture = Fixture::new(
        "
        body { margin: 0 }
        div { height: 5px }
        #outer::part(label) { width: 77px }
        #outer::part(inner-label) { width: 66px }
        #outer::part(hidden) { width: 1px }",
    );
    let outer = fixture.element(BODY, "div", &[("id", "outer")]);
    let root = fixture.attach_shadow(outer);
    let label = fixture.element(root, "div", &[("part", "label other")]);
    let nested = fixture.element(root, "div", &[("exportparts", "label: inner-label")]);
    let nested_root = fixture.attach_shadow(nested);
    let inner = fixture.element(nested_root, "div", &[("part", "label")]);
    let hidden = fixture.element(nested_root, "div", &[("part", "hidden")]);
    let (_, tree) = styled(&fixture.pack());
    let width = |node| find(&tree, node).border_box.size.width;
    assert_eq!(width(label), 77.0);
    assert_eq!(width(inner), 66.0, "exportparts renames the part outward");
    assert_eq!(width(hidden), 640.0, "an unexported part stays private");
}

fn input() -> FrameInput<'static> {
    let now = std::time::Instant::now();
    FrameInput {
        tick: FrameTick {
            sequence: 1,
            vsync: now,
            present_at: now,
            commit_deadline: now,
            viewport: Viewport {
                css: VIEWPORT,
                device: [640, 480],
                scale: 1.0,
                epoch: 1,
            },
        },
        media: Default::default(),
        scroll: ScrollSync::default(),
    }
}

fn record(op: Opcode, a: u32, b: u32) -> Record {
    Record {
        op,
        flags: 0,
        a,
        b,
        c: 0,
    }
}

#[test]
fn reassigning_slots_restyles_and_relays_out_the_moved_nodes() {
    let mut fixture = Fixture::new("body { margin: 0 }");
    let host = fixture.element(BODY, "div", &[]);
    let light = fixture.element(host, "div", &[]);
    let root = fixture.attach_shadow(host);
    let style = fixture.element(root, "style", &[]);
    fixture.text(
        style,
        "
        #first { display: block; color: rgb(255, 0, 0); padding-left: 10px }
        #second { display: block; color: rgb(0, 0, 255); padding-left: 30px }
        ::slotted(*) { height: 5px }",
    );
    let first = fixture.element(root, "slot", &[("id", "first")]);
    let second = fixture.element(root, "slot", &[("id", "second"), ("name", "b")]);
    fixture.assign(first, &[light]);
    let pack = fixture.pack();
    let (send, _updates) = resource_channel();
    let mut engine =
        StaticDocument::from_pack(&pack, "https://app.test/index.html".parse().unwrap(), send)
            .unwrap();
    let left = |engine: &mut StaticDocument<'_>| {
        let QueryValue::Rect(rect) = engine
            .query(&input(), Query::BoundingRect(NodeId::from_raw(light)))
            .unwrap()
        else {
            panic!("expected a rect");
        };
        (rect.origin.x, rect.size.height)
    };
    assert_eq!(left(&mut engine), (10.0, 5.0));
    engine
        .flush(DecodedBatch {
            records: &[
                record(Opcode::AssignSlot, first, 0),
                record(Opcode::AssignSlot, second, 0),
                record(Opcode::AssignSlot, second, light),
            ],
            strings: &[],
            staged_bytes: &[],
        })
        .unwrap();
    assert_eq!(left(&mut engine), (30.0, 5.0));
    let QueryValue::String(color) = engine
        .query(
            &input(),
            Query::ComputedStyle {
                node: NodeId::from_raw(light),
                pseudo: None,
                property: "color",
            },
        )
        .unwrap()
    else {
        panic!("expected a computed value");
    };
    assert_eq!(color, "rgb(0, 0, 255)");
}
