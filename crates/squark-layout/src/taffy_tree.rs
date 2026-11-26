// SPDX-License-Identifier: MIT
use std::{rc::Rc, slice};

use style::Atom;
use taffy::prelude::*;
use taffy::{
    BlockContainerStyle, BlockContext, BoxGenerationMode, CacheTree, CalcHandle, Contain,
    ContainingBlockClaims, CoreStyle, DetailedLayoutInfo, FlexboxContainerStyle,
    GridContainerStyle, Layout, LayoutBlockContainer, LayoutContainingBlock,
    LayoutFlexboxContainer, LayoutGridContainer, LayoutInput, LayoutOutput, LayoutPartialTree,
    MaybeResolve, Overflow, TraversePartialTree,
};

use crate::{BoxTree, Content};

type CssStyle = Style<Atom>;
type Id = taffy::NodeId;

#[derive(Clone, Copy)]
pub(crate) struct Container<'a> {
    style: &'a CssStyle,
    claims: ContainingBlockClaims,
}

macro_rules! forward {
    ($trait:ident; $($name:ident -> $result:ty),+ $(,)?) => {
        $(fn $name(&self) -> $result { $trait::$name(self.style) })+
    };
}

impl CoreStyle for Container<'_> {
    type CustomIdent = Atom;
    fn is_containing_block(&self) -> ContainingBlockClaims {
        self.claims
    }

    forward!(CoreStyle;
        box_generation_mode -> BoxGenerationMode,
        is_block -> bool,
        is_compressible_replaced -> bool,
        is_replaced -> bool,
        box_sizing -> taffy::BoxSizing,
        direction -> taffy::Direction,
        overflow -> taffy::Point<Overflow>,
        scrollbar_width -> f32,
        position -> Position,
        inset -> Rect<LengthPercentageAuto>,
        size -> Size<Dimension>,
        min_size -> Size<LengthPercentageAuto>,
        max_size -> Size<LengthPercentageAuto>,
        aspect_ratio -> Option<f32>,
        margin -> Rect<LengthPercentageAuto>,
        padding -> Rect<LengthPercentage>,
        border -> Rect<LengthPercentage>,
        contain -> Contain,
    );
}

impl BlockContainerStyle for Container<'_> {
    forward!(BlockContainerStyle;
        text_align -> taffy::TextAlign,
        align_content -> AlignContent,
        justify_items -> AlignItems,
    );
}

impl FlexboxContainerStyle for Container<'_> {
    forward!(FlexboxContainerStyle;
        flex_direction -> FlexDirection,
        flex_wrap -> FlexWrap,
        flex_line_count -> u16,
        gap -> Size<LengthPercentage>,
        align_content -> AlignContent,
        align_items -> AlignItems,
        justify_content -> JustifyContent,
    );
}

impl GridContainerStyle for Container<'_> {
    type Repetition<'a>
        = <CssStyle as GridContainerStyle>::Repetition<'a>
    where
        Self: 'a;
    type TemplateTrackList<'a>
        = <CssStyle as GridContainerStyle>::TemplateTrackList<'a>
    where
        Self: 'a;
    type AutoTrackList<'a>
        = <CssStyle as GridContainerStyle>::AutoTrackList<'a>
    where
        Self: 'a;
    type TemplateLineNames<'a>
        = <CssStyle as GridContainerStyle>::TemplateLineNames<'a>
    where
        Self: 'a;
    type GridTemplateAreas<'a>
        = <CssStyle as GridContainerStyle>::GridTemplateAreas<'a>
    where
        Self: 'a;

    forward!(GridContainerStyle;
        grid_template_rows -> Option<Self::TemplateTrackList<'_>>,
        grid_template_columns -> Option<Self::TemplateTrackList<'_>>,
        grid_auto_rows -> Self::AutoTrackList<'_>,
        grid_auto_columns -> Self::AutoTrackList<'_>,
        grid_template_areas -> Option<Self::GridTemplateAreas<'_>>,
        grid_template_area_row_count -> u16,
        grid_template_area_column_count -> u16,
        grid_template_column_names -> Option<Self::TemplateLineNames<'_>>,
        grid_template_row_names -> Option<Self::TemplateLineNames<'_>>,
        grid_auto_flow -> GridAutoFlow,
        gap -> Size<LengthPercentage>,
        align_content -> AlignContent,
        align_items -> AlignItems,
        justify_content -> JustifyContent,
        justify_items -> AlignItems,
    );
}

impl TraversePartialTree for BoxTree<'_> {
    type ChildIter<'a>
        = std::iter::Copied<slice::Iter<'a, Id>>
    where
        Self: 'a;

    fn child_ids(&self, id: Id) -> Self::ChildIter<'_> {
        self[id].children.iter().copied()
    }
    fn child_count(&self, id: Id) -> usize {
        self[id].children.len()
    }
    fn get_child_id(&self, id: Id, child: usize) -> Id {
        self[id].children[child]
    }
}

impl LayoutPartialTree for BoxTree<'_> {
    type CoreContainerStyle<'a>
        = Container<'a>
    where
        Self: 'a;
    type CustomIdent = Atom;

    fn get_core_container_style(&self, id: Id) -> Self::CoreContainerStyle<'_> {
        Container {
            style: &self[id].style,
            claims: self[id].claims,
        }
    }
    fn resolve_calc_value(&self, handle: CalcHandle, basis: f32) -> f32 {
        self.calcs.resolve(handle, basis)
    }
    fn set_unrounded_layout(&mut self, id: Id, layout: &Layout) {
        self[id].layout = *layout;
    }
    fn compute_child_layout(&mut self, id: Id, input: LayoutInput) -> LayoutOutput {
        self.compute(id, input, None)
    }
}

impl BoxTree<'_> {
    fn compute(
        &mut self,
        id: Id,
        input: LayoutInput,
        block: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        taffy::compute_cached_layout(self, id, input, |tree, id, input| {
            let display = tree[id].style.display;
            if display == Display::None || input.run_mode == taffy::RunMode::PerformHiddenLayout {
                return taffy::compute_hidden_layout(tree, id);
            }
            let mut output = match tree[id].content {
                Content::Inline(_) => tree.inline_layout(id, input),
                Content::InlineEdge(_) => unreachable!("inline edges are measured by Parley"),
                Content::Replaced(intrinsic, ref replacement) => {
                    let has_only_svg_ratio = matches!(replacement, crate::Replacement::Svg)
                        && intrinsic.width.is_none()
                        && intrinsic.height.is_none()
                        && intrinsic.ratio.is_some();
                    let calcs = Rc::clone(&tree.calcs);
                    let style = &tree[id].style;
                    let resolve = |handle, basis| calcs.resolve(handle, basis);
                    let specified = style.size.maybe_resolve(input.parent_size, resolve);
                    let constrained = input.known_dimensions.or(specified);
                    taffy::compute_leaf_layout(input, style, resolve, |_, available| {
                        let known = Size {
                            width: constrained
                                .width
                                .and_then(|_| available.width.into_option()),
                            height: constrained
                                .height
                                .and_then(|_| available.height.into_option()),
                        };
                        // A ratio-only inline SVG has no intrinsic contribution
                        // when its percentage width is unresolved.
                        if has_only_svg_ratio && known == Size::NONE {
                            Size::ZERO
                        } else {
                            crate::replaced_size(intrinsic, known)
                        }
                    })
                }
                Content::Control(_) => crate::controls::compute(tree, id, input),
                Content::Table(ref table) => {
                    crate::table::compute(tree, id, input, table.spacing, table.auto_columns)
                }
                Content::Container => match display {
                    Display::Flex => tree.compute_flex(id, input),
                    Display::Grid => taffy::compute_grid_layout(tree, id, input),
                    _ => taffy::compute_block_layout(tree, id, input, block),
                },
            };
            if input.run_mode == taffy::RunMode::PerformLayout {
                taffy::compute_oof_layout(tree, id, &mut output);
                if matches!(display, Display::Block | Display::FlowRoot)
                    && matches!(tree[id].content, Content::Container)
                    && !tree[id].style.contain.suppresses_baseline()
                {
                    output.baselines.last = tree[id].children.iter().rev().find_map(|&child| {
                        let child = &tree[child];
                        if child.style.position.is_out_of_flow()
                            || child.style.overflow.y != taffy::Overflow::Visible
                        {
                            return None;
                        }
                        child
                            .baselines
                            .last
                            .or(child.baselines.first)
                            .map(|baseline| child.layout.location.y + baseline)
                    });
                }
                tree[id].baselines = output.baselines;
            }
            output
        })
    }
    fn compute_flex(&mut self, id: Id, input: LayoutInput) -> LayoutOutput {
        use taffy::{AvailableSpace, RunMode, SizingMode};

        let result = taffy::compute_flexbox_layout(self, id, input);
        let style = &self[id].style;
        if !style.position.is_out_of_flow()
            || !style.size.width.is_auto()
            || input.sizing_mode != SizingMode::ContentSize
            || input.run_mode != RunMode::ComputeSize
            || input.known_dimensions.width.is_some()
        {
            return result;
        }
        let AvailableSpace::Definite(available) = input.available_space.width else {
            return result;
        };
        if result.size.width <= available {
            return result;
        }
        // An auto-width out-of-flow flex box uses fit-content sizing when
        // its insets leave an automatic side. Taffy's definite-space
        // flex pass can keep the unwrapped preferred width in this case.
        let min = taffy::compute_flexbox_layout(
            self,
            id,
            LayoutInput {
                available_space: Size {
                    width: AvailableSpace::MinContent,
                    ..input.available_space
                },
                ..input
            },
        );
        let width = min.size.width.max(available);
        if width >= result.size.width {
            return result;
        }
        taffy::compute_flexbox_layout(
            self,
            id,
            LayoutInput {
                known_dimensions: Size {
                    width: Some(width),
                    ..input.known_dimensions
                },
                ..input
            },
        )
    }
}

impl CacheTree for BoxTree<'_> {
    fn cache_get(&self, id: Id, input: &LayoutInput) -> Option<LayoutOutput> {
        self[id].cache.get(input)
    }
    fn cache_store(&mut self, id: Id, input: &LayoutInput, output: LayoutOutput) {
        self[id].cache.store(input, output);
    }
    fn cache_clear(&mut self, id: Id) {
        self[id].cache.clear();
    }
}

impl LayoutContainingBlock for BoxTree<'_> {
    type OofItemStyle<'a>
        = &'a CssStyle
    where
        Self: 'a;

    fn get_oof_item_style(&self, id: Id) -> &CssStyle {
        &self[id].style
    }
    fn clear_hoisted_children(&mut self, id: Id) {
        self[id].hoisted.clear();
    }
    fn add_hoisted_children(&mut self, id: Id, children: &[Id]) {
        self[id].hoisted.extend_from_slice(children);
    }
    fn get_detailed_layout_info(&self, id: Id) -> &DetailedLayoutInfo<Atom> {
        &self[id].details
    }
}

impl LayoutBlockContainer for BoxTree<'_> {
    type BlockContainerStyle<'a>
        = Container<'a>
    where
        Self: 'a;
    type BlockItemStyle<'a>
        = &'a CssStyle
    where
        Self: 'a;

    fn get_block_container_style(&self, id: Id) -> Container<'_> {
        self.get_core_container_style(id)
    }
    fn get_block_child_style(&self, id: Id) -> &CssStyle {
        &self[id].style
    }
    fn compute_block_child_layout(
        &mut self,
        id: Id,
        input: LayoutInput,
        block: Option<&mut BlockContext<'_>>,
    ) -> LayoutOutput {
        self.compute(id, input, block)
    }
}

impl LayoutFlexboxContainer for BoxTree<'_> {
    type FlexboxContainerStyle<'a>
        = Container<'a>
    where
        Self: 'a;
    type FlexboxItemStyle<'a>
        = &'a CssStyle
    where
        Self: 'a;

    fn get_flexbox_container_style(&self, id: Id) -> Container<'_> {
        self.get_core_container_style(id)
    }
    fn get_flexbox_child_style(&self, id: Id) -> &CssStyle {
        &self[id].style
    }
}

impl LayoutGridContainer for BoxTree<'_> {
    type GridContainerStyle<'a>
        = Container<'a>
    where
        Self: 'a;
    type GridItemStyle<'a>
        = &'a CssStyle
    where
        Self: 'a;

    fn get_grid_container_style(&self, id: Id) -> Container<'_> {
        self.get_core_container_style(id)
    }
    fn get_grid_child_style(&self, id: Id) -> &CssStyle {
        &self[id].style
    }
    fn set_detailed_grid_info(&mut self, id: Id, info: taffy::DetailedGridInfo<Atom>) {
        self[id].details = DetailedLayoutInfo::Grid(Box::new(info));
    }
}
