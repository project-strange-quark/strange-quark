// SPDX-License-Identifier: MIT

use std::{collections::BTreeMap, rc::Rc};

use parley::Layout;
use squark_text::{TextBrush, TextPositions};
use squark_types::{NodeId, Point, Rect, fragment::FragmentId};

pub(crate) type Fields = BTreeMap<NodeId, Field>;

#[derive(Debug)]
pub(crate) struct Field {
    pub fragment: FragmentId,
    pub origin: Point,
    pub positions: TextPositions,
    pub layout: Rc<Layout<TextBrush>>,
}

impl Field {
    pub fn caret(&self, offset: u32) -> Rect {
        place(self.origin, self.positions.caret(&self.layout, offset))
    }

    pub fn active_caret(&self) -> Rect {
        self.caret(self.positions.focus())
    }

    pub fn offset_at(&self, point: Point) -> u32 {
        self.positions.offset_at(
            &self.layout,
            Point {
                x: point.x - self.origin.x,
                y: point.y - self.origin.y,
            },
        )
    }
}

pub(crate) fn place(origin: Point, rect: Rect) -> Rect {
    Rect::new(
        origin.x + rect.origin.x,
        origin.y + rect.origin.y,
        rect.size.width,
        rect.size.height,
    )
}

pub(crate) fn overflow(
    layout: &Layout<TextBrush>,
    origin: Point,
    port: Rect,
    single_line: bool,
) -> Rect {
    let mut start = port.origin.x;
    let mut end = port.max_x();
    let mut top = port.origin.y;
    let mut bottom = port.max_y();
    for line in layout.lines() {
        let metrics = line.metrics();
        let x = origin.x + metrics.inline_min_coord + metrics.offset;
        // Pre-wrap whitespace can hang past a soft break without expanding
        // the scrollable area. Parley leaves pre and break-spaces advances
        // in the line's non-hanging content.
        let (line_start, line_end) = if layout.is_rtl() {
            (x + metrics.hanging_advance, x + metrics.advance)
        } else {
            (x, x + metrics.advance - metrics.hanging_advance)
        };
        start = start.min(line_start);
        // A multiline caret must not create horizontal scrolling for a line
        // that already fits. Keep space for the caret on overflowing lines.
        let caret_width = if single_line || line_end > port.max_x() {
            1.0
        } else {
            0.0
        };
        end = end.max(line_end + caret_width);
        top = top.min(origin.y + metrics.content_block_min_coord);
        bottom = bottom.max(origin.y + metrics.content_block_max_coord);
    }
    Rect::new(
        start,
        top,
        end - start,
        (origin.y + layout.height()).max(bottom) - top,
    )
}
