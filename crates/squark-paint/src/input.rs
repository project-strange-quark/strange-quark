// SPDX-License-Identifier: MIT
//! Geometry and paint values resolved for the stacking and display-list walks.

use std::{collections::BTreeMap, sync::Arc};

use squark_types::{
    NodeId, Point, Rect, Size,
    host::ListenerFlags,
    render::{GlyphKey, ImageKey},
};

use crate::{BorderSide, ClipShape, Filter, Matrix, PaintColor, PathPaint, SamplingMode, Shadow};

pub use squark_types::fragment::{
    BlendMode, Cursor, FragmentId, FragmentKind, ObjectFit, OverscrollBehavior,
    PositionKind as Position,
};

#[derive(Debug)]
pub struct FragmentTree {
    pub root: FragmentId,
    pub nodes: BTreeMap<FragmentId, Fragment>,
}

#[derive(Debug)]
pub struct Fragment {
    pub id: FragmentId,
    pub owner: NodeId,
    pub kind: FragmentKind,
    pub parent: Option<FragmentId>,
    pub containing_block: Option<FragmentId>,
    pub children: Box<[FragmentId]>,
    pub border_box: Rect,
    pub padding: [f32; 4],
    pub border: [f32; 4],
    pub scroll: Option<ScrollFrame>,
    pub sticky: Option<StickyConstraints>,
    pub content: FragmentContent,
    pub style: Arc<PaintValues>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollFrame {
    pub owner: NodeId,
    pub scrollport: Rect,
    pub content: Rect,
    pub offset: Point,
    pub generation: u64,
    pub gutter: [f32; 4],
    pub scroll_x: bool,
    pub scroll_y: bool,
    pub clip_x: bool,
    pub clip_y: bool,
    pub wheel_axes: [bool; 2],
    pub force_scrollbars: [bool; 2],
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickyConstraints {
    pub scroll_owner: NodeId,
    pub containing_rect: Rect,
    pub insets: [Option<f32>; 4],
}

#[derive(Debug, Default)]
pub enum FragmentContent {
    #[default]
    None,
    Text(Box<[PositionedRun]>),
    Image {
        key: ImageKey,
        intrinsic_size: Size,
    },
    Canvas {
        id: NodeId,
        bitmap_size: [u32; 2],
    },
    Svg(Arc<[SvgPath]>),
}

#[derive(Debug)]
pub struct SvgPath {
    pub paint: PathPaint,
    pub clip: Option<ClipShape>,
    pub transform: Option<Matrix>,
}

#[derive(Debug)]
pub struct PositionedRun {
    pub glyphs: Box<[(GlyphKey, Point)]>,
    pub under_and_overlines: Box<[(Rect, PaintColor)]>,
    pub line_through: Box<[(Rect, PaintColor)]>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Display {
    None,
    #[default]
    Block,
    Inline,
    InlineBlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoxArea {
    Border,
    Padding,
    Content,
    Text,
}

#[derive(Debug)]
pub struct BackgroundLayer {
    pub area: BoxArea,
    pub origin: Rect,
    pub layer: squark_types::fragment::BackgroundLayer,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outline {
    pub side: BorderSide,
    pub offset: f32,
}

#[derive(Debug)]
pub struct PaintValues {
    pub display: Display,
    pub position: Position,
    pub z_index: Option<i32>,
    pub floating: bool,
    pub flex_or_grid_item: bool,
    pub top_layer: Option<u32>,
    pub isolation: bool,
    pub stacking_containment: bool,
    pub visible: bool,
    pub background: PaintColor,
    pub background_propagated: bool,
    pub background_clip: BoxArea,
    pub background_layers: Box<[BackgroundLayer]>,
    pub radii: [Size; 4],
    pub border: [BorderSide; 4],
    pub outline: Option<Outline>,
    pub shadows: Box<[Shadow]>,
    pub text_color: PaintColor,
    pub text_shadows: Box<[Shadow]>,
    pub image_sampling: SamplingMode,
    pub object_fit: ObjectFit,
    /// Fraction of remaining width and height on each axis.
    pub object_position: Point,
    pub object_position_offset: Point,
    pub transform: Option<Matrix>,
    /// An offset from the untransformed border box's origin.
    pub transform_origin: [f32; 3],
    pub transform_reference: Size,
    pub animations: Arc<[squark_types::fragment::CompositorAnimation]>,
    pub perspective: Option<Matrix>,
    pub preserve_3d: bool,
    pub clip: Option<ClipShape>,
    pub opacity: f32,
    pub filters: Box<[Filter]>,
    pub backdrop: Box<[Filter]>,
    pub blend: BlendMode,
    pub overscroll: [OverscrollBehavior; 2],
    pub pointer_events: bool,
    pub cursor: Cursor,
}

impl Default for PaintValues {
    fn default() -> Self {
        Self {
            display: Display::Block,
            position: Position::Static,
            z_index: None,
            floating: false,
            flex_or_grid_item: false,
            top_layer: None,
            isolation: false,
            stacking_containment: false,
            visible: true,
            background: PaintColor::srgb([0.0; 4]),
            background_propagated: false,
            background_clip: BoxArea::Border,
            background_layers: Box::new([]),
            radii: [Size::default(); 4],
            border: [BorderSide::default(); 4],
            outline: None,
            shadows: Box::new([]),
            text_color: PaintColor::srgb([0.0, 0.0, 0.0, 1.0]),
            text_shadows: Box::new([]),
            image_sampling: SamplingMode::Linear,
            object_fit: ObjectFit::Fill,
            object_position: Point { x: 0.5, y: 0.5 },
            object_position_offset: Point::default(),
            transform: None,
            transform_origin: [0.0; 3],
            transform_reference: Size::default(),
            animations: Arc::from([]),
            perspective: None,
            preserve_3d: false,
            clip: None,
            opacity: 1.0,
            filters: Box::new([]),
            backdrop: Box::new([]),
            blend: BlendMode::Normal,
            overscroll: [OverscrollBehavior::Auto; 2],
            pointer_events: true,
            cursor: Cursor::Auto,
        }
    }
}

impl PaintValues {
    pub(crate) fn has_effect(&self) -> bool {
        self.opacity != 1.0
            || !self.filters.is_empty()
            || !self.backdrop.is_empty()
            || self.blend != BlendMode::Normal
            || self.isolation
            || self.animations.iter().any(crate::animation::affects_effect)
    }

    pub(crate) fn creates_context(&self) -> bool {
        self.has_effect()
            || self.stacking_containment
            || matches!(self.position, Position::Sticky | Position::Fixed)
            || self.transform.is_some()
            || self
                .animations
                .iter()
                .any(crate::animation::affects_transform)
            || self.perspective.is_some()
            || self.clip.is_some()
            || self.preserve_3d
            || self.top_layer.is_some()
    }
}

/// Bits on this node only. Use NodeId::NONE to read the window's flags.
pub trait MirrorRead {
    fn parent(&self, id: NodeId) -> Option<NodeId>;
    fn is_element(&self, id: NodeId) -> bool;
    fn listeners(&self, id: NodeId) -> ListenerFlags;
}
