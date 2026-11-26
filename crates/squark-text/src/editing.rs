// SPDX-License-Identifier: MIT

use std::ops::Range;

use parley::{
    BoundingBox, Layout,
    editing::{Cursor, Selection},
    layout::Affinity,
};
use squark_types::{Point, Rect};

use crate::TextBrush;

/// The committed value stays in the mirror. Build the visual value with the
/// composition spliced in, and keep the relationship between both offsets.
#[derive(Debug)]
pub struct EditText {
    value_len: u32,
    selection: Range<u32>,
    backward: bool,
    preedit_len: u32,
    preedit_cursor: u32,
}

impl EditText {
    pub fn project(
        value: &[u16],
        selection: Range<u32>,
        backward: bool,
        preedit: &[u16],
        cursor: u32,
        password: bool,
    ) -> (Self, Vec<u16>) {
        let value_len = value.len() as u32;
        let end = selection.end.min(value_len);
        let selection = selection.start.min(end)..end;
        let preedit_len = preedit.len() as u32;
        let mut display = if preedit.is_empty() {
            value.to_vec()
        } else {
            let start = selection.start as usize;
            let end = selection.end as usize;
            [&value[..start], preedit, &value[end..]].concat()
        };
        if password {
            display.fill(0x2022);
        }
        (
            Self {
                value_len,
                selection,
                backward,
                preedit_len,
                preedit_cursor: cursor.min(preedit_len),
            },
            display,
        )
    }

    pub fn focus(&self) -> u32 {
        if self.backward {
            self.selection.start
        } else {
            self.selection.end
        }
    }

    fn preedit(&self) -> Option<Range<u32>> {
        (self.preedit_len > 0)
            .then(|| self.selection.start..self.selection.start + self.preedit_len)
    }

    fn visual_offset(&self, value_offset: u32) -> u32 {
        let offset = value_offset.min(self.value_len);
        match self.preedit() {
            None => offset,
            Some(_) if offset < self.selection.start => offset,
            Some(range) if offset > self.selection.end => range.end + (offset - self.selection.end),
            Some(range) => range.start + self.preedit_cursor,
        }
    }

    fn value_offset(&self, visual_offset: u32) -> u32 {
        match self.preedit() {
            Some(range) if visual_offset >= range.end => {
                (self.selection.end + (visual_offset - range.end)).min(self.value_len)
            }
            Some(range) if visual_offset >= range.start => self.selection.start,
            _ => visual_offset.min(self.value_len),
        }
    }
}

/// Maps original UTF-16 positions in the displayed value to Parley's UTF-8
/// cluster positions. The layout supplies visual ordering and line affinity.
#[derive(Debug)]
pub struct TextPositions {
    edit: EditText,
    offsets: Vec<(u32, usize)>,
}

impl TextPositions {
    pub fn new(
        edit: EditText,
        starts: impl Iterator<Item = (u32, usize)>,
        end_byte: usize,
        placeholder: bool,
    ) -> Self {
        let end_unit = edit.preedit().map_or(edit.value_len, |range| {
            range.end + (edit.value_len - edit.selection.end)
        });
        let offsets = if placeholder {
            vec![(0, 0)]
        } else {
            starts.chain([(end_unit, end_byte)]).collect()
        };
        Self { edit, offsets }
    }

    fn cursor(&self, layout: &Layout<TextBrush>, offset: u32, affinity: Affinity) -> Cursor {
        let right = self.offsets.partition_point(|&(unit, _)| unit < offset);
        let index = match self.offsets.get(right) {
            Some(&(unit, _)) if unit == offset => right,
            _ => right.saturating_sub(1),
        };
        Cursor::from_byte_index(layout, self.offsets[index].1, affinity)
    }

    pub fn caret(&self, layout: &Layout<TextBrush>, offset: u32) -> Rect {
        let offset = self.edit.visual_offset(offset);
        rect(
            self.cursor(layout, offset, Affinity::Downstream)
                .geometry(layout, 1.0),
        )
    }

    pub fn offset_at(&self, layout: &Layout<TextBrush>, point: Point) -> u32 {
        let byte = Cursor::from_point(layout, point.x, point.y).index();
        self.edit.value_offset(self.unit_from_byte(byte))
    }

    fn unit_from_byte(&self, byte: usize) -> u32 {
        let index = self.offsets.partition_point(|&(_, start)| start <= byte);
        self.offsets[index.saturating_sub(1)].0
    }

    pub fn selected_at_byte(&self, byte: usize) -> bool {
        self.edit.preedit_len == 0 && self.edit.selection.contains(&self.unit_from_byte(byte))
    }

    pub fn focus(&self) -> u32 {
        self.edit.focus()
    }

    pub fn selection(&self, layout: &Layout<TextBrush>) -> Vec<(Rect, usize)> {
        if self.edit.preedit_len > 0 {
            return Vec::new();
        }
        self.range_rects(layout, self.edit.selection.clone())
    }

    pub fn preedit(&self, layout: &Layout<TextBrush>) -> Vec<(Rect, usize)> {
        self.edit
            .preedit()
            .map_or_else(Vec::new, |range| self.range_rects(layout, range))
    }

    fn range_rects(&self, layout: &Layout<TextBrush>, range: Range<u32>) -> Vec<(Rect, usize)> {
        if range.is_empty() {
            return Vec::new();
        }
        let start = self.cursor(layout, range.start, Affinity::Downstream);
        let end = self.cursor(layout, range.end, Affinity::Upstream);
        Selection::new(start, end)
            .geometry(layout)
            .into_iter()
            .map(|(bounds, line)| (rect(bounds), line))
            .collect()
    }
}

fn rect(bounds: BoundingBox) -> Rect {
    Rect::new(
        bounds.x0 as f32,
        bounds.y0 as f32,
        (bounds.x1 - bounds.x0) as f32,
        (bounds.y1 - bounds.y0) as f32,
    )
}
