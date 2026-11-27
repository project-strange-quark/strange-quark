// SPDX-License-Identifier: MIT
//! Maps DOM text positions to the inline layouts that rendered them and
//! back, for `Range.getClientRects()` and `caretPositionFromPoint()`.
//!
//! Fragment emission keeps each paragraph's Parley layout and source map
//! instead of dropping them; nothing is copied per frame. Queries are rare
//! next to frames, so the index from Text nodes and line fragments to
//! paragraphs is built on the first query after a layout.

use std::{collections::HashMap, ops::Range, rc::Rc};

use parley::{Layout, Line, PositionedLayoutItem};
use squark_mirror::Document;
use squark_text::TextBrush;
use squark_types::{
    NodeId, Point, Rect,
    fragment::{DisplayKind, Fragment, FragmentId, FragmentKind, PositionKind},
};

use crate::{inline::Source, query::Geometry};

/// A DOM position: a Text node and UTF-16 offset, or a node and child index.
pub(crate) type Caret = (NodeId, u32);

/// One inline formatting context as it was laid out.
#[derive(Debug)]
pub(crate) struct Flow {
    /// The box whose content holds the line fragments.
    pub fragment: FragmentId,
    /// The layout origin in unscrolled document coordinates.
    pub origin: Point,
    /// The painted line fragments, in order.
    pub lines: Box<[FragmentId]>,
    pub layout: Rc<Layout<TextBrush>>,
    pub text: String,
    pub sources: Vec<Source>,
    /// Collapsible white space: the space a soft wrap leaves at the end of
    /// a line takes no width, as Chromium removes it.
    pub collapsible: bool,
}

#[derive(Debug, Default)]
pub(crate) struct Flows {
    flows: Vec<Flow>,
    index: Option<Index>,
}

#[derive(Debug)]
struct Index {
    /// Each run of one node's sources in a flow, sorted by node.
    texts: Vec<(NodeId, u32, Range<u32>)>,
    lines: HashMap<FragmentId, u32>,
}

impl Flows {
    pub fn push(&mut self, flow: Flow) {
        self.flows.push(flow);
    }

    /// Builds the lookup tables once per layout, before the first query.
    pub fn prepare(&mut self) {
        if self.index.is_some() {
            return;
        }
        let mut texts = Vec::new();
        let mut lines = HashMap::new();
        for (index, flow) in self.flows.iter().enumerate() {
            let index = index as u32;
            let mut start = 0;
            for end in 1..=flow.sources.len() {
                let node = flow.sources[start].range.node;
                if end == flow.sources.len() || flow.sources[end].range.node != node {
                    texts.push((node, index, start as u32..end as u32));
                    start = end;
                }
            }
            lines.extend(flow.lines.iter().map(|&line| (line, index)));
        }
        texts.sort_by_key(|&(node, flow, _)| (node, flow));
        self.index = Some(Index { texts, lines });
    }

    fn runs(&self, node: NodeId) -> &[(NodeId, u32, Range<u32>)] {
        let Some(index) = &self.index else {
            return &[];
        };
        let start = index.texts.partition_point(|entry| entry.0 < node);
        let end = index.texts.partition_point(|entry| entry.0 <= node);
        &index.texts[start..end]
    }

    fn line_flow(&self, line: FragmentId) -> Option<&Flow> {
        let index = self.index.as_ref()?.lines.get(&line)?;
        self.flows.get(*index as usize)
    }

    /// Text box pieces that the UTF-16 range of a Text node covers, in
    /// unscrolled document coordinates of the flow's box. A range that
    /// covers no rendered text gives a zero-width caret rect.
    pub fn text_rects(&self, node: NodeId, start: u32, end: u32) -> Vec<(FragmentId, Rect)> {
        let end = end.max(start);
        let mut rects = Vec::new();
        let runs = self.runs(node);
        let mut first_run = 0;
        while first_run < runs.len() {
            let flow_index = runs[first_run].1;
            let count = runs[first_run..]
                .iter()
                .take_while(|run| run.1 == flow_index)
                .count();
            let flow = &self.flows[flow_index as usize];
            let group = &runs[first_run..first_run + count];
            first_run += count;
            let own = || {
                group
                    .iter()
                    .flat_map(|run| &flow.sources[run.2.start as usize..run.2.end as usize])
            };
            let (Some(head), Some(tail)) = (own().next(), own().next_back()) else {
                continue;
            };
            let first = own().find(|source| source.range.end_utf16 > start);
            let last = own().rev().find(|source| source.range.start_utf16 < end);
            let start_byte = first.map_or_else(|| flow.end_of(tail), |source| source.byte as usize);
            let end_byte = last.map_or(head.byte as usize, |source| flow.end_of(source));
            let before = rects.len();
            if start < end && start_byte < end_byte {
                flow.pieces(start_byte..end_byte, &mut |rect| {
                    rects.push((flow.fragment, rect));
                });
            }
            if rects.len() == before {
                // A collapsed position attaches to the next character, or to
                // the last one at the end of the node, as Chromium places it.
                rects.extend(
                    flow.caret(start_byte, first.is_none())
                        .map(|rect| (flow.fragment, rect)),
                );
            }
        }
        rects
    }

    /// The caret position for a client point that hit testing resolved to
    /// `hit`, following Chromium's `PositionForPoint` on Linux.
    pub fn caret_at(&self, geometry: &Geometry<'_>, hit: NodeId, point: Point) -> Option<Caret> {
        Finder {
            geometry,
            flows: self,
            point,
        }
        .at(hit)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopKind {
    Text,
    /// White space hanging at the end of a soft-wrapped line.
    Hanging,
    Newline,
    /// An atomic inline, an inline box edge or generated content.
    Object,
    /// A bidi control or other layout-only character.
    Empty,
}

/// One cluster of a line in visual order, in layout coordinates.
#[derive(Debug)]
struct Stop {
    bytes: Range<usize>,
    x: Range<f32>,
    rtl: bool,
    style: u16,
    kind: StopKind,
}

impl Flow {
    fn source(&self, byte: usize) -> Option<&Source> {
        let index = self
            .sources
            .binary_search_by_key(&(byte as u32), |source| source.byte)
            .ok()?;
        self.sources.get(index)
    }

    fn end_of(&self, source: &Source) -> usize {
        let byte = source.byte as usize;
        byte + self.text[byte..].chars().next().map_or(0, char::len_utf8)
    }

    fn painted_lines(&self) -> impl Iterator<Item = Line<'_, TextBrush>> {
        self.layout.lines().take(self.lines.len())
    }

    fn stops(&self, line: &Line<'_, TextBrush>) -> Vec<Stop> {
        let metrics = line.metrics();
        let range = line.text_range();
        let hang = if metrics.hanging_advance > 0.0 {
            let bytes = self.text.as_bytes();
            let mut end = range.end;
            while end > range.start && matches!(bytes[end - 1], b' ' | b'\t') {
                end -= 1;
            }
            end
        } else {
            range.end
        };
        let rtl_paragraph = self.layout.is_rtl();
        let mut x = metrics.offset + metrics.inline_min_coord;
        let mut stops = Vec::new();
        for run in line.runs() {
            for cluster in run.visual_clusters() {
                let bytes = cluster.text_range();
                let advance = cluster.advance();
                let kind = if cluster.is_hard_line_break() {
                    StopKind::Newline
                } else if self.source(bytes.start).is_some() {
                    if bytes.start >= hang {
                        StopKind::Hanging
                    } else {
                        StopKind::Text
                    }
                } else if self.text[bytes.start..].starts_with('\u{fffc}') {
                    StopKind::Object
                } else {
                    StopKind::Empty
                };
                let span = if kind == StopKind::Hanging && self.collapsible {
                    // Keep the removed space next to the line's content.
                    let edge = if rtl_paragraph { x + advance } else { x };
                    edge..edge
                } else {
                    x..x + advance
                };
                stops.push(Stop {
                    bytes,
                    x: span,
                    rtl: cluster.is_rtl(),
                    style: cluster.style_index(),
                    kind,
                });
                x += advance;
            }
        }
        stops
    }

    fn pieces(&self, bytes: Range<usize>, push: &mut impl FnMut(Rect)) {
        let mut push = |rect: Rect| push(self.place(rect));
        for line in self.painted_lines() {
            let range = line.text_range();
            if range.end <= bytes.start || range.start >= bytes.end {
                continue;
            }
            let vertical = Vertical::new(&line);
            let mut piece: Option<(Rect, bool, StopKind)> = None;
            for stop in self.stops(&line) {
                let selected = matches!(stop.kind, StopKind::Text | StopKind::Hanging)
                    && bytes.contains(&stop.bytes.start);
                if !selected {
                    if let Some((rect, _, _)) = piece.take() {
                        push(rect);
                    }
                    continue;
                }
                let (top, height) = vertical.of(stop.style);
                match &mut piece {
                    // Chromium keeps one text box per style and bidi run, and
                    // a preserved hanging space in a box of its own.
                    Some((rect, rtl, kind))
                        if *rtl == stop.rtl
                            && rect.origin.y == top
                            && rect.size.height == height
                            && (rect.max_x() - stop.x.start).abs() < 0.01
                            && (self.collapsible || *kind == stop.kind) =>
                    {
                        rect.size.width = stop.x.end - rect.origin.x;
                    }
                    _ => {
                        if let Some((rect, _, _)) = piece.take() {
                            push(rect);
                        }
                        piece = Some((
                            Rect::new(stop.x.start, top, stop.x.end - stop.x.start, height),
                            stop.rtl,
                            stop.kind,
                        ));
                    }
                }
            }
            if let Some((rect, _, _)) = piece {
                push(rect);
            }
        }
    }

    /// The zero-width caret rect at a byte position, attached to the
    /// character after it, or to the one before it when `upstream`.
    fn caret(&self, byte: usize, upstream: bool) -> Option<Rect> {
        for line in self.painted_lines() {
            let range = line.text_range();
            let inside = if upstream {
                range.start < byte && byte <= range.end
            } else {
                range.contains(&byte)
            };
            if !inside {
                continue;
            }
            let Some(stop) = self.stops(&line).into_iter().find(|stop| {
                if upstream {
                    stop.bytes.start < byte && byte <= stop.bytes.end
                } else {
                    stop.bytes.contains(&byte)
                }
            }) else {
                continue;
            };
            let (top, height) = Vertical::new(&line).of(stop.style);
            let x = if upstream != stop.rtl {
                stop.x.end
            } else {
                stop.x.start
            };
            return Some(self.place(Rect::new(x, top, 0.0, height)));
        }
        // The end of the paragraph has no following character.
        if upstream {
            None
        } else {
            self.caret(byte, true)
        }
    }

    fn place(&self, rect: Rect) -> Rect {
        Rect::new(
            self.origin.x + rect.origin.x,
            self.origin.y + rect.origin.y,
            rect.size.width,
            rect.size.height,
        )
    }

    /// The line boxes' union in unscrolled document coordinates.
    fn bounds(&self) -> Rect {
        let mut bounds: Option<Rect> = None;
        for line in self.painted_lines() {
            let metrics = line.metrics();
            let rect = self.place(Rect::new(
                metrics.inline_min_coord,
                metrics.block_min_coord,
                metrics.inline_max_coord - metrics.inline_min_coord,
                metrics.block_max_coord - metrics.block_min_coord,
            ));
            bounds = Some(bounds.map_or(rect, |bounds| union(bounds, rect)));
        }
        bounds.unwrap_or(Rect::new(self.origin.x, self.origin.y, 0.0, 0.0))
    }

    fn before(&self, document: &Document, bytes: &Range<usize>) -> Option<Caret> {
        let index = self
            .sources
            .partition_point(|source| (source.byte as usize) < bytes.start);
        let source = self
            .sources
            .get(index)
            .filter(|source| (source.byte as usize) < bytes.end)?;
        position(document, source, false)
    }

    fn after(&self, document: &Document, bytes: &Range<usize>) -> Option<Caret> {
        let end = self
            .sources
            .partition_point(|source| (source.byte as usize) < bytes.end);
        let source = self.sources[..end]
            .last()
            .filter(|source| source.byte as usize >= bytes.start)?;
        position(document, source, true)
    }
}

/// The text box (ascent and descent of the span's primary font) for each
/// style on a line, as Chromium sizes text fragments.
struct Vertical {
    styles: Vec<(u16, f32, f32)>,
    fallback: (f32, f32),
}

impl Vertical {
    fn new(line: &Line<'_, TextBrush>) -> Self {
        let metrics = line.metrics();
        let styles = line
            .items()
            .filter_map(|item| match item {
                PositionedLayoutItem::GlyphRun(run) => {
                    let span = run.span_metrics();
                    Some((
                        run.style_index(),
                        run.baseline() - span.ascent,
                        span.ascent + span.descent,
                    ))
                }
                PositionedLayoutItem::InlineBox(_) => None,
            })
            .collect();
        Self {
            styles,
            fallback: (
                metrics.content_block_min_coord,
                metrics.content_block_max_coord - metrics.content_block_min_coord,
            ),
        }
    }

    fn of(&self, style: u16) -> (f32, f32) {
        self.styles
            .iter()
            .find(|entry| entry.0 == style)
            .map_or(self.fallback, |entry| (entry.1, entry.2))
    }
}

fn position(document: &Document, source: &Source, after: bool) -> Option<Caret> {
    let range = source.range;
    if document.is_text(range.node) {
        Some((
            range.node,
            if after {
                range.end_utf16
            } else {
                range.start_utf16
            },
        ))
    } else {
        // A <br> is a position in its parent.
        beside(document, range.node, after)
    }
}

fn beside(document: &Document, node: NodeId, after: bool) -> Option<Caret> {
    let Some(parent) = document.parent(node) else {
        return Some((node, 0));
    };
    let index = document
        .children(parent)?
        .iter()
        .position(|&child| child == node)?;
    Some((parent, index as u32 + u32::from(after)))
}

/// Form controls render a value from their UA shadow tree, so a position in
/// them is the position before the control, as Chromium adjusts it.
fn is_control(document: &Document, node: NodeId) -> bool {
    matches!(
        document.html_element(node),
        Some("input" | "textarea" | "select")
    )
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

fn distance(rect: Rect, point: Point) -> f32 {
    let dx = (rect.origin.x - point.x)
        .max(point.x - rect.max_x())
        .max(0.0);
    let dy = (rect.origin.y - point.y)
        .max(point.y - rect.max_y())
        .max(0.0);
    dx.hypot(dy)
}

#[derive(Clone, Copy)]
enum Candidate<'a> {
    Flow(&'a Flow),
    Box(&'a Fragment),
}

/// A meaningful caret target on a line: text, or an atomic inline.
struct Target<'a> {
    bytes: Range<usize>,
    x: Range<f32>,
    rtl: bool,
    atomic: Option<&'a Fragment>,
}

struct Finder<'a, 'g> {
    geometry: &'a Geometry<'g>,
    flows: &'a Flows,
    point: Point,
}

impl<'a> Finder<'a, '_> {
    fn document(&self) -> &'a Document {
        self.geometry.document
    }

    fn at(&self, hit: NodeId) -> Option<Caret> {
        let document = self.document();
        if hit == document.root() {
            return self.in_box(self.geometry.viewport());
        }
        let fragment = self
            .geometry
            .boxes(hit)
            .find(|fragment| self.contains(fragment))
            .or_else(|| self.geometry.boxes(hit).next());
        if let Some(fragment) = fragment {
            // A decorated inline element: the paragraph its line belongs to.
            if fragment.kind == FragmentKind::Box
                && fragment.style.stacking.display == DisplayKind::Inline
                && let Some(flow) = fragment.parent.and_then(|line| self.flows.line_flow(line))
            {
                return self.in_flow(flow);
            }
            return self.in_box(fragment);
        }
        // An inline element without a box of its own: the nearest paragraph
        // that holds its text.
        let mut nearest: Option<(f32, &Flow)> = None;
        let mut pending = vec![hit];
        while let Some(node) = pending.pop() {
            if document.is_text(node) {
                for run in self.flows.runs(node) {
                    let flow = &self.flows.flows[run.1 as usize];
                    let Some(distance) = self.flow_distance(flow) else {
                        continue;
                    };
                    if nearest.is_none_or(|(best, _)| distance < best) {
                        nearest = Some((distance, flow));
                    }
                }
            } else {
                pending.extend(document.children(node).unwrap_or_default().iter().rev());
            }
        }
        match nearest {
            Some((_, flow)) => self.in_flow(flow),
            None => self.at(document.parent(hit)?),
        }
    }

    fn contains(&self, fragment: &Fragment) -> bool {
        self.geometry
            .document_point(fragment, self.point)
            .is_some_and(|point| fragment.border_box.contains(point))
    }

    /// The point in the coordinates of the fragment's (scrolled) content.
    fn content_point(&self, fragment: &Fragment) -> Option<Point> {
        let point = self.geometry.document_point(fragment, self.point)?;
        let scroll = fragment
            .scroll
            .map_or(Point::default(), |frame| frame.offset);
        Some(Point {
            x: point.x + scroll.x,
            y: point.y + scroll.y,
        })
    }

    fn flow_distance(&self, flow: &Flow) -> Option<f32> {
        let container = self.geometry.tree.nodes.get(&flow.fragment)?;
        Some(distance(flow.bounds(), self.content_point(container)?))
    }

    fn in_box(&self, fragment: &'a Fragment) -> Option<Caret> {
        let document = self.document();
        if fragment.kind == FragmentKind::Replaced || is_control(document, fragment.owner) {
            return self.around(fragment);
        }
        let point = self.content_point(fragment)?;
        let mut candidates = Vec::new();
        for id in &fragment.children {
            let child = &self.geometry.tree.nodes[id];
            match child.kind {
                FragmentKind::Line => {
                    if let Some(flow) = self.flows.line_flow(*id)
                        && !matches!(candidates.last(), Some(Candidate::Flow(last)) if std::ptr::eq(*last, flow))
                    {
                        candidates.push(Candidate::Flow(flow));
                    }
                }
                FragmentKind::Box | FragmentKind::Replaced
                    if !matches!(
                        child.style.stacking.position,
                        PositionKind::Absolute | PositionKind::Fixed
                    ) =>
                {
                    candidates.push(Candidate::Box(child));
                }
                _ => {}
            }
        }
        let Some(&last) = candidates.last() else {
            return Some((fragment.owner, 0));
        };
        let bounds = |candidate: &Candidate<'_>| match candidate {
            Candidate::Flow(flow) => flow.bounds(),
            Candidate::Box(child) => child.border_box,
        };
        let chosen = if matches!(
            fragment.style.stacking.display,
            DisplayKind::Flex
                | DisplayKind::InlineFlex
                | DisplayKind::Grid
                | DisplayKind::InlineGrid
                | DisplayKind::Table
        ) {
            candidates
                .iter()
                .copied()
                .min_by(|a, b| distance(bounds(a), point).total_cmp(&distance(bounds(b), point)))
                .unwrap_or(last)
        } else {
            // Block flow takes the first child that ends below the point.
            candidates
                .iter()
                .copied()
                .find(|candidate| point.y < bounds(candidate).max_y())
                .unwrap_or(last)
        };
        match chosen {
            Candidate::Flow(flow) => self.in_flow(flow),
            Candidate::Box(child) => self.in_box(child),
        }
    }

    /// Before a replaced element, or after it past its middle.
    fn around(&self, fragment: &Fragment) -> Option<Caret> {
        let after = fragment.kind == FragmentKind::Replaced
            && self
                .geometry
                .document_point(fragment, self.point)
                .is_some_and(|point| {
                    point.x >= fragment.border_box.origin.x + fragment.border_box.size.width / 2.0
                });
        beside(self.document(), fragment.owner, after)
    }

    fn in_flow(&self, flow: &'a Flow) -> Option<Caret> {
        let container = self.geometry.tree.nodes.get(&flow.fragment)?;
        let point = self.content_point(container)?;
        let x = point.x - flow.origin.x;
        let y = point.y - flow.origin.y;
        let mut before = None;
        let mut after = None;
        for (index, line) in flow.painted_lines().enumerate() {
            let metrics = line.metrics();
            if y < metrics.block_min_coord {
                after.get_or_insert(index);
                continue;
            }
            // A point on the bottom edge belongs to the next line.
            if y >= metrics.block_max_coord {
                before = Some(index);
                continue;
            }
            if let Some(caret) = self.in_line(flow, index, &line, x) {
                return Some(caret);
            }
        }
        // Past the top or bottom, Linux and macOS go to the line boundary.
        if let Some(index) = before {
            return self.line_edge(flow, index, true);
        }
        self.line_edge(flow, after?, false)
    }

    fn targets(&self, flow: &'a Flow, index: usize, stops: &[Stop]) -> Vec<Target<'a>> {
        let line = flow
            .lines
            .get(index)
            .and_then(|id| self.geometry.tree.nodes.get(id));
        stops
            .iter()
            .filter_map(|stop| {
                let atomic = match stop.kind {
                    StopKind::Text => None,
                    StopKind::Hanging if !flow.collapsible => None,
                    StopKind::Object => Some(self.atomic(flow, line?, stop)?),
                    _ => return None,
                };
                Some(Target {
                    bytes: stop.bytes.clone(),
                    x: stop.x.clone(),
                    rtl: stop.rtl,
                    atomic,
                })
            })
            .collect()
    }

    /// The atomic inline box an object character stands for. Inline box
    /// edges and generated content have none, so they act as gaps.
    fn atomic(&self, flow: &Flow, line: &'a Fragment, stop: &Stop) -> Option<&'a Fragment> {
        let start = flow.origin.x + stop.x.start;
        let end = flow.origin.x + stop.x.end;
        line.children.iter().find_map(|id| {
            let child = self.geometry.tree.nodes.get(id)?;
            let atomic = child.kind == FragmentKind::Replaced
                || (child.kind == FragmentKind::Box
                    && child.style.stacking.display != DisplayKind::Inline);
            let center = child.border_box.origin.x + child.border_box.size.width / 2.0;
            (atomic && start <= center && center <= end).then_some(child)
        })
    }

    fn in_line(
        &self,
        flow: &'a Flow,
        index: usize,
        line: &Line<'_, TextBrush>,
        x: f32,
    ) -> Option<Caret> {
        let stops = flow.stops(line);
        let targets = self.targets(flow, index, &stops);
        let (Some(first), Some(last)) = (targets.first(), targets.last()) else {
            return self.line_edge(flow, index, false);
        };
        if let Some(target) = targets
            .iter()
            .find(|target| target.x.start <= x && x < target.x.end)
        {
            return match target.atomic {
                Some(fragment) => self.in_box(fragment),
                None => self.edge(flow, target, x < (target.x.start + target.x.end) / 2.0),
            };
        }
        let rtl = flow.layout.is_rtl();
        if x < first.x.start {
            return self.line_edge(flow, index, rtl);
        }
        if x >= last.x.end {
            return self.line_edge(flow, index, !rtl);
        }
        // Between targets the caret goes to the next one in line direction.
        if rtl {
            let target = targets.iter().rev().find(|target| target.x.end <= x)?;
            self.edge(flow, target, false)
        } else {
            let target = targets.iter().find(|target| target.x.start >= x)?;
            self.edge(flow, target, true)
        }
    }

    /// The position at a target's visual left or right edge.
    fn edge(&self, flow: &Flow, target: &Target<'_>, left: bool) -> Option<Caret> {
        let after = left == target.rtl;
        match target.atomic {
            Some(fragment) => beside(self.document(), fragment.owner, after),
            None if after => flow.after(self.document(), &target.bytes),
            None => flow.before(self.document(), &target.bytes),
        }
    }

    /// The logical start or end of a line. The end excludes a forced break
    /// and a removed hanging space.
    fn line_edge(&self, flow: &'a Flow, index: usize, end: bool) -> Option<Caret> {
        let line = flow.layout.get(index)?;
        let stops = flow.stops(&line);
        let targets = self.targets(flow, index, &stops);
        let target = if end {
            targets.iter().max_by_key(|target| target.bytes.start)
        } else {
            targets.iter().min_by_key(|target| target.bytes.start)
        };
        let document = self.document();
        match target {
            Some(target) => match target.atomic {
                Some(fragment) => beside(document, fragment.owner, end),
                None if end => flow.after(document, &target.bytes),
                None => flow.before(document, &target.bytes),
            },
            // An empty line holds only its forced break.
            None => {
                let newline = stops.iter().find(|stop| stop.kind == StopKind::Newline)?;
                flow.before(document, &newline.bytes)
            }
        }
    }
}
