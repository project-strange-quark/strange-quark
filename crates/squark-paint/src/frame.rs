// SPDX-License-Identifier: MIT

use std::{collections::HashSet, sync::Arc, time::Instant};

use color::{ColorSpaceTag as Tag, DynamicColor};
use squark_types::{
    NodeId, Point, Rect, Size,
    render::{FrameId, GlyphKey, ImageKey, PaintError, ResourceSeq, Viewport},
};

use crate::{
    ClipShape, FillRule, HitRegion, HitTestTree, PropertyRefs, PropertyTrees, ScrollAxis, ScrollId,
    hit::element_target,
    input::*,
    properties::PropertyBuilder,
    stacking::{PaintFragment, Phase, paint_order},
};

#[derive(Clone, Debug)]
pub struct Frame {
    pub display_list: Arc<Vec<DisplayItem>>,
    pub property_trees: PropertyTrees,
    pub hit_test: HitTestTree,
    pub resource_updates: ResourceSeq,
    pub damage: Damage,
    /// Native IME candidate area in viewport CSS coordinates.
    pub ime_area: Option<Rect>,
}

#[derive(Clone, Debug)]
pub struct CommittedFrame {
    pub id: FrameId,
    pub viewport: Viewport,
    pub frame: Frame,
    pub publication: Option<FramePublication>,
}

#[derive(Clone, Copy, Debug)]
pub struct FramePublication {
    pub sequence: u64,
    pub paint_started: Instant,
    pub committed: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Damage {
    Full,
    Rects { base: FrameId, rects: Box<[Rect]> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ItemId(pub FragmentId, pub u32);

#[derive(Debug, PartialEq)]
pub struct DisplayItem {
    pub id: ItemId,
    pub bounds: Rect,
    pub properties: PropertyRefs,
    pub primitive: Primitive,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoundedRect {
    pub rect: Rect,
    /// Clockwise from the top left.
    pub radii: [Size; 4],
}

impl RoundedRect {
    pub fn new(rect: Rect, mut radii: [Size; 4]) -> Self {
        // A zero radius on either axis makes the corner square. Reduce all
        // remaining radii by the same factor if adjoining corners overlap.
        for radius in &mut radii {
            if radius.width <= 0.0 || radius.height <= 0.0 {
                *radius = Size::default();
            }
        }
        let [tl, tr, br, bl] = radii;
        let side_lengths = [
            (rect.size.width, tl.width + tr.width),
            (rect.size.width, bl.width + br.width),
            (rect.size.height, tl.height + bl.height),
            (rect.size.height, tr.height + br.height),
        ];
        let scale = side_lengths
            .into_iter()
            .filter(|(_, sum)| *sum > 0.0)
            .fold(1.0_f32, |scale, (side, sum)| scale.min(side.max(0.0) / sum));
        if scale < 1.0 {
            for radius in &mut radii {
                radius.width *= scale;
                radius.height *= scale;
            }
        }
        Self { rect, radii }
    }

    pub fn inset(self, [top, right, bottom, left]: [f32; 4]) -> Self {
        let edges = [(left, top), (right, top), (right, bottom), (left, bottom)];
        Self::new(
            Rect::new(
                self.rect.origin.x + left,
                self.rect.origin.y + top,
                (self.rect.size.width - left - right).max(0.0),
                (self.rect.size.height - top - bottom).max(0.0),
            ),
            std::array::from_fn(|i| Size {
                width: (self.radii[i].width - edges[i].0).max(0.0),
                height: (self.radii[i].height - edges[i].1).max(0.0),
            }),
        )
    }
}

/// Straight alpha. Components are in the color space named by the value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaintColor {
    pub space: ColorSpace,
    pub rgba: [f32; 4],
}

impl PaintColor {
    pub fn dynamic(self) -> DynamicColor {
        let mut components = self.rgba;
        if self.space == ColorSpace::DisplayP3Linear {
            let [r, g, b, _] = components;
            components[..3].copy_from_slice(&[
                0.48657095 * r + 0.2656677 * g + 0.19821729 * b,
                0.22897457 * r + 0.69173855 * g + 0.07928691 * b,
                0.04511338 * g + 1.0439444 * b,
            ]);
        }
        DynamicColor {
            cs: self.space.tag(),
            flags: Default::default(),
            components,
        }
    }

    pub const fn srgb(rgba: [f32; 4]) -> Self {
        Self {
            space: ColorSpace::Srgb,
            rgba,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpace {
    Srgb,
    SrgbLinear,
    DisplayP3,
    DisplayP3Linear,
    A98Rgb,
    ProPhotoRgb,
    Rec2020,
    XyzD50,
    XyzD65,
    Lab,
    Lch,
    Oklab,
    Oklch,
    Hsl,
    Hwb,
}

impl ColorSpace {
    pub fn tag(self) -> Tag {
        match self {
            ColorSpace::Srgb => Tag::Srgb,
            ColorSpace::SrgbLinear => Tag::LinearSrgb,
            ColorSpace::DisplayP3 => Tag::DisplayP3,
            ColorSpace::A98Rgb => Tag::A98Rgb,
            ColorSpace::ProPhotoRgb => Tag::ProphotoRgb,
            ColorSpace::Rec2020 => Tag::Rec2020,
            ColorSpace::XyzD50 => Tag::XyzD50,
            ColorSpace::XyzD65 | ColorSpace::DisplayP3Linear => Tag::XyzD65,
            ColorSpace::Lab => Tag::Lab,
            ColorSpace::Lch => Tag::Lch,
            ColorSpace::Oklab => Tag::Oklab,
            ColorSpace::Oklch => Tag::Oklch,
            ColorSpace::Hsl => Tag::Hsl,
            ColorSpace::Hwb => Tag::Hwb,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
    pub width: f32,
    pub style: BorderStyle,
    pub color: PaintColor,
}

impl BorderSide {
    fn visible(self) -> bool {
        self.width > 0.0
            && self.color.rgba[3] > 0.0
            && !matches!(self.style, BorderStyle::None | BorderStyle::Hidden)
    }
}

impl Default for BorderSide {
    fn default() -> Self {
        Self {
            width: 0.0,
            style: BorderStyle::None,
            color: PaintColor::srgb([0.0; 4]),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shadow {
    pub offset: Point,
    pub blur: f32,
    pub spread: f32,
    pub color: PaintColor,
    pub inset: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GradientGeometry {
    Linear { start: Point, end: Point },
    Radial { center: Point, radii: Size },
    Conic { center: Point, start_angle: f32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HueInterpolation {
    #[default]
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub geometry: GradientGeometry,
    pub interpolation: ColorSpace,
    pub hue: HueInterpolation,
    pub stops: Box<[(f32, PaintColor)]>,
    pub repeating: bool,
    pub hints: Box<[Option<f32>]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageSource {
    Decoded(ImageKey),
    Canvas { id: NodeId, version: ResourceSeq },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplingMode {
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageQuad {
    pub source: ImageSource,
    pub src: Rect,
    pub dst: Rect,
    pub sampling: SamplingMode,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    pub color: PaintColor,
    pub glyphs: Box<[(GlyphKey, Point)]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PathBrush {
    Color(PaintColor),
    Gradient(Arc<Gradient>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct PathPaint {
    pub path: Arc<kurbo::BezPath>,
    pub fill: Option<(PathBrush, FillRule)>,
    pub stroke: Option<(PathBrush, kurbo::Stroke)>,
}

#[derive(Debug, PartialEq)]
pub enum Primitive {
    RoundedRect {
        shape: RoundedRect,
        color: PaintColor,
    },
    Border {
        shape: RoundedRect,
        sides: [BorderSide; 4],
    },
    Scrollbar {
        scroll: ScrollId,
        axis: ScrollAxis,
    },
    Shadow {
        shape: RoundedRect,
        shadow: Shadow,
    },
    Image(ImageQuad),
    BackgroundImage {
        shape: RoundedRect,
        origin: Rect,
        layer: squark_types::fragment::BackgroundLayer,
    },
    Backdrop,
    GradientRamp {
        shape: RoundedRect,
        key: squark_types::render::GradientKey,
        geometry: GradientGeometry,
        stop_range: [f32; 2],
        repeating: bool,
    },
    GlyphRun(GlyphRun),
    Path(PathPaint),
}

pub(crate) fn build_resolved_frame(
    tree: &FragmentTree,
    mirror: &dyn MirrorRead,
    resource_updates: ResourceSeq,
) -> Result<Frame, PaintError> {
    let (mut properties, fragments) = prepare(tree)?;
    let mut display_list = Vec::new();
    let mut hit_regions = Vec::new();
    paint_order(&fragments, tree, |index, phase| {
        let fragment = &fragments[index];
        if !fragment.fragment.style.visible {
            return;
        }
        let mut painter = ItemBuilder::new(fragment, phase, &mut properties, &mut display_list);
        match phase {
            Phase::Decoration => {
                painter.decoration();
                if let Some(region) = build_hit_region(fragment, mirror, &properties) {
                    hit_regions.push(region);
                }
            }
            Phase::Content => painter.content(resource_updates),
            Phase::Outline => painter.outline(),
        }
    });
    Ok(Frame {
        display_list: Arc::new(display_list),
        property_trees: properties.finish(),
        hit_test: HitTestTree {
            regions: hit_regions.into(),
        },
        resource_updates,
        damage: Damage::Full,
        ime_area: None,
    })
}

/// Rebuild the synchronous hit-test view from the current layout snapshot.
pub(crate) fn build_resolved_hit_test(
    tree: &FragmentTree,
    mirror: &dyn MirrorRead,
) -> Result<(HitTestTree, PropertyTrees), PaintError> {
    let (properties, fragments) = prepare(tree)?;
    let mut regions = Vec::new();
    paint_order(&fragments, tree, |index, phase| {
        let fragment = &fragments[index];
        if matches!(phase, Phase::Decoration)
            && fragment.fragment.style.visible
            && let Some(region) = build_hit_region(fragment, mirror, &properties)
        {
            regions.push(region);
        }
    });
    Ok((
        HitTestTree {
            regions: regions.into(),
        },
        properties.finish(),
    ))
}

fn prepare(tree: &FragmentTree) -> Result<(PropertyBuilder, Vec<PaintFragment<'_>>), PaintError> {
    let mut properties = PropertyBuilder::new();
    let mut fragments = Vec::new();
    let mut visited = HashSet::new();
    let mut pending = vec![(tree.root, None)];
    while let Some((id, parent)) = pending.pop() {
        let fragment = tree
            .nodes
            .get(&id)
            .ok_or(PaintError::MissingFragment(id.0))?;
        if fragment.id != id || fragment.parent != parent || !visited.insert(id) {
            return Err(PaintError::InvalidFragment(id.0));
        }
        if fragment.style.display == Display::None {
            continue;
        }
        let inherited = parent.map_or(PropertyRefs::default(), |id| properties.descendants(id));
        let (decoration, content) = properties.add(fragment, inherited);
        fragments.push(PaintFragment {
            fragment,
            decoration,
            content,
        });
        pending.extend(
            fragment
                .children
                .iter()
                .rev()
                .map(|child| (*child, Some(id))),
        );
    }
    Ok((properties, fragments))
}

fn build_hit_region(
    paint: &PaintFragment<'_>,
    mirror: &dyn MirrorRead,
    properties: &PropertyBuilder,
) -> Option<HitRegion> {
    let fragment = paint.fragment;
    if matches!(fragment.kind, FragmentKind::Viewport | FragmentKind::Line) {
        return None;
    }
    let target = element_target(mirror, fragment.owner)?;
    Some(HitRegion {
        target,
        props: paint.decoration,
        shape: RoundedRect::new(fragment.border_box, fragment.style.radii),
        cursor: fragment.style.cursor,
        pointer_events: fragment.style.pointer_events,
        listeners: crate::hit::listeners(mirror, target),
        scroll_chain: properties.scroll_chain(paint.content.scroll),
    })
}

struct ItemBuilder<'a, 'tree> {
    fragment: &'tree Fragment,
    list: &'a mut Vec<DisplayItem>,
    properties: &'a mut PropertyBuilder,
    refs: PropertyRefs,
    slot: u32,
    decoration: RoundedRect,
}

impl<'a, 'tree> ItemBuilder<'a, 'tree> {
    fn new(
        paint: &PaintFragment<'tree>,
        phase: Phase,
        properties: &'a mut PropertyBuilder,
        list: &'a mut Vec<DisplayItem>,
    ) -> Self {
        let fragment = paint.fragment;
        let (slot, refs) = match phase {
            Phase::Decoration => (0, paint.decoration),
            Phase::Content => (1, paint.content),
            Phase::Outline => (2, paint.decoration),
        };
        Self {
            fragment,
            list,
            properties,
            refs,
            slot,
            decoration: RoundedRect::new(fragment.border_box, fragment.style.radii),
        }
    }

    fn push(&mut self, bounds: Rect, primitive: Primitive) {
        self.push_with(self.refs, bounds, primitive);
    }

    fn push_with(&mut self, properties: PropertyRefs, bounds: Rect, primitive: Primitive) {
        if bounds.size.width > 0.0 && bounds.size.height > 0.0 {
            self.list.push(DisplayItem {
                id: ItemId(self.fragment.id, self.slot),
                bounds,
                properties,
                primitive,
            });
        }
        self.slot += 3; // Disjoint stable paint slots for decoration, content and outline.
    }

    fn area(&self, area: BoxArea) -> RoundedRect {
        match area {
            BoxArea::Border => self.decoration,
            BoxArea::Padding => self.decoration.inset(self.fragment.border),
            BoxArea::Content | BoxArea::Text => self
                .decoration
                .inset(add_edges(self.fragment.border, self.fragment.padding)),
        }
    }

    fn decoration(&mut self) {
        let style = &self.fragment.style;
        if matches!(self.fragment.kind, FragmentKind::Line | FragmentKind::Text) {
            return;
        }
        let shape = self.decoration;
        if !style.backdrop.is_empty() {
            self.push(shape.rect, Primitive::Backdrop);
        }
        for &shadow in style.shadows.iter().rev().filter(|s| !s.inset) {
            let extent = shadow.spread + shadow.blur * 2.0;
            let mut bounds = shape.inset([-extent; 4]).rect;
            bounds.origin.x += shadow.offset.x;
            bounds.origin.y += shadow.offset.y;
            self.push(bounds, Primitive::Shadow { shape, shadow });
        }
        if !style.background_propagated {
            self.background();
        }
        for &shadow in style.shadows.iter().rev().filter(|s| s.inset) {
            let shape = self.area(BoxArea::Padding);
            self.push(shape.rect, Primitive::Shadow { shape, shadow });
        }
        if style.border.iter().any(|side| side.visible()) {
            self.push(
                shape.rect,
                Primitive::Border {
                    shape,
                    sides: style.border,
                },
            );
        }
    }

    fn background_area(&self, area: BoxArea) -> RoundedRect {
        if self.fragment.kind == FragmentKind::Viewport {
            RoundedRect::new(self.fragment.border_box, [Size::default(); 4])
        } else {
            self.area(area)
        }
    }

    fn background(&mut self) {
        let style = &self.fragment.style;
        let background = self.background_area(style.background_clip);
        if style.background.rgba[3] > 0.0 && style.background_clip != BoxArea::Text {
            self.push(
                background.rect,
                Primitive::RoundedRect {
                    shape: background,
                    color: style.background,
                },
            );
        }
        for layer in style.background_layers.iter().rev() {
            if layer.area == BoxArea::Text {
                continue;
            }
            let shape = self.background_area(layer.area);
            self.push(
                shape.rect,
                Primitive::BackgroundImage {
                    shape,
                    origin: layer.origin,
                    layer: layer.layer.clone(),
                },
            );
        }
    }

    fn content(&mut self, resource_updates: ResourceSeq) {
        let fragment = self.fragment;
        match &fragment.content {
            FragmentContent::None => {}
            FragmentContent::Text(runs) => {
                for &shadow in fragment.style.text_shadows.iter().rev() {
                    let refs =
                        self.properties
                            .add_text_shadow(self.refs, fragment.border_box, shadow);
                    self.text(runs, refs, Some(PaintColor::srgb([0.0, 0.0, 0.0, 1.0])));
                }
                self.text(runs, self.refs, None);
            }
            FragmentContent::Image {
                key,
                intrinsic_size,
            } => {
                self.replaced(ImageSource::Decoded(*key), *intrinsic_size);
            }
            FragmentContent::Canvas { id, bitmap_size } => {
                self.replaced(
                    ImageSource::Canvas {
                        id: *id,
                        version: resource_updates,
                    },
                    Size {
                        width: bitmap_size[0] as f32,
                        height: bitmap_size[1] as f32,
                    },
                );
            }
            FragmentContent::Svg(paths) => {
                for path in paths.as_ref() {
                    let mut refs = self.refs;
                    if let Some(local) = path.transform {
                        refs = self.properties.add_transform(refs, fragment.owner, local);
                    }
                    if let Some(shape) = &path.clip {
                        refs = self.properties.add_clip(refs, shape.clone());
                    }
                    self.push_with(
                        refs,
                        fragment.border_box,
                        Primitive::Path(path.paint.clone()),
                    );
                }
            }
        }
    }

    fn text(&mut self, runs: &[PositionedRun], refs: PropertyRefs, mask: Option<PaintColor>) {
        for run in runs {
            self.text_lines(&run.under_and_overlines, refs, mask);
            self.push_with(
                refs,
                self.fragment.border_box,
                Primitive::GlyphRun(GlyphRun {
                    color: mask.unwrap_or(self.fragment.style.text_color),
                    glyphs: run.glyphs.clone(),
                }),
            );
            self.text_lines(&run.line_through, refs, mask);
        }
    }

    fn text_lines(
        &mut self,
        lines: &[(Rect, PaintColor)],
        refs: PropertyRefs,
        mask: Option<PaintColor>,
    ) {
        for &(rect, color) in lines {
            self.push_with(
                refs,
                rect,
                Primitive::RoundedRect {
                    shape: RoundedRect::new(rect, [Size::default(); 4]),
                    color: mask.unwrap_or(color),
                },
            );
        }
    }

    fn replaced(&mut self, source: ImageSource, intrinsic: Size) {
        let content = self.area(BoxArea::Content);
        let bounds = content.rect.size;
        if intrinsic.width <= 0.0
            || intrinsic.height <= 0.0
            || bounds.width <= 0.0
            || bounds.height <= 0.0
        {
            return;
        }
        let sx = bounds.width / intrinsic.width;
        let sy = bounds.height / intrinsic.height;
        let scale = match self.fragment.style.object_fit {
            ObjectFit::Fill => None,
            ObjectFit::Contain => Some(sx.min(sy)),
            ObjectFit::Cover => Some(sx.max(sy)),
            ObjectFit::None => Some(1.0),
            ObjectFit::ScaleDown => Some(1.0_f32.min(sx).min(sy)),
        };
        let size = scale.map_or(bounds, |scale| Size {
            width: intrinsic.width * scale,
            height: intrinsic.height * scale,
        });
        let offset = self.fragment.style.object_position;
        let dst = Rect::new(
            content.rect.origin.x
                + (bounds.width - size.width) * offset.x
                + self.fragment.style.object_position_offset.x,
            content.rect.origin.y
                + (bounds.height - size.height) * offset.y
                + self.fragment.style.object_position_offset.y,
            size.width,
            size.height,
        );
        if let Some(bounds) = dst.intersection(&content.rect) {
            let refs = self
                .properties
                .add_clip(self.refs, ClipShape::RoundedRect(content));
            self.push_with(
                refs,
                bounds,
                Primitive::Image(ImageQuad {
                    source,
                    src: Rect::new(0.0, 0.0, intrinsic.width, intrinsic.height),
                    dst,
                    sampling: self.fragment.style.image_sampling,
                }),
            );
        }
    }

    fn outline(&mut self) {
        self.scrollbars();
        if !matches!(self.fragment.kind, FragmentKind::Line | FragmentKind::Text)
            && let Some(outline) = self.fragment.style.outline
            && outline.side.visible()
        {
            let extent = outline.side.width + outline.offset;
            let shape = self.decoration.inset([-extent; 4]);
            self.push(
                shape.rect,
                Primitive::Border {
                    shape,
                    sides: [outline.side; 4],
                },
            );
        }
    }

    fn scrollbars(&mut self) {
        let Some(frame) = self.fragment.scroll else {
            return;
        };
        let [top, right, bottom, left] = frame.gutter;
        let horizontal_gutter = if bottom > 0.0 { bottom } else { top };
        let vertical_gutter = if right > 0.0 { right } else { left };
        let scroll = self.properties.descendants(self.fragment.id).scroll;
        let port = frame.scrollport;
        let content = frame.content;
        let horizontal = frame.wheel_axes[0]
            && horizontal_gutter > 0.0
            && (frame.force_scrollbars[0]
                || content.origin.x < port.origin.x
                || content.max_x() > port.max_x());
        let vertical = frame.wheel_axes[1]
            && vertical_gutter > 0.0
            && (frame.force_scrollbars[1]
                || content.origin.y < port.origin.y
                || content.max_y() > port.max_y());
        if horizontal {
            let y = if bottom > 0.0 {
                port.max_y() - horizontal_gutter
            } else {
                port.origin.y
            };
            let occupied = if vertical { vertical_gutter } else { 0.0 };
            let x = if right > 0.0 { 0.0 } else { occupied };
            self.push(
                Rect::new(
                    port.origin.x + x,
                    y,
                    port.size.width - occupied,
                    horizontal_gutter,
                ),
                Primitive::Scrollbar {
                    scroll,
                    axis: ScrollAxis::Horizontal,
                },
            );
        }
        if vertical {
            let x = if right > 0.0 {
                port.max_x() - vertical_gutter
            } else {
                port.origin.x
            };
            let occupied = if horizontal { horizontal_gutter } else { 0.0 };
            let y = if bottom > 0.0 { 0.0 } else { occupied };
            self.push(
                Rect::new(
                    x,
                    port.origin.y + y,
                    vertical_gutter,
                    port.size.height - occupied,
                ),
                Primitive::Scrollbar {
                    scroll,
                    axis: ScrollAxis::Vertical,
                },
            );
        }
    }
}

fn add_edges(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    std::array::from_fn(|i| a[i] + b[i])
}
