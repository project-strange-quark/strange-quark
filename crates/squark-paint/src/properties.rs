// SPDX-License-Identifier: MIT

use std::{collections::BTreeMap, sync::Arc};

use squark_types::{NodeId, Point, Rect};

use crate::{AnimationTarget, RoundedRect, animation::affects_transform, input::*};

/// Column-major, multiplying column vectors.
pub type Matrix = [[f32; 4]; 4];

pub(crate) const IDENTITY: Matrix = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

pub(crate) fn multiply(a: Matrix, b: Matrix) -> Matrix {
    std::array::from_fn(|col| {
        std::array::from_fn(|row| (0..4).map(|axis| a[axis][row] * b[col][axis]).sum())
    })
}

pub(crate) fn translation(x: f32, y: f32, z: f32) -> Matrix {
    let mut matrix = IDENTITY;
    matrix[3] = [x, y, z, 1.0];
    matrix
}

pub(crate) fn with_origin(matrix: Matrix, [x, y, z]: [f32; 3]) -> Matrix {
    if matrix == IDENTITY {
        return IDENTITY;
    }
    multiply(
        multiply(translation(x, y, z), matrix),
        translation(-x, -y, -z),
    )
}

macro_rules! property_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        pub struct $name(pub u32);

        impl $name {
            fn next(len: usize) -> Self {
                Self(u32::try_from(len).expect("property tree cannot exceed u32 indices"))
            }
        }
    };
}

property_id!(TransformId);
property_id!(ClipId);
property_id!(EffectId);
property_id!(ScrollId);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PropertyRefs {
    pub transform: TransformId,
    pub clip: ClipId,
    pub effect: EffectId,
    pub scroll: ScrollId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    NonZero,
    EvenOdd,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ClipShape {
    Rect(Rect),
    RoundedRect(RoundedRect),
    Path(Arc<kurbo::BezPath>, FillRule),
    /// Overflow clips only the named axes in the clip's space.
    AxisAligned {
        rect: Rect,
        axes: [bool; 2],
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    Blur(f32),
    Brightness(f32),
    Contrast(f32),
    Grayscale(f32),
    HueRotate(f32),
    Invert(f32),
    Opacity(f32),
    Saturate(f32),
    Sepia(f32),
    DropShadow(crate::Shadow),
    TextShadow(crate::Shadow),
}

impl From<squark_types::fragment::Filter> for Filter {
    fn from(value: squark_types::fragment::Filter) -> Self {
        crate::shared::filter(value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TransformNode {
    pub fragment: Option<FragmentId>,
    pub parent: TransformId,
    pub owner: NodeId,
    pub local: Matrix,
    /// Offset introduced at this node. Descendants inherit it in their matrix.
    pub scroll: ScrollId,
    pub sticky: Option<StickyNode>,
    pub preserve_3d: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StickyNode {
    pub scroll: ScrollId,
    pub normal_rect: Rect,
    pub containing_rect: Rect,
    pub insets: [Option<f32>; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct ClipNode {
    pub parent: ClipId,
    pub space: TransformId,
    pub shape: Option<ClipShape>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EffectNode {
    pub parent: EffectId,
    pub bounds: RoundedRect,
    pub space: TransformId,
    pub clip: ClipId,
    pub opacity: f32,
    pub filters: Box<[Filter]>,
    pub backdrop: Box<[Filter]>,
    pub blend: BlendMode,
    pub isolated: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScrollNode {
    pub parent: ScrollId,
    pub owner: NodeId,
    pub space: TransformId,
    pub scrollport: Rect,
    pub overflow: Rect,
    pub initial_offset: Point,
    pub generation: u64,
    pub axes: [bool; 2],
    pub wheel_axes: [bool; 2],
    pub overscroll: [OverscrollBehavior; 2],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollAxis {
    Horizontal,
    Vertical,
}

impl ScrollNode {
    pub fn offset_limits(&self) -> [[f32; 2]; 2] {
        [
            [
                (self.overflow.origin.x - self.scrollport.origin.x).min(0.0),
                (self.overflow.max_x() - self.scrollport.max_x()).max(0.0),
            ],
            [
                (self.overflow.origin.y - self.scrollport.origin.y).min(0.0),
                (self.overflow.max_y() - self.scrollport.max_y()).max(0.0),
            ],
        ]
    }

    /// Map the compositor offset to a thumb within a classic scrollbar track.
    pub fn scrollbar_thumb(&self, axis: ScrollAxis, mut track: Rect) -> Rect {
        let [min, max] = self.offset_limits()[axis as usize];
        let (length, origin, viewport, offset) = if axis == ScrollAxis::Horizontal {
            (
                &mut track.size.width,
                &mut track.origin.x,
                self.scrollport.size.width,
                self.initial_offset.x,
            )
        } else {
            (
                &mut track.size.height,
                &mut track.origin.y,
                self.scrollport.size.height,
                self.initial_offset.y,
            )
        };
        let range = max - min;
        let size = (*length * viewport / (viewport + range)).clamp((*length).min(12.0), *length);
        if range > 0.0 {
            *origin += (*length - size) * ((offset - min) / range).clamp(0.0, 1.0);
        }
        *length = size;
        track
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PropertyTrees {
    pub transforms: Arc<[TransformNode]>,
    pub clips: Arc<[ClipNode]>,
    pub effects: Arc<[EffectNode]>,
    pub scrolls: Box<[ScrollNode]>,
    pub animations: Arc<[AnimationTarget]>,
}

#[derive(Debug)]
pub(crate) struct PropertyBuilder {
    transforms: Vec<TransformNode>,
    clips: Vec<ClipNode>,
    /// The fragment that introduced an overflow clip, if any.
    overflow_clips: Vec<Option<FragmentId>>,
    effects: Vec<EffectNode>,
    scrolls: Vec<ScrollNode>,
    animations: Vec<AnimationTarget>,
    descendants: BTreeMap<FragmentId, PropertyRefs>,
}

impl PropertyBuilder {
    pub fn new() -> Self {
        Self {
            transforms: vec![TransformNode {
                fragment: None,
                parent: TransformId(0),
                owner: NodeId::NONE,
                local: IDENTITY,
                scroll: ScrollId(0),
                sticky: None,
                preserve_3d: false,
            }],
            clips: vec![ClipNode {
                parent: ClipId(0),
                space: TransformId(0),
                shape: None,
            }],
            overflow_clips: vec![None],
            effects: vec![EffectNode {
                parent: EffectId(0),
                bounds: RoundedRect::new(Rect::default(), Default::default()),
                space: TransformId(0),
                clip: ClipId(0),
                opacity: 1.0,
                filters: Box::new([]),
                backdrop: Box::new([]),
                blend: BlendMode::Normal,
                isolated: false,
            }],
            scrolls: vec![ScrollNode {
                parent: ScrollId(0),
                owner: NodeId::NONE,
                space: TransformId(0),
                scrollport: Rect::default(),
                overflow: Rect::default(),
                initial_offset: Point::default(),
                generation: 0,
                axes: [false; 2],
                wheel_axes: [false; 2],
                overscroll: [OverscrollBehavior::Auto; 2],
            }],
            descendants: BTreeMap::new(),
            animations: Vec::new(),
        }
    }

    fn transform(&mut self, node: TransformNode) -> TransformId {
        let id = TransformId::next(self.transforms.len());
        self.transforms.push(node);
        id
    }

    fn clip(&mut self, node: ClipNode, overflow_owner: Option<FragmentId>) -> ClipId {
        let id = ClipId::next(self.clips.len());
        self.clips.push(node);
        self.overflow_clips.push(overflow_owner);
        id
    }

    /// Skip overflow clips between an out-of-flow box and its containing block.
    /// Clip-path and effect ancestry still follows the paint tree.
    fn out_of_flow_clip(&mut self, clip: ClipId, containing: ClipId) -> ClipId {
        if clip == containing || clip == ClipId(0) {
            return clip;
        }
        let mut node = self.clips[clip.0 as usize].clone();
        let parent = self.out_of_flow_clip(node.parent, containing);
        if self.overflow_clips[clip.0 as usize].is_some() {
            return parent;
        }
        if parent == node.parent {
            return clip;
        }
        node.parent = parent;
        self.clip(node, None)
    }

    pub fn add(
        &mut self,
        fragment: &Fragment,
        inherited: PropertyRefs,
    ) -> (PropertyRefs, PropertyRefs) {
        let style = &fragment.style;
        let mut decoration = inherited;
        if matches!(style.position, Position::Absolute | Position::Fixed) {
            let containing = fragment
                .containing_block
                .and_then(|id| self.descendants.get(&id))
                .copied()
                .unwrap_or_default();
            decoration.transform = containing.transform;
            decoration.scroll = containing.scroll;
            decoration.clip = self.out_of_flow_clip(inherited.clip, containing.clip);
        }
        if style.top_layer.is_some() {
            decoration = PropertyRefs::default();
        }
        let mut origin = style.transform_origin;
        origin[0] += fragment.border_box.origin.x;
        origin[1] += fragment.border_box.origin.y;
        let animated_transform = style.animations.iter().any(affects_transform);
        if style.transform.is_some()
            || animated_transform
            || fragment.sticky.is_some()
            || style.preserve_3d
        {
            let local = with_origin(style.transform.unwrap_or(IDENTITY), origin);
            decoration.transform = self.transform(TransformNode {
                fragment: Some(fragment.id),
                parent: decoration.transform,
                owner: fragment.owner,
                local,
                scroll: ScrollId(0),
                sticky: fragment.sticky.and_then(|constraints| {
                    self.find_scroll_owner(decoration.scroll, constraints.scroll_owner)
                        .map(|scroll| StickyNode {
                            scroll,
                            normal_rect: fragment.border_box,
                            containing_rect: constraints.containing_rect,
                            insets: constraints.insets,
                        })
                }),
                preserve_3d: style.preserve_3d,
            });
        }
        if let Some(shape) = &style.clip {
            decoration.clip = self.clip(
                ClipNode {
                    parent: decoration.clip,
                    space: decoration.transform,
                    shape: Some(shape.clone()),
                },
                None,
            );
        }
        if style.has_effect() {
            let parent = decoration.effect;
            decoration.effect = EffectId::next(self.effects.len());
            self.effects.push(EffectNode {
                parent,
                bounds: RoundedRect::new(fragment.border_box, fragment.style.radii),
                space: decoration.transform,
                clip: decoration.clip,
                opacity: style.opacity,
                filters: style.filters.clone(),
                backdrop: style.backdrop.clone(),
                blend: style.blend,
                isolated: style.isolation,
            });
        }

        if !style.animations.is_empty() {
            self.animations.push(AnimationTarget::new(
                style,
                origin,
                animated_transform.then(|| {
                    (
                        decoration.transform,
                        self.transforms[decoration.transform.0 as usize].local,
                    )
                }),
                style.has_effect().then_some(decoration.effect),
            ));
        }

        let mut content = decoration;
        if let Some(scroll) = fragment.scroll {
            content = self.scroll_content(fragment, scroll, content);
        }
        let mut descendants = content;
        if let Some(local) = style.perspective {
            descendants.transform = self.transform(TransformNode {
                fragment: Some(fragment.id),
                parent: content.transform,
                owner: fragment.owner,
                local,
                scroll: ScrollId(0),
                sticky: None,
                preserve_3d: style.preserve_3d,
            });
        }
        self.descendants.insert(fragment.id, descendants);
        (decoration, content)
    }

    pub fn add_transform(
        &mut self,
        mut refs: PropertyRefs,
        owner: NodeId,
        local: Matrix,
    ) -> PropertyRefs {
        refs.transform = self.transform(TransformNode {
            fragment: None,
            parent: refs.transform,
            owner,
            local,
            scroll: ScrollId(0),
            sticky: None,
            preserve_3d: false,
        });
        refs
    }

    pub fn add_text_shadow(
        &mut self,
        mut refs: PropertyRefs,
        rect: Rect,
        shadow: crate::Shadow,
    ) -> PropertyRefs {
        let parent = refs.effect;
        refs.effect = EffectId::next(self.effects.len());
        self.effects.push(EffectNode {
            parent,
            bounds: RoundedRect::new(rect, Default::default()),
            space: refs.transform,
            clip: refs.clip,
            opacity: 1.0,
            filters: Box::new([Filter::TextShadow(shadow)]),
            backdrop: Box::new([]),
            blend: BlendMode::Normal,
            isolated: true,
        });
        refs
    }

    pub fn add_clip(&mut self, mut refs: PropertyRefs, shape: ClipShape) -> PropertyRefs {
        refs.clip = self.clip(
            ClipNode {
                parent: refs.clip,
                space: refs.transform,
                shape: Some(shape),
            },
            None,
        );
        refs
    }

    pub fn scroll_chain(&self, mut scroll: ScrollId) -> Box<[NodeId]> {
        let mut chain = Vec::new();
        while scroll != ScrollId(0) {
            let node = &self.scrolls[scroll.0 as usize];
            chain.push(node.owner);
            scroll = node.parent;
        }
        chain.into_boxed_slice()
    }

    fn find_scroll_owner(&self, mut scroll: ScrollId, owner: NodeId) -> Option<ScrollId> {
        while scroll != ScrollId(0) {
            let node = &self.scrolls[scroll.0 as usize];
            if node.owner == owner {
                return Some(scroll);
            }
            scroll = node.parent;
        }
        None
    }

    pub fn descendants(&self, id: FragmentId) -> PropertyRefs {
        self.descendants[&id]
    }

    fn scroll_content(
        &mut self,
        fragment: &Fragment,
        scroll: ScrollFrame,
        mut refs: PropertyRefs,
    ) -> PropertyRefs {
        let clip_axes = [scroll.clip_x, scroll.clip_y];
        if clip_axes.contains(&true) {
            let padding_box =
                RoundedRect::new(fragment.border_box, fragment.style.radii).inset(fragment.border);
            let shape = if scroll.clip_x && scroll.clip_y {
                ClipShape::RoundedRect(RoundedRect::new(scroll.scrollport, padding_box.radii))
            } else {
                ClipShape::AxisAligned {
                    rect: scroll.scrollport,
                    axes: clip_axes,
                }
            };
            refs.clip = self.clip(
                ClipNode {
                    parent: refs.clip,
                    space: refs.transform,
                    shape: Some(shape),
                },
                Some(fragment.id),
            );
        }
        let axes = [scroll.scroll_x, scroll.scroll_y];
        if axes.contains(&true) {
            let id = ScrollId::next(self.scrolls.len());
            self.scrolls.push(ScrollNode {
                parent: refs.scroll,
                owner: scroll.owner,
                space: refs.transform,
                scrollport: scroll.scrollport,
                overflow: scroll.content,
                initial_offset: scroll.offset,
                generation: scroll.generation,
                axes,
                wheel_axes: scroll.wheel_axes,
                overscroll: fragment.style.overscroll,
            });
            refs.scroll = id;
            refs.transform = self.transform(TransformNode {
                fragment: Some(fragment.id),
                parent: refs.transform,
                owner: scroll.owner,
                local: IDENTITY,
                scroll: id,
                sticky: None,
                preserve_3d: fragment.style.preserve_3d,
            });
        }
        refs
    }

    pub fn finish(self) -> PropertyTrees {
        PropertyTrees {
            transforms: self.transforms.into(),
            clips: self.clips.into(),
            effects: self.effects.into(),
            scrolls: self.scrolls.into_boxed_slice(),
            animations: self.animations.into(),
        }
    }
}
