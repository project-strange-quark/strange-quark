// SPDX-License-Identifier: MIT
//! Table layout against the geometry the pinned Chromium gives the same
//! markup and styles.

use squark_types::{Rect, fragment::FragmentTree};

use super::{BODY, Fixture, find, layout};

const CSS: &str = "
    body { margin: 0; font: 20px/30px monospace }
    table { border-spacing: 0 }
    td { padding: 0; width: 20px }
    td div { height: 10px }
    .top { vertical-align: top; height: 100px }
    .middle { vertical-align: middle }
    .bottom { vertical-align: bottom }
    .baseline { vertical-align: baseline }
    .shifted { padding-top: 15px }
    .marker { display: inline-block; width: 0; height: 0 }
    .inline-table { display: inline-table; border-collapse: collapse }
    .row { display: table-row }
    .cell { display: table-cell; vertical-align: bottom; width: 10px }
    .tall { height: 40px }
    .short { height: 15px }
    .inline-block { display: inline-block }
    .table { display: table }
";

fn rect(tree: &FragmentTree, node: u32) -> Rect {
    find(tree, node).border_box
}

#[test]
fn aligns_cell_content_inside_cells_that_fill_their_row() {
    let mut fixture = Fixture::new(CSS);
    let table = fixture.element(BODY, "table", &[]);
    let row = fixture.element(table, "tr", &[]);
    let mut cells = Vec::new();
    let mut blocks = Vec::new();
    // `td` is `vertical-align: middle` in the user agent style sheet.
    for class in [
        "top",
        "middle",
        "bottom",
        "baseline",
        "baseline shifted",
        "",
    ] {
        let cell = fixture.element(row, "td", &[("class", class)]);
        blocks.push(fixture.element(cell, "div", &[]));
        cells.push(cell);
    }
    let tree = layout(&fixture.pack());
    assert_eq!(rect(&tree, table), Rect::new(0.0, 0.0, 120.0, 100.0));
    for (index, &cell) in cells.iter().enumerate() {
        assert_eq!(
            rect(&tree, cell),
            Rect::new(20.0 * index as f32, 0.0, 20.0, 100.0)
        );
    }
    // A cell without a line box has its baseline at the bottom of its
    // content, so the unpadded baseline cell moves down to the padded one.
    let tops: Vec<f32> = blocks
        .iter()
        .map(|&block| rect(&tree, block).origin.y)
        .collect();
    assert_eq!(tops, [0.0, 45.0, 90.0, 15.0, 15.0, 45.0]);
}

#[test]
fn aligns_inline_tables_by_their_first_row() {
    let mut fixture = Fixture::new(CSS);
    let cell = |fixture: &mut Fixture, row, class| {
        fixture.element(row, "span", &[("class", class)]);
    };
    // The first row has no cell on the baseline, so its baseline is the
    // bottom of its cells' content boxes.
    let line = fixture.element(BODY, "div", &[]);
    let marker = fixture.element(line, "span", &[("class", "marker")]);
    let table = fixture.element(line, "span", &[("class", "inline-table")]);
    let first = fixture.element(table, "span", &[("class", "row")]);
    cell(&mut fixture, first, "cell tall");
    let second = fixture.element(table, "span", &[("class", "row")]);
    cell(&mut fixture, second, "cell short");
    // An inline-block takes no baseline from a table inside it, so it sits
    // on its bottom margin edge.
    let line = fixture.element(BODY, "div", &[]);
    let block_marker = fixture.element(line, "span", &[("class", "marker")]);
    let block = fixture.element(line, "span", &[("class", "inline-block")]);
    let inner = fixture.element(block, "span", &[("class", "table")]);
    let row = fixture.element(inner, "span", &[("class", "row")]);
    cell(&mut fixture, row, "cell tall");

    let tree = layout(&fixture.pack());
    assert_eq!(rect(&tree, table), Rect::new(0.0, 0.0, 10.0, 55.0));
    assert_eq!(rect(&tree, marker).origin.y, 40.0);
    assert_eq!(rect(&tree, block), Rect::new(0.0, 55.0, 10.0, 40.0));
    assert_eq!(rect(&tree, block_marker).origin.y, 95.0);
}

/// KaTeX stacks a superscript as a bottom-aligned cell whose zero-height
/// block is moved up by `top` and holds a clipped strut that sets the
/// line's baseline.
#[test]
fn places_katex_style_vertical_lists_on_the_baseline() {
    let mut fixture = Fixture::new(&format!(
        "{CSS}
        .vlist {{ display: table-cell; vertical-align: bottom; position: relative; height: 16px }}
        .lift {{ display: block; height: 0; position: relative; top: -3em }}
        .lift > span {{ display: inline-block }}
        .pstrut {{ overflow: hidden; width: 0; height: 2.7em }}
        .script {{ width: 10px; height: 10px }}
        .gap {{ display: table-cell; font-size: 1px; line-height: 1.2; width: 2px;
                min-width: 2px; vertical-align: bottom }}"
    ));
    let line = fixture.element(BODY, "div", &[]);
    let marker = fixture.element(line, "span", &[("class", "marker")]);
    let table = fixture.element(line, "span", &[("class", "inline-table")]);
    let row = fixture.element(table, "span", &[("class", "row")]);
    let vlist = fixture.element(row, "span", &[("class", "vlist")]);
    let lift = fixture.element(vlist, "span", &[("class", "lift")]);
    fixture.element(lift, "span", &[("class", "pstrut")]);
    let script = fixture.element(lift, "span", &[("class", "script")]);
    let gap = fixture.element(row, "span", &[("class", "gap")]);
    fixture.text(gap, "\u{200b}");

    let tree = layout(&fixture.pack());
    let baseline = rect(&tree, marker).origin.y;
    assert_eq!(rect(&tree, table).size.height, 16.0);
    assert_eq!(rect(&tree, table).origin.y, baseline - 16.0);
    // The script's bottom sits 2.7em below the block, which is 3em above
    // the bottom of the row.
    assert_eq!(rect(&tree, script).origin.y, baseline - 16.0);
}

#[test]
fn wraps_misparented_cells_and_loose_content_in_anonymous_tables() {
    let mut fixture = Fixture::new(&format!(
        "{CSS}
        .first {{ display: table-cell; width: 30px; height: 10px }}
        .second {{ display: table-cell; width: 20px; height: 10px }}
        .centered {{ display: table; margin: 0 auto }}
        .box {{ display: inline-block; width: 40px; height: 10px }}"
    ));
    let block = fixture.element(BODY, "div", &[]);
    let first = fixture.element(block, "span", &[("class", "first")]);
    fixture.text(block, " ");
    let second = fixture.element(block, "span", &[("class", "second")]);
    let centered = fixture.element(BODY, "div", &[("class", "centered")]);
    fixture.element(centered, "span", &[("class", "box")]);

    let tree = layout(&fixture.pack());
    assert_eq!(rect(&tree, first), Rect::new(0.0, 0.0, 30.0, 10.0));
    assert_eq!(rect(&tree, second), Rect::new(30.0, 0.0, 20.0, 10.0));
    // The table shrinks to its anonymous cell around the inline content.
    assert_eq!(rect(&tree, centered), Rect::new(300.0, 10.0, 40.0, 30.0));
}

#[test]
fn sizes_tables_by_their_box_sizing() {
    let mut fixture = Fixture::new(&format!(
        "{CSS}
        .sized {{ width: 200px; height: 50px; padding: 10px; border: 5px solid }}"
    ));
    // `table` elements are `border-box` in the user agent style sheet.
    let element = fixture.element(BODY, "table", &[("class", "sized")]);
    let row = fixture.element(element, "tr", &[]);
    fixture.element(row, "td", &[]);
    let styled = fixture.element(BODY, "div", &[("class", "table sized")]);
    let cell = fixture.element(styled, "div", &[("class", "cell")]);

    let tree = layout(&fixture.pack());
    assert_eq!(rect(&tree, element), Rect::new(0.0, 0.0, 200.0, 50.0));
    assert_eq!(rect(&tree, styled), Rect::new(0.0, 50.0, 230.0, 80.0));
    // A lone cell fills the table's content box.
    assert_eq!(rect(&tree, cell), Rect::new(15.0, 65.0, 200.0, 50.0));
}

#[test]
fn moves_relatively_positioned_inline_blocks() {
    let mut fixture = Fixture::new(&format!(
        "{CSS}
        .moved {{ display: inline-block; position: relative; left: -5px; top: 3px;
                  width: 10px; height: 10px }}"
    ));
    let line = fixture.element(BODY, "div", &[]);
    let marker = fixture.element(line, "span", &[("class", "marker")]);
    let moved = fixture.element(line, "span", &[("class", "moved")]);

    let tree = layout(&fixture.pack());
    let baseline = rect(&tree, marker).origin.y;
    assert_eq!(
        rect(&tree, moved),
        Rect::new(-5.0, baseline - 10.0 + 3.0, 10.0, 10.0)
    );
}

/// KaTeX's struts set a line's depth with fractional `vertical-align`
/// lengths, which Chromium does not round.
#[test]
fn keeps_fractional_vertical_align_lengths_of_inline_blocks() {
    let mut fixture = Fixture::new(&format!(
        "{CSS}
        .lowered {{ display: inline-block; width: 0; height: 20px; vertical-align: -2.5px }}
        .raised {{ display: inline-block; width: 0; height: 20px; vertical-align: 7.25px }}"
    ));
    let line = fixture.element(BODY, "div", &[]);
    let marker = fixture.element(line, "span", &[("class", "marker")]);
    let lowered = fixture.element(line, "span", &[("class", "lowered")]);
    let raised = fixture.element(line, "span", &[("class", "raised")]);

    let tree = layout(&fixture.pack());
    let baseline = rect(&tree, marker).origin.y;
    assert_eq!(rect(&tree, lowered).origin.y, baseline + 2.5 - 20.0);
    assert_eq!(rect(&tree, raised).origin.y, baseline - 7.25 - 20.0);
}
