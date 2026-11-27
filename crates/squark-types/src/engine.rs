// SPDX-License-Identifier: MIT
//! Calls from the document-thread driver into the engine.

use std::fmt;
use std::time::{Duration, Instant};

use crate::fragment::{FragmentTree, MirrorRead};
use crate::journal::Record;
use crate::render::{FrameTick, Viewport};
use crate::{NodeId, Point, Rect};

/// The decoder owns the journal transaction. This borrowed view lets the
/// engine consume a validated batch before the caller acknowledges it.
#[derive(Clone, Copy, Debug)]
pub struct DecodedBatch<'a> {
    pub records: &'a [Record],
    pub strings: &'a [&'a [u16]],
    pub staged_bytes: &'a [(u32, &'a [u8])],
}

pub trait DocumentEngine {
    fn flush(&mut self, batch: DecodedBatch<'_>) -> Result<FlushReport, EngineError>;
    fn update_layout(&mut self, input: &FrameInput<'_>) -> Result<(), EngineError>;
    fn resize_entries(&mut self, deeper_than: u32) -> ResizeDelivery;
    fn intersection_entries(&mut self, now: Instant) -> Vec<IntersectionEntry>;
    fn snapshot(&self) -> (&FragmentTree, &dyn MirrorRead);
    fn query(
        &mut self,
        input: &FrameInput<'_>,
        query: Query<'_>,
    ) -> Result<QueryValue, EngineError>;
    fn parse_fragment(&self, context: NodeId, html: &[u16])
    -> Result<Vec<ParsedNode>, EngineError>;

    /// Parse a whole HTML document with scripting disabled, for `DOMParser`.
    fn parse_html_document(&self, _html: &[u16]) -> Result<ParsedDocument, EngineError> {
        Err(EngineError::Unsupported("HTML document parsing"))
    }

    /// Run a CSS selector query against a detached inert document without touching
    /// the live document mirror, for `DOMParser`.
    #[allow(clippy::too_many_arguments)]
    fn select_detached(
        &self,
        _root: NodeId,
        _quirks: crate::pack::QuirksMode,
        _is_html: bool,
        _batches: &[DecodedBatch<'_>],
        _scope: NodeId,
        _selector: &str,
        _mode: SelectMode,
    ) -> Result<QueryValue, EngineError> {
        Err(EngineError::Unsupported("detached selector query"))
    }
    fn measure_text(&mut self, request: MeasureText<'_>) -> Result<TextMetrics, EngineError>;

    /// Parse a CSS color against the canvas's current style without flushing.
    /// Canvas `fillStyle` and `strokeStyle` use it to validate colours.
    fn parse_color(&self, _canvas: NodeId, _text: &[u16]) -> Option<[u8; 4]> {
        None
    }

    /// Parses and serializes the Canvas 2D font shorthand in the canvas's environment.
    fn parse_canvas_font(&self, _canvas: NodeId, _css: &[u16]) -> Option<Vec<u16>> {
        None
    }

    /// Validate the CSS text before squark-dom records a canvas filter.
    fn validate_filter(&self, _text: &[u16]) -> bool {
        false
    }

    fn font_load(&mut self, face: FaceId) -> Result<(), EngineError>;

    /// Drain layout-initiated starts before delivering the matching resource results.
    fn take_font_load_starts(&mut self) -> Vec<FaceId> {
        Vec::new()
    }

    /// Drain the font resource tasks that settle font loads.
    fn take_font_completions(&mut self) -> Vec<FontCompletion> {
        Vec::new()
    }

    /// Parse the CSS font shorthand and find the faces covering the text,
    /// for `FontFaceSet.load` and `check`.
    fn match_font_faces(&self, _font: &[u16], _text: &[u16]) -> Result<Vec<FaceId>, EngineError> {
        Err(EngineError::Unsupported("font face matching"))
    }

    // Observer registrations default to no-ops, for engines without observers.
    fn observe_resize(&mut self, _observer: u32, _target: NodeId, _box_kind: ResizeBox) {}
    fn unobserve_resize(&mut self, _observer: u32, _target: NodeId) {}
    fn disconnect_resize(&mut self, _observer: u32) {}
    fn observe_intersection(
        &mut self,
        _observer: u32,
        _target: NodeId,
        _options: IntersectionOptions,
    ) {
    }
    fn unobserve_intersection(&mut self, _observer: u32, _target: NodeId) {}
    fn disconnect_intersection(&mut self, _observer: u32) {}

    fn has_running_animations(&self) -> bool {
        false
    }

    fn take_animation_events(&mut self) -> Vec<AnimationEvent> {
        Vec::new()
    }

    /// Evaluate a CSS supports condition with Stylo, for `CSS.supports`.
    fn css_supports(&self, _condition: &[u16]) -> bool {
        false
    }

    /// Drain image resource completions keyed by the committed src generation.
    fn take_image_events(&mut self) -> Vec<ImageEvent> {
        Vec::new()
    }

    fn parse_rule(&self, _text: &[u16]) -> Option<Vec<u16>> {
        None
    }

    fn set_media_state(&mut self, _viewport: &Viewport, _media: &MediaState) {}

    fn evaluate_media(&self, query: &[u16]) -> (Vec<u16>, bool) {
        (query.to_vec(), false)
    }
}

/// An image resource task: an image finished loading or failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageEvent {
    pub target: NodeId,
    /// The element's src generation.
    pub id: u32,
    pub natural_size: Option<[u32; 2]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimationEventType {
    TransitionRun,
    TransitionStart,
    TransitionEnd,
    TransitionCancel,
    AnimationStart,
    AnimationIteration,
    AnimationEnd,
    AnimationCancel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnimationEvent {
    pub target: NodeId,
    pub pseudo: Option<String>,
    pub kind: AnimationEventType,
    pub name: String,
    pub elapsed_time: f64,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FaceId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontCompletion {
    pub face: FaceId,
    pub result: Result<(), String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineError {
    MissingNode(NodeId),
    InvalidSelector,
    InvalidBatch { record: Option<u32>, reason: String },
    Unsupported(&'static str),
    Resource(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNode(id) => write!(f, "unknown node {}", id.raw()),
            Self::InvalidSelector => f.write_str("invalid selector"),
            Self::InvalidBatch {
                record: Some(index),
                reason,
            } => write!(f, "invalid record {index}: {reason}"),
            Self::InvalidBatch {
                record: None,
                reason,
            } => write!(f, "invalid batch: {reason}"),
            Self::Unsupported(feature) => write!(f, "unsupported engine feature: {feature}"),
            Self::Resource(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for EngineError {}

#[derive(Clone, Debug)]
pub struct FrameInput<'a> {
    pub tick: FrameTick,
    pub media: MediaState,
    pub scroll: ScrollSync<'a>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ScrollSync<'a> {
    pub frame: u64,
    pub tick: u64,
    pub offsets: &'a [crate::ScrollOffset],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MediaState {
    pub color_scheme: ColorScheme,
    pub reduced_motion: bool,
    pub contrast: Contrast,
    pub primary_pointer: PointerMedia,
    pub any_fine_pointer: bool,
    pub any_coarse_pointer: bool,
    pub primary_hover: bool,
    pub any_hover: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Contrast {
    #[default]
    NoPreference,
    More,
    Less,
    Custom,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerMedia {
    #[default]
    None,
    Coarse,
    Fine,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FlushReport {
    pub applied: u32,
    pub released: Vec<NodeId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeBox {
    ContentBox,
    BorderBox,
    DevicePixelContentBox,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CssLength {
    Px(f64),
    Percent(f64),
}

#[derive(Clone, Debug, PartialEq)]
pub struct IntersectionOptions {
    pub root: Option<NodeId>,
    pub root_margin: [CssLength; 4],
    pub thresholds: Vec<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResizeDelivery {
    pub entries: Vec<ResizeEntry>,
    pub next_depth: u32,
    pub skipped: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LogicalSize {
    pub inline_size: f64,
    pub block_size: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResizeEntry {
    pub observer: u32,
    pub target: NodeId,
    pub depth: u32,
    pub content_rect: Rect,
    pub content_box: Vec<LogicalSize>,
    pub border_box: Vec<LogicalSize>,
    pub device_pixel_content_box: Vec<LogicalSize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntersectionEntry {
    pub observer: u32,
    pub target: NodeId,
    pub time: Duration,
    pub root_bounds: Option<Rect>,
    pub bounds: Rect,
    pub intersection: Rect,
    pub ratio: f64,
    pub is_intersecting: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Query<'a> {
    BoundingRect(NodeId),
    ClientRects(NodeId),
    BoxMetrics(NodeId),
    ComputedStyle {
        node: NodeId,
        pseudo: Option<Pseudo>,
        property: &'a str,
    },
    Selector {
        scope: NodeId,
        selector: &'a str,
        mode: SelectMode,
    },
    ElementFromPoint(Point),
    SvgBBox(NodeId),
    SvgComputedTextLength(NodeId),
    FocusScroll(NodeId),
    SheetRules(NodeId),
    /// Whether the engine holds a stylesheet for a `<style>` or `<link>` owner:
    /// `QueryValue::Bool(false)` for a link whose resource did not load.
    SheetLoaded(NodeId),
    CaretRect(NodeId, u32),
    OffsetFromPoint(NodeId, Point),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pseudo {
    Before,
    After,
    Marker,
    Placeholder,
    FirstLetter,
    FirstLine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SelectMode {
    Match,
    First,
    All,
    Closest,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BoxMetrics {
    pub offset_parent: Option<NodeId>,
    pub offset_top: i32,
    pub offset_left: i32,
    pub offset_width: i32,
    pub offset_height: i32,
    pub client_top: i32,
    pub client_left: i32,
    pub client_width: i32,
    pub client_height: i32,
    pub scroll_width: i32,
    pub scroll_height: i32,
}

impl BoxMetrics {
    /// The numeric fast-path field indices squark-dom queries.
    pub fn number(self, field: u32) -> Option<i32> {
        let index = field.checked_sub(1)? as usize;
        [
            self.offset_top,
            self.offset_left,
            self.offset_width,
            self.offset_height,
            self.client_top,
            self.client_left,
            self.client_width,
            self.client_height,
            self.scroll_width,
            self.scroll_height,
        ]
        .get(index)
        .copied()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum QueryValue {
    Rect(Rect),
    Rects(Vec<Rect>),
    BoxMetrics(BoxMetrics),
    String(String),
    Strings(Vec<String>),
    Bool(bool),
    Node(Option<NodeId>),
    Nodes(Vec<NodeId>),
    Number(f64),
    Void,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedDocument {
    pub quirks_mode: crate::pack::QuirksMode,
    pub nodes: Vec<ParsedNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedNode {
    Element {
        name: ParsedName,
        attrs: Vec<(ParsedName, Vec<u16>)>,
        children: Vec<ParsedNode>,
    },
    Text(Vec<u16>),
    Comment(Vec<u16>),
    Doctype {
        name: Vec<u16>,
        public_id: Vec<u16>,
        system_id: Vec<u16>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedName {
    pub namespace: String,
    pub local: String,
    pub prefix: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Ltr,
    Rtl,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureText<'a> {
    pub font_css: &'a str,
    pub text: &'a [u16],
    pub direction: Direction,
    pub letter_spacing: f32,
    pub word_spacing: f32,
    pub font_environment: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextMetrics {
    pub width: f64,
    pub actual_left: f64,
    pub actual_right: f64,
    pub actual_ascent: f64,
    pub actual_descent: f64,
    pub font_ascent: f64,
    pub font_descent: f64,
    pub em_ascent: f64,
    pub em_descent: f64,
    pub alphabetic_baseline: f64,
    pub hanging_baseline: f64,
    pub ideographic_baseline: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_metric_numbers_follow_the_wire_order() {
        let metrics = BoxMetrics {
            offset_top: 1,
            offset_left: 2,
            offset_width: 3,
            offset_height: 4,
            client_top: 5,
            client_left: 6,
            client_width: 7,
            client_height: 8,
            scroll_width: 9,
            scroll_height: 10,
            ..Default::default()
        };
        assert_eq!(metrics.number(0), None);
        for index in 1..=10 {
            assert_eq!(metrics.number(index), Some(index as i32));
        }
        assert_eq!(metrics.number(11), None);
    }
}
