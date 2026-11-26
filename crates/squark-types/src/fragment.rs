// SPDX-License-Identifier: MIT
//! The document-thread mirror and the geometry consumed by paint.
//!
//! Fragment geometry is in unscrolled document CSS pixels before CSS transforms.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::journal::{ElementState, ListenerFlags};
use crate::render::{FontInstanceId, ImageKey};
use crate::{NodeId, Point, Rect, Size};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AtomId(pub u32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    Document,
    Doctype,
    Fragment,
    Comment,
    Element,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Name {
    pub namespace: Option<AtomId>,
    pub local: AtomId,
    pub prefix: Option<AtomId>,
}

/// The name references a kind-2 or kind-5 journal atom. A kind-5 atom carries
/// the namespace URI, a NUL separator and the qualified name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttrRef<'a> {
    pub name: AtomId,
    pub value: &'a [u16],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Links {
    pub parent: Option<NodeId>,
    pub first: Option<NodeId>,
    pub last: Option<NodeId>,
    pub prev: Option<NodeId>,
    pub next: Option<NodeId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Dirty(pub u8);

impl Dirty {
    pub const STYLE: Self = Self(1);
    pub const LAYOUT: Self = Self(2);
    pub const PAINT: Self = Self(4);
    pub const ACCESSIBILITY: Self = Self(8);
}

/// Attribute values and atom text retain the original UTF-16 code units.
#[derive(Debug)]
pub struct NodeRead<'a> {
    pub kind: NodeKind,
    pub links: Links,
    pub name: Option<Name>,
    pub attrs: Vec<AttrRef<'a>>,
    pub text: Option<&'a [u16]>,
    pub state: ElementState,
    pub own: Dirty,
    pub descendants: Dirty,
    pub listeners: ListenerFlags,
}

pub trait MirrorRead {
    fn document(&self) -> NodeId;
    fn node(&self, id: NodeId) -> Option<NodeRead<'_>>;
    fn atom(&self, id: AtomId) -> Option<&[u16]>;

    fn window_listeners(&self) -> ListenerFlags {
        ListenerFlags::default()
    }

    /// A hit region includes blocking listeners up through Document and Window.
    fn effective_listeners(&self, target: NodeId) -> ListenerFlags {
        let mut flags = self.window_listeners().bits();
        let mut cursor = Some(target);
        while let Some(node) = cursor.and_then(|id| self.node(id)) {
            flags |= node.listeners.bits();
            cursor = node.links.parent;
        }
        ListenerFlags::from_bits(flags).expect("valid flags composed from live nodes")
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FragmentId(pub u64);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentKind {
    Viewport,
    Box,
    Line,
    Text,
    Before,
    After,
    Marker,
    Replaced,
}

#[derive(Clone, Debug)]
pub struct FragmentTree {
    pub root: FragmentId,
    /// Viewport-space caret or preedit bounds of the focused text control.
    pub ime_area: Option<Rect>,
    pub nodes: BTreeMap<FragmentId, Fragment>,
}

#[derive(Clone, Debug)]
pub struct Fragment {
    pub id: FragmentId,
    pub owner: NodeId,
    pub kind: FragmentKind,
    pub parent: Option<FragmentId>,
    pub containing_block: Option<FragmentId>,
    /// Children in tree order before paint applies CSS stacking rules.
    pub children: Box<[FragmentId]>,
    pub border_box: Rect,
    pub padding: Edges<f32>,
    pub border: Edges<f32>,
    pub first_baseline: Option<f32>,
    pub last_baseline: Option<f32>,
    pub scrollable_overflow: Rect,
    pub scroll: Option<ScrollFrame>,
    pub sticky: Option<StickyConstraints>,
    pub content: FragmentContent,
    pub style: Arc<PaintValues>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollFrame {
    pub owner: NodeId,
    /// The document's current offset for this owner and its generation
    /// (see `crate::scroll`).
    pub offset: crate::Point,
    pub generation: u64,
    pub scrollport: Rect,
    pub content: Rect,
    pub gutter: Edges<f32>,
    pub scroll_x: bool,
    pub scroll_y: bool,
    pub clip_x: bool,
    pub clip_y: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickyConstraints {
    pub scroll_owner: NodeId,
    pub containing_rect: Rect,
    pub insets: Edges<Option<f32>>,
}

#[derive(Clone, Debug)]
pub enum FragmentContent {
    None,
    Text(Box<[PositionedRun]>),
    Image { key: ImageKey, intrinsic_size: Size },
    Canvas { id: NodeId, bitmap_size: [u32; 2] },
    Svg(Arc<SvgGeometry>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TextRange {
    pub node: NodeId,
    pub start_utf16: u32,
    pub end_utf16: u32,
}

#[derive(Clone, Debug)]
pub struct PositionedRun {
    pub font: FontInstanceId,
    /// Absolute CSS y.
    pub baseline: f32,
    pub bidi_level: u8,
    pub glyphs: Box<[PositionedGlyph]>,
    pub source_ranges: Box<[TextRange]>,
    pub decoration_spans: Box<[DecorationSpan]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionedGlyph {
    pub glyph_id: u32,
    pub position: Point,
    /// Index into the run's source_ranges.
    pub cluster: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationSpan {
    pub x: [f32; 2],
    pub y: f32,
    pub thickness: f32,
    pub style: DecorationStyle,
    pub color: PaintColor,
    pub kind: DecorationKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecorationKind {
    Underline,
    Overline,
    LineThrough,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecorationStyle {
    Solid,
    Double,
    Dotted,
    Dashed,
    Wavy,
}

/// A computed color in its original color space. The components are
/// unpremultiplied; alpha is stored separately.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintColor {
    pub space: ColorSpace,
    pub components: [f32; 3],
    pub alpha: f32,
}

impl PaintColor {
    pub const TRANSPARENT: Self = Self::srgb(0.0, 0.0, 0.0, 0.0);
    pub const BLACK: Self = Self::srgb(0.0, 0.0, 0.0, 1.0);

    pub const fn srgb(red: f32, green: f32, blue: f32, alpha: f32) -> Self {
        Self {
            space: ColorSpace::Srgb,
            components: [red, green, blue],
            alpha,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorSpace {
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    A98Rgb,
    ProphotoRgb,
    Rec2020,
    XyzD50,
    XyzD65,
    Hsl,
    Hwb,
    Lab,
    Lch,
    Oklab,
    Oklch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LengthPercentage {
    pub px: f32,
    /// A multiplier of the reference length, where 1.0 represents 100%.
    pub fraction: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Position2D {
    pub x: LengthPercentage,
    pub y: LengthPercentage,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PositionKind {
    #[default]
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum DisplayKind {
    None,
    Contents,
    Block,
    #[default]
    Inline,
    InlineBlock,
    Flex,
    InlineFlex,
    Grid,
    InlineGrid,
    ListItem,
    Table,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Visibility {
    #[default]
    Visible,
    Hidden,
    Collapse,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Containment {
    pub size: bool,
    pub inline_size: bool,
    pub layout: bool,
    pub style: bool,
    pub paint: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StackingValues {
    pub position: PositionKind,
    pub display: DisplayKind,
    pub visibility: Visibility,
    pub z_index: Option<i32>,
    pub isolation: bool,
    pub floating: bool,
    pub flex_or_grid_item: bool,
    pub containment: Containment,
    pub top_layer_order: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BorderStyle {
    #[default]
    None,
    Hidden,
    Solid,
    Dotted,
    Dashed,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderSide {
    pub style: BorderStyle,
    pub color: PaintColor,
}

impl Default for BorderSide {
    fn default() -> Self {
        Self {
            style: BorderStyle::None,
            color: PaintColor::TRANSPARENT,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Outline {
    pub width: f32,
    pub offset: f32,
    pub side: BorderSide,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Overflow {
    #[default]
    Visible,
    Hidden,
    Clip,
    Scroll,
    Auto,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverscrollBehavior {
    #[default]
    Auto,
    Contain,
    None,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BoxPaintValues {
    /// This background has been propagated to the viewport and is drawn there.
    pub background_propagated: bool,
    pub background_clip: BackgroundClip,
    pub backgrounds: Box<[BackgroundLayer]>,
    /// Elliptical corner radii in top-left, top-right, bottom-right and bottom-left order.
    pub radii: [Position2D; 4],
    pub border: Edges<BorderSide>,
    pub outline: Outline,
    pub overflow: [Overflow; 2],
    pub overscroll: [OverscrollBehavior; 2],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BackgroundClip {
    #[default]
    Border,
    Padding,
    Content,
    Text,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GeometryBox {
    Border,
    Padding,
    Content,
    Fill,
    Stroke,
    View,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Repeat {
    Repeat,
    Space,
    Round,
    NoRepeat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ImageSize {
    Explicit([Option<LengthPercentage>; 2]),
    Contain,
    Cover,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundLayer {
    pub image: BackgroundImage,
    pub origin: GeometryBox,
    pub clip: BackgroundClip,
    pub size: ImageSize,
    pub position: Position2D,
    pub repeat: [Repeat; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub enum BackgroundImage {
    Image(ImageKey),
    Gradient(Arc<Gradient>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HueInterpolation {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub geometry: GradientGeometry,
    pub space: ColorSpace,
    pub hue: HueInterpolation,
    pub stops: Box<[GradientStop]>,
    pub repeating: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientGeometry {
    Linear {
        angle_radians: f32,
    },
    LinearTo {
        horizontal: i8,
        vertical: i8,
    },
    Radial {
        center: Position2D,
        extent: RadialExtent,
        circle: bool,
    },
    Conic {
        center: Position2D,
        from_radians: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RadialExtent {
    Explicit(Position2D),
    ClosestSide,
    FarthestSide,
    ClosestCorner,
    FarthestCorner,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StopPosition {
    Length(LengthPercentage),
    AngleRadians(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientStop {
    Color {
        position: Option<StopPosition>,
        color: PaintColor,
    },
    Hint(StopPosition),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub offset: Point,
    pub blur: f32,
    pub spread: f32,
    pub color: PaintColor,
    pub inset: bool,
}

/// Computed CSS `image-rendering` for replaced images.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ImageRendering {
    #[default]
    Auto,
    Smooth,
    HighQuality,
    Pixelated,
    CrispEdges,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ObjectFit {
    #[default]
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArtValues {
    pub color: PaintColor,
    pub accent_color: Option<PaintColor>,
    pub background_color: PaintColor,
    pub box_shadows: Box<[Shadow]>,
    pub text_shadows: Box<[Shadow]>,
    pub object_fit: ObjectFit,
    pub object_position: Position2D,
}

impl Default for ArtValues {
    fn default() -> Self {
        Self {
            color: PaintColor::BLACK,
            accent_color: None,
            background_color: PaintColor::TRANSPARENT,
            box_shadows: Box::default(),
            text_shadows: Box::default(),
            object_fit: ObjectFit::Fill,
            object_position: Position2D {
                x: LengthPercentage {
                    px: 0.0,
                    fraction: 0.5,
                },
                y: LengthPercentage {
                    px: 0.0,
                    fraction: 0.5,
                },
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    Move(Point),
    Line(Point),
    Quad {
        control: Point,
        to: Point,
    },
    Cubic {
        control1: Point,
        control2: Point,
        to: Point,
    },
    Close,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClipShape {
    Inset {
        edges: Edges<LengthPercentage>,
        radii: [Position2D; 4],
    },
    Ellipse {
        center: Position2D,
        radii: Position2D,
    },
    Polygon {
        points: Box<[Position2D]>,
        fill_rule: FillRule,
    },
    Path {
        commands: Arc<[PathCommand]>,
        fill_rule: FillRule,
    },
    Box,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub reference_box: GeometryBox,
    pub shape: ClipShape,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SvgPaint {
    Color(PaintColor),
    Gradient(Arc<Gradient>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SvgStroke {
    pub paint: SvgPaint,
    pub width: f32,
    pub dash_array: Box<[f32]>,
    pub dash_offset: f32,
    pub line_cap: LineCap,
    pub line_join: LineJoin,
    pub miter_limit: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SvgPath {
    pub commands: Arc<[PathCommand]>,
    pub fill: Option<SvgPaint>,
    pub fill_rule: FillRule,
    pub stroke: Option<SvgStroke>,
    pub clip: Option<Clip>,
    pub transform: [[f32; 4]; 4],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SvgGeometry {
    pub paths: Box<[SvgPath]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transform {
    /// Column-major matrix.
    Matrix([[f32; 4]; 4]),
    Translate {
        xy: Position2D,
        z: f32,
    },
    /// `rotate()`, `rotateX()`, `rotateY()`, `rotateZ()` and `rotate3d()`:
    /// `angle` radians about the unit vector `axis`, clockwise on screen for
    /// a positive angle about z. Animation keyframes keep the angle, because
    /// as a matrix `rotate(360deg)` is the same as no rotation at all.
    Rotate {
        axis: [f32; 3],
        angle: f32,
    },
    /// `scale()`, `scaleX()`, `scaleY()`, `scaleZ()` and `scale3d()`.
    Scale {
        x: f32,
        y: f32,
        z: f32,
    },
    /// `skew()`, `skewX()` and `skewY()`, in radians.
    Skew {
        x: f32,
        y: f32,
    },
}

impl Transform {
    /// The column-major matrix of this function in a reference box of `size`,
    /// using the CSS Transforms formulas.
    pub fn matrix(&self, size: crate::Size) -> [[f32; 4]; 4] {
        let mut m = [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        match *self {
            Self::Matrix(matrix) => return matrix,
            Self::Translate { xy, z } => {
                m[3] = [
                    xy.x.px + xy.x.fraction * size.width,
                    xy.y.px + xy.y.fraction * size.height,
                    z,
                    1.0,
                ];
            }
            Self::Rotate {
                axis: [x, y, z],
                angle,
            } => {
                let (s, c) = angle.sin_cos();
                let t = 1.0 - c;
                m[0] = [t * x * x + c, t * x * y + s * z, t * x * z - s * y, 0.0];
                m[1] = [t * x * y - s * z, t * y * y + c, t * y * z + s * x, 0.0];
                m[2] = [t * x * z + s * y, t * y * z - s * x, t * z * z + c, 0.0];
            }
            Self::Scale { x, y, z } => {
                m[0][0] = x;
                m[1][1] = y;
                m[2][2] = z;
            }
            Self::Skew { x, y } => {
                m[0][1] = y.tan();
                m[1][0] = x.tan();
            }
        }
        m
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Filter {
    Blur(f32),
    Brightness(f32),
    Contrast(f32),
    DropShadow(Shadow),
    Grayscale(f32),
    HueRotate(f32),
    Invert(f32),
    Opacity(f32),
    Saturate(f32),
    Sepia(f32),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
    PlusLighter,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AnimatedValue {
    Opacity(f32),
    Transform(Box<[Transform]>),
    Filter(Box<[Filter]>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Easing {
    Linear,
    CubicBezier([f32; 4]),
    Steps {
        count: u32,
        jump_start: bool,
        jump_end: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnimationKeyframe {
    pub offset: f32,
    pub value: AnimatedValue,
    pub easing: Easing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompositorAnimation {
    pub id: u64,
    pub start: std::time::Instant,
    pub delay_seconds: f64,
    pub duration: std::time::Duration,
    pub iterations: f64,
    pub direction: AnimationDirection,
    pub fill: AnimationFill,
    pub keyframes: Box<[AnimationKeyframe]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationDirection {
    Normal,
    Reverse,
    Alternate,
    AlternateReverse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AnimationFill {
    None,
    Forwards,
    Backwards,
    Both,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropertyValues {
    pub transform: Box<[Transform]>,
    pub transform_origin: Position2D,
    pub transform_origin_z: f32,
    pub transform_box: GeometryBox,
    pub perspective: Option<f32>,
    pub perspective_origin: Position2D,
    pub preserve_3d: bool,
    pub clip: Option<Clip>,
    pub opacity: f32,
    pub filters: Box<[Filter]>,
    pub backdrop_filters: Box<[Filter]>,
    pub blend: BlendMode,
    pub animations: Box<[CompositorAnimation]>,
}

impl Default for PropertyValues {
    fn default() -> Self {
        let center = LengthPercentage {
            px: 0.0,
            fraction: 0.5,
        };
        let origin = Position2D {
            x: center,
            y: center,
        };
        Self {
            transform: Box::default(),
            transform_origin: origin,
            transform_origin_z: 0.0,
            transform_box: GeometryBox::Border,
            perspective: None,
            perspective_origin: origin,
            preserve_3d: false,
            clip: None,
            opacity: 1.0,
            filters: Box::default(),
            backdrop_filters: Box::default(),
            blend: BlendMode::Normal,
            animations: Box::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PointerEvents {
    #[default]
    Auto,
    None,
    VisiblePainted,
    VisibleFill,
    VisibleStroke,
    Visible,
    Painted,
    Fill,
    Stroke,
    All,
    BoundingBox,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Cursor {
    #[default]
    Auto,
    Default,
    None,
    ContextMenu,
    Help,
    Pointer,
    Progress,
    Wait,
    Cell,
    Crosshair,
    Text,
    VerticalText,
    Alias,
    Copy,
    Move,
    NoDrop,
    NotAllowed,
    Grab,
    Grabbing,
    EResize,
    NResize,
    NeResize,
    NwResize,
    SResize,
    SeResize,
    SwResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ColResize,
    RowResize,
    AllScroll,
    ZoomIn,
    ZoomOut,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InteractionValues {
    pub pointer_events: PointerEvents,
    pub cursor: Cursor,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaintValues {
    pub image_rendering: ImageRendering,
    pub stacking: StackingValues,
    pub box_paint: BoxPaintValues,
    pub art: ArtValues,
    pub properties: PropertyValues,
    pub interaction: InteractionValues,
}
