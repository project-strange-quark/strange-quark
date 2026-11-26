// SPDX-License-Identifier: MIT
//! CPU geometry comparison for tools' packed R1 inline fixtures.

use std::{collections::BTreeMap, fs::File, path::PathBuf};

use serde_json::Value;
use squark_assets::Pack;
use squark_layout::LayoutEngine;
use squark_style::{Cascade, screen};
use squark_types::{
    Rect, Size,
    fragment::{Fragment, FragmentKind, FragmentTree},
};
use url::Url;

#[cfg(not(target_os = "linux"))]
#[path = "font_warning.rs"]
mod font_warning;

/// Set SQUARK_R1_PACKS to the directory of packs named after the case IDs.
#[test]
#[ignore = "uses R1 packs built by squark-cli"]
fn compares_r1_layout_to_chromium() {
    #[cfg(not(target_os = "linux"))]
    font_warning::warn_if_missing();
    let packs = PathBuf::from(std::env::var_os("SQUARK_R1_PACKS").expect("packed R1 fixtures"));
    let expected: Value =
        serde_json::from_str(include_str!("../../../tests/results/r1/geometry.json")).unwrap();
    let mut compared = 0;
    let mut cases = 0;
    let mut text_lines = 0;
    let mut errors = Vec::new();
    for (case, spec) in expected["cases"].as_object().unwrap() {
        if !case.starts_with("01-inline/") {
            continue;
        }
        cases += 1;
        let path = packs.join(case).with_extension("sqpak");
        let pack = Pack::open(
            &File::open(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display())),
        )
        .unwrap();
        let view = Size {
            width: spec["width"].as_f64().unwrap() as f32,
            height: spec["height"].as_f64().unwrap() as f32,
        };
        let media = screen(
            view.width,
            view.height,
            spec["dpr"].as_f64().unwrap() as f32,
        );
        let document_url = Url::parse(&format!("https://r1.test/{case}.html")).unwrap();
        let (mut doc, _, mut cascade) =
            Cascade::from_pack(&pack, document_url, media.clone()).unwrap();
        cascade.update(&mut doc, media).unwrap();
        let tree = LayoutEngine::default().layout(&doc, &cascade, view);
        let named: BTreeMap<_, _> = tree
            .nodes
            .values()
            .filter(|frag| matches!(frag.kind, FragmentKind::Box | FragmentKind::Replaced))
            .filter_map(|frag| {
                let name = String::from_utf16_lossy(doc.attribute(frag.owner, "data-geom")?);
                Some((name, frag))
            })
            .collect();
        for (name, expected) in spec["elements"].as_object().unwrap() {
            let Some(actual) = named.get(name) else {
                errors.push(format!("{case} {name} has no box fragment"));
                continue;
            };
            if case.starts_with("01-inline/06-") || case.starts_with("01-inline/07-") {
                compare_lines(
                    &tree,
                    actual,
                    case,
                    name,
                    &expected["rangeRects"],
                    &mut errors,
                );
                text_lines += expected["rangeRects"].as_array().unwrap().len();
            }
            let expected = &expected["boundingRect"];
            let actual = actual.border_box;
            let metrics = [
                ("x", actual.origin.x),
                ("y", actual.origin.y),
                ("width", actual.size.width),
                ("height", actual.size.height),
            ];
            for (key, actual) in metrics {
                let expected = expected[key].as_f64().unwrap() as f32;
                if (actual - expected).abs() > 0.25 {
                    errors.push(format!(
                        "{case} {name} {key}: Chromium {expected}, engine {actual}"
                    ));
                }
            }
            compared += 1;
        }
    }
    assert!(cases > 0 && compared > 0);
    println!("{cases} R1 inline cases, {compared} boxes, {text_lines} text ranges");
    for error in &errors {
        eprintln!("{error}");
    }
    assert!(errors.is_empty(), "{} geometry mismatches", errors.len());
}

fn compare_lines(
    tree: &FragmentTree,
    fragment: &Fragment,
    case: &str,
    name: &str,
    expected: &Value,
    errors: &mut Vec<String>,
) {
    let mut actual = Vec::new();
    for child in &fragment.children {
        let line = &tree.nodes[child];
        if line.kind != FragmentKind::Line {
            continue;
        }
        let rect = line
            .children
            .iter()
            .filter_map(|id| {
                let text = &tree.nodes[id];
                (text.kind == FragmentKind::Text).then_some(text.border_box)
            })
            .reduce(union);
        actual.extend(rect);
    }
    let expected = expected.as_array().unwrap();
    if actual.len() != expected.len() {
        errors.push(format!(
            "{case} {name}: expected {} text lines, found {}",
            expected.len(),
            actual.len()
        ));
    }
    for (line, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        for (field, value) in [("x", actual.origin.x), ("y", actual.origin.y)] {
            let expected = expected[field].as_f64().unwrap() as f32;
            if (value - expected).abs() > 1.0 {
                errors.push(format!(
                    "{case} {name} line {line} {field}: Chromium {expected}, engine {value}"
                ));
            }
        }
    }
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.origin.x.min(b.origin.x);
    let y = a.origin.y.min(b.origin.y);
    Rect::new(
        x,
        y,
        a.max_x().max(b.max_x()) - x,
        a.max_y().max(b.max_y()) - y,
    )
}
