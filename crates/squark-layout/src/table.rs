// SPDX-License-Identifier: MIT

use std::{collections::BTreeMap, ops::Range};

use squark_types::{NodeId, Rect as CssRect, fragment::FragmentId};
use taffy::{
    AvailableSpace, BoxSizing, Dimension, GridPlacement, LayoutInput, LayoutOutput,
    LayoutPartialTree, LengthPercentage, MaybeMath, MaybeResolve, Rect, ResolveOrZero, RunMode,
    Size, SizingMode,
    style_helpers::{TaffyAuto, length},
};

use crate::{BoxTree, Id};

#[derive(Debug)]
pub(crate) struct Table {
    pub(crate) spacing: Size<f32>,
    pub(crate) auto_columns: Option<usize>,
    pub(crate) parts: Vec<Part>,
}

pub(crate) type Parts = BTreeMap<NodeId, (FragmentId, CssRect)>;

#[derive(Debug)]
pub(crate) struct Part {
    pub(crate) owner: NodeId,
    pub(crate) rows: Range<usize>,
}

pub(crate) fn record_parts(
    parts: &[Part],
    grid: &taffy::DetailedGridInfo<style::Atom>,
    table: FragmentId,
    border_box: CssRect,
    output: &mut Parts,
) {
    let horizontal = &grid.columns.positions;
    let left = horizontal
        .iter()
        .map(|col| col.start)
        .reduce(f32::min)
        .or(grid.columns.empty_axis_line)
        .unwrap_or_default();
    let right = horizontal
        .iter()
        .map(|col| col.end)
        .reduce(f32::max)
        .unwrap_or(left);
    let vertical = &grid.rows.positions;
    let bottom = vertical
        .last()
        .map(|row| row.end)
        .or(grid.rows.empty_axis_line)
        .unwrap_or_default();
    for part in parts {
        let top = vertical
            .get(part.rows.start)
            .map_or(bottom, |row| row.start);
        let end = if part.rows.is_empty() {
            top
        } else {
            vertical
                .get(part.rows.end - 1)
                .map_or(bottom, |row| row.end)
        };
        output.insert(
            part.owner,
            (
                table,
                CssRect::new(
                    border_box.origin.x + left,
                    border_box.origin.y + top,
                    right - left,
                    end - top,
                ),
            ),
        );
    }
}

/// Tables use a Taffy grid for placement. Separate borders leave one spacing
/// slot around the outside as well as the ordinary gaps between tracks.
pub(crate) fn compute(
    tree: &mut BoxTree<'_>,
    id: Id,
    input: LayoutInput,
    spacing: Size<f32>,
    auto_columns: Option<usize>,
) -> LayoutOutput {
    let originals = tree[id].style.padding;
    let calcs = std::rc::Rc::clone(&tree.calcs);
    let resolve = |handle, basis| calcs.resolve(handle, basis);
    let padding = originals.resolve_or_zero(input.parent_size.width, resolve);
    let extended_padding = Rect {
        left: padding.left + spacing.width,
        right: padding.right + spacing.width,
        top: padding.top + spacing.height,
        bottom: padding.bottom + spacing.height,
    };
    let border = tree[id]
        .style
        .border
        .resolve_or_zero(input.parent_size.width, resolve);
    let inset = extended_padding.horizontal_axis_sum() + border.horizontal_axis_sum();
    let columns = auto_columns.and_then(|count| {
        let style = &tree[id].style;
        // Explicitly sized cells keep the grid approximation. The proportional
        // pass is for unconstrained columns.
        if tree[id]
            .children
            .iter()
            .any(|&child| tree[child].style.size.width != Dimension::AUTO)
        {
            return None;
        }
        let adjustment = match style.box_sizing {
            BoxSizing::BorderBox => 0.0,
            BoxSizing::ContentBox => inset,
        };
        let resolve_width = |dimension: Dimension| {
            dimension
                .maybe_resolve(input.parent_size.width, resolve)
                .map(|width| width + adjustment)
        };
        let min = resolve_width(style.min_size.width.into());
        let max = resolve_width(style.max_size.width.into());
        let preferred = input.known_dimensions.width.or_else(|| {
            (input.sizing_mode == SizingMode::InherentSize)
                .then(|| resolve_width(style.size.width))
                .flatten()
        });
        let gaps = spacing.width * count.saturating_sub(1) as f32;
        let extras = inset + gaps;
        let definite = preferred.map(|width| width.maybe_clamp(min, max));
        let available = input.available_space.width;
        let mut contributions = vec![[0.0f32; 2]; count];
        let children = tree[id].children.clone();
        for child in children {
            let GridPlacement::Line(line) = tree[child].style.grid_column.start else {
                continue;
            };
            let index = line.as_i16().saturating_sub(1) as usize;
            let Some(contribution) = contributions.get_mut(index) else {
                continue;
            };
            for (slot, available) in contribution
                .iter_mut()
                .zip([AvailableSpace::MinContent, AvailableSpace::MaxContent])
            {
                let output = tree.compute_child_layout(
                    child,
                    LayoutInput {
                        run_mode: RunMode::ComputeSize,
                        available_space: Size {
                            width: available,
                            height: AvailableSpace::MaxContent,
                        },
                        ..LayoutInput::HIDDEN
                    },
                );
                *slot = slot.max(output.size.width);
            }
        }
        let [min_total, max_total] = contributions.iter().fold([0.0, 0.0], |total, [min, max]| {
            [total[0] + min, total[1] + max]
        });
        let requested = match (definite, available) {
            (Some(width), _) => width - extras,
            (None, AvailableSpace::MinContent) => min_total,
            (None, AvailableSpace::MaxContent) => max_total,
            (None, AvailableSpace::Definite(width)) => (width - extras).min(max_total),
        };
        let attempted = (requested + extras).maybe_clamp(min, max) - extras;
        let width = attempted.max(min_total);
        Some(
            contributions
                .into_iter()
                .map(|[min, max]| {
                    if width <= max_total && max_total > min_total {
                        let proportion = (width - min_total) / (max_total - min_total);
                        length(min + proportion * (max - min))
                    } else if max_total > 0.0 {
                        length(width * max / max_total)
                    } else {
                        length(width / count as f32)
                    }
                })
                .collect(),
        )
    });
    let style = &mut tree[id].style;
    style.padding = extended_padding.map(LengthPercentage::length);
    let old_columns =
        columns.map(|columns| std::mem::replace(&mut style.grid_template_columns, columns));
    let output = taffy::compute_grid_layout(tree, id, input);
    let style = &mut tree[id].style;
    style.padding = originals;
    if let Some(columns) = old_columns {
        style.grid_template_columns = columns;
    }
    output
}
