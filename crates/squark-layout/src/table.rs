// SPDX-License-Identifier: MIT
//! Table layout (CSS 2.1 § 17, CSS Tables 3) as Chromium implements it.
//!
//! Columns are sized from their cells' min- and max-content contributions,
//! rows from their cells' heights, and every cell box fills its rows while
//! `vertical-align` places the cell's content inside it. A table's baseline
//! is its first row's baseline.

use std::{collections::BTreeMap, ops::Range, rc::Rc};

use squark_types::{NodeId, Rect as CssRect, fragment::FragmentId};
use taffy::{
    AvailableSpace, Baselines, BoxSizing, Dimension, Layout, LayoutInput, LayoutOutput,
    LayoutPartialTree, LengthPercentageAuto, Line, MaybeMath, MaybeResolve, OofCandidates,
    OofPositioningArea, Overflow, Point, Rect, RequestedAxis, ResolveOrZero, RunMode,
    ScrollableOverflow, Size, SizingMode, style::ExpandedDimension,
};

use crate::{BoxTree, Content, Id};

#[derive(Debug)]
pub(crate) struct Table {
    /// `border-spacing`, or zero when borders collapse.
    pub(crate) spacing: Size<f32>,
    /// `table-layout: fixed`, which applies only when the table's width is
    /// not `auto`.
    pub(crate) fixed: bool,
    pub(crate) rows: Vec<Row>,
    /// One cell for each child box of the table, in the same order.
    pub(crate) cells: Vec<Cell>,
    pub(crate) columns: usize,
    pub(crate) parts: Vec<Part>,
    /// Row edges and the column extent of the last full layout, relative to
    /// the table's border box.
    pub(crate) tracks: Tracks,
}

#[derive(Debug)]
pub(crate) struct Row {
    /// The row's own `height`, a minimum for the row.
    pub(crate) height: Dimension,
}

/// A cell's place in the table grid and the size properties that the table
/// resolves itself. The cell's box style keeps automatic sizes, so laying
/// the box out measures its content.
#[derive(Debug)]
pub(crate) struct Cell {
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) rows: usize,
    pub(crate) columns: usize,
    pub(crate) align: Align,
    pub(crate) size: Size<Dimension>,
    pub(crate) min_size: Size<LengthPercentageAuto>,
    pub(crate) max_width: LengthPercentageAuto,
}

/// A cell's `vertical-align`. Every value other than `top`, `middle` and
/// `bottom` aligns the cell's baseline with its row's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Top,
    Middle,
    Bottom,
    Baseline,
}

#[derive(Debug, Default)]
pub(crate) struct Tracks {
    rows: Vec<Line<f32>>,
    /// The left and right edges of the columns.
    columns: [f32; 2],
    end: f32,
}

pub(crate) type Parts = BTreeMap<NodeId, (FragmentId, CssRect)>;

#[derive(Debug)]
pub(crate) struct Part {
    pub(crate) owner: NodeId,
    pub(crate) rows: Range<usize>,
}

/// Records the client rects of rows and row groups, which have no boxes of
/// their own: each spans the table's columns over its rows.
pub(crate) fn record_parts(table: &Table, id: FragmentId, border_box: CssRect, output: &mut Parts) {
    let tracks = &table.tracks;
    for part in &table.parts {
        let top = tracks
            .rows
            .get(part.rows.start)
            .map_or(tracks.end, |row| row.start);
        let end = if part.rows.is_empty() {
            top
        } else {
            tracks
                .rows
                .get(part.rows.end - 1)
                .map_or(tracks.end, |row| row.end)
        };
        output.insert(
            part.owner,
            (
                id,
                CssRect::new(
                    border_box.origin.x + tracks.columns[0],
                    border_box.origin.y + top,
                    tracks.columns[1] - tracks.columns[0],
                    end - top,
                ),
            ),
        );
    }
}

/// A column's width constraints, merged from the cells in it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Column {
    min: f32,
    max: f32,
    percent: Option<f32>,
    /// Whether a cell in the column has a fixed width.
    fixed: bool,
}

impl Column {
    /// Merges a single-column cell's constraint. A column with a fixed-width
    /// cell takes its maximum from fixed-width cells only.
    fn encompass(&mut self, cell: Column) {
        self.min = self.min.max(cell.min);
        match (self.fixed, cell.fixed) {
            (true, false) => {}
            (false, true) => {
                self.max = cell.max;
                self.fixed = true;
            }
            _ => self.max = self.max.max(cell.max),
        }
        self.max = self.max.max(self.min);
        self.percent = match (self.percent, cell.percent) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// What the table needs from a cell's content-sized layout.
#[derive(Clone, Copy, Debug, Default)]
struct Measure {
    /// The border-box height of the content.
    content: f32,
    /// The cell's `height` and `min-height` as a border-box minimum.
    specified: f32,
    /// The first baseline from the cell's top, or its content box bottom.
    baseline: f32,
    /// The cell's bottom padding and border.
    end: f32,
}

pub(crate) fn compute(tree: &mut BoxTree<'_>, id: Id, input: LayoutInput) -> LayoutOutput {
    let Content::Table(mut table) = std::mem::replace(&mut tree[id].content, Content::Container)
    else {
        unreachable!("table layout expected")
    };
    let output = layout(tree, id, &mut table, input);
    tree[id].content = Content::Table(table);
    output
}

fn layout(tree: &mut BoxTree<'_>, id: Id, table: &mut Table, input: LayoutInput) -> LayoutOutput {
    let calcs = Rc::clone(&tree.calcs);
    let resolve = |handle, basis| calcs.resolve(handle, basis);
    let style = &tree[id].style;
    let parent = input.parent_size;
    let padding = style.padding.resolve_or_zero(parent.width, resolve);
    let border = style.border.resolve_or_zero(parent.width, resolve);
    let inset = Rect {
        left: padding.left + border.left,
        right: padding.right + border.right,
        top: padding.top + border.top,
        bottom: padding.bottom + border.bottom,
    };
    let inherent = input.sizing_mode == SizingMode::InherentSize;
    // Sizes below are border-box sizes.
    let (adjust_width, adjust_height) = match style.box_sizing {
        BoxSizing::ContentBox => (inset.left + inset.right, inset.top + inset.bottom),
        BoxSizing::BorderBox => (0.0, 0.0),
    };
    let width_of = |value: Option<f32>| value.map(|width| width + adjust_width);
    let height_of = |value: Option<f32>| value.map(|height| height + adjust_height);
    let css_width = width_of(
        inherent
            .then(|| style.size.width.maybe_resolve(parent.width, resolve))
            .flatten(),
    );
    let min_width = width_of(style.min_size.width.maybe_resolve(parent.width, resolve));
    let max_width = width_of(style.max_size.width.maybe_resolve(parent.width, resolve));
    let css_height = height_of(
        inherent
            .then(|| style.size.height.maybe_resolve(parent.height, resolve))
            .flatten(),
    );
    let min_height = height_of(style.min_size.height.maybe_resolve(parent.height, resolve));
    let max_height = height_of(style.max_size.height.maybe_resolve(parent.height, resolve));
    let rtl = style.direction.is_rtl();
    let overflow = style.overflow;
    let contain = style.contain;
    let spacing = table.spacing;
    let column_count = table.columns;
    let row_count = table.rows.len();
    let inline_spacing = if column_count > 0 {
        spacing.width * (column_count + 1) as f32
    } else {
        0.0
    };
    let block_spacing = if row_count > 0 {
        spacing.height * (row_count + 1) as f32
    } else {
        0.0
    };
    let children = tree[id].children.clone();

    // Column widths.
    let fixed_layout = table.fixed && css_width.is_some();
    let columns = column_constraints(tree, table, &children, fixed_layout);
    let extras = inline_spacing + inset.left + inset.right;
    let min_sum: f32 = columns.iter().map(|column| column.min).sum();
    let table_min = min_sum + extras;
    let table_max = max_content(&columns).max(min_sum) + extras;
    let width = input.known_dimensions.width.unwrap_or_else(|| {
        css_width
            .unwrap_or(match input.available_space.width {
                AvailableSpace::MinContent => table_min,
                AvailableSpace::MaxContent => table_max,
                AvailableSpace::Definite(available) => table_max.min(available),
            })
            .maybe_clamp(min_width, max_width)
            .max(table_min)
    });
    if input.run_mode == RunMode::ComputeSize && input.axis == RequestedAxis::Horizontal {
        return LayoutOutput::from_outer_size(Size { width, height: 0.0 });
    }
    let grid_width = width.max(table_min);
    let target = (grid_width - extras).max(0.0);
    let widths = if fixed_layout {
        distribute_fixed(&columns, target)
    } else {
        distribute_auto(&columns, target)
    };
    let mut lefts = Vec::with_capacity(column_count);
    if rtl {
        let mut edge = grid_width - inset.right - spacing.width;
        for width in &widths {
            lefts.push(edge - width);
            edge -= width + spacing.width;
        }
    } else {
        let mut edge = inset.left + spacing.width;
        for width in &widths {
            lefts.push(edge);
            edge += width + spacing.width;
        }
    }
    let span_width = |cell: &Cell| {
        let range = cell.column..(cell.column + cell.columns).min(column_count);
        widths[range.clone()].iter().sum::<f32>()
            + spacing.width * range.len().saturating_sub(1) as f32
    };

    // Row heights from the cells' content laid out at their widths.
    let mut heights: Vec<f32> = table
        .rows
        .iter()
        .map(|row| row.height.maybe_resolve(None, resolve).unwrap_or(0.0).max(0.0))
        .collect();
    let mut measures = Vec::with_capacity(table.cells.len());
    for (cell, &child) in table.cells.iter().zip(&children) {
        let width = span_width(cell);
        // Size-only results are cached without baselines, so cells on the
        // baseline are measured with a full layout.
        let run_mode = if cell.align == Align::Baseline {
            RunMode::PerformLayout
        } else {
            RunMode::ComputeSize
        };
        let output = tree.compute_child_layout(
            child,
            LayoutInput {
                run_mode,
                sizing_mode: SizingMode::InherentSize,
                axis: RequestedAxis::Both,
                known_dimensions: Size {
                    width: Some(width),
                    height: None,
                },
                known_dimensions_are_definite: Size {
                    width: true,
                    height: true,
                },
                parent_size: Size {
                    width: Some(width),
                    height: None,
                },
                available_space: Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::MaxContent,
                },
                vertical_margins_are_collapsible: Line::FALSE,
            },
        );
        let style = &tree[child].style;
        let padding = style.padding.resolve_or_zero(Some(width), resolve);
        let border = style.border.resolve_or_zero(Some(width), resolve);
        let adjustment = match style.box_sizing {
            BoxSizing::ContentBox => padding.top + padding.bottom + border.top + border.bottom,
            BoxSizing::BorderBox => 0.0,
        };
        let specified = [
            cell.size.height.maybe_resolve(None, resolve),
            cell.min_size.height.maybe_resolve(None, resolve),
        ]
        .into_iter()
        .flatten()
        .map(|height| height + adjustment)
        .fold(0.0, f32::max);
        let content = output.size.height;
        let end = padding.bottom + border.bottom;
        let baseline = output
            .baselines
            .first
            .map(|baseline| {
                if style.overflow.y.is_scroll_container() {
                    baseline.min(content).max(0.0)
                } else {
                    baseline
                }
            })
            .unwrap_or(content - end);
        measures.push(Measure {
            content,
            specified,
            baseline,
            end,
        });
    }
    // Cells aligned on the baseline share their first row's baseline.
    let mut baselines: Vec<Option<f32>> = vec![None; row_count];
    for (cell, measure) in table.cells.iter().zip(&measures) {
        if cell.align == Align::Baseline {
            let baseline = &mut baselines[cell.row];
            *baseline = Some(baseline.map_or(measure.baseline, |b| b.max(measure.baseline)));
        }
    }
    let needed = |cell: &Cell, measure: &Measure| {
        let content = match (cell.align, baselines[cell.row]) {
            (Align::Baseline, Some(baseline)) => baseline - measure.baseline + measure.content,
            _ => measure.content,
        };
        content.max(measure.specified)
    };
    for (cell, measure) in table.cells.iter().zip(&measures) {
        if cell.rows == 1 {
            heights[cell.row] = heights[cell.row].max(needed(cell, measure));
        }
    }
    let mut spanning: Vec<usize> = (0..table.cells.len())
        .filter(|&index| table.cells[index].rows > 1)
        .collect();
    spanning.sort_by_key(|&index| table.cells[index].rows);
    for index in spanning {
        let cell = &table.cells[index];
        let range = cell.row..(cell.row + cell.rows).min(row_count);
        let current = heights[range.clone()].iter().sum::<f32>()
            + spacing.height * range.len().saturating_sub(1) as f32;
        grow(&mut heights[range], needed(cell, &measures[index]) - current);
    }
    let content_height = heights.iter().sum::<f32>() + block_spacing + inset.top + inset.bottom;
    let height = input.known_dimensions.height.unwrap_or_else(|| {
        css_height
            .unwrap_or(content_height)
            .maybe_clamp(min_height, max_height)
            .max(content_height)
    });
    // A taller table gives the extra height to its rows.
    grow(&mut heights, height - content_height);

    // A row with no baseline-aligned cell takes its baseline from the lowest
    // content box bottom among its cells.
    let mut row_baselines = baselines.clone();
    let mut occupied = vec![false; row_count];
    for (cell, measure) in table.cells.iter().zip(&measures) {
        occupied[cell.row] = true;
        if cell.rows == 1 && baselines[cell.row].is_none() {
            let bottom = heights[cell.row] - measure.end;
            let baseline = &mut row_baselines[cell.row];
            *baseline = Some(baseline.map_or(bottom, |b| b.max(bottom)));
        }
    }
    let mut tops = Vec::with_capacity(row_count);
    let mut edge = inset.top + spacing.height;
    for height in &heights {
        tops.push(edge);
        edge += height + spacing.height;
    }
    let first_baseline = (0..row_count)
        .find(|&row| occupied[row])
        .and_then(|row| row_baselines[row].map(|baseline| tops[row] + baseline))
        .filter(|_| !contain.suppresses_baseline());
    let baselines = Baselines {
        first: first_baseline,
        // Chromium gives a table the same first and last baseline.
        last: first_baseline,
    };
    let size = Size { width, height };
    if input.run_mode == RunMode::ComputeSize {
        let mut output = LayoutOutput::from_outer_size(size);
        output.baselines = baselines;
        return output;
    }

    // Final layout: each cell fills its rows, and its content moves down by
    // the cell's alignment offset.
    let mut overflow_rect: Option<Rect<f32>> = None;
    let mut candidates = OofCandidates::new();
    for (order, ((cell, measure), &child)) in table
        .cells
        .iter()
        .zip(&measures)
        .zip(&children)
        .enumerate()
    {
        let width = span_width(cell);
        let rows = cell.row..(cell.row + cell.rows).min(row_count);
        let height = heights[rows.clone()].iter().sum::<f32>()
            + spacing.height * rows.len().saturating_sub(1) as f32;
        let last_column = (cell.column + cell.columns).min(column_count) - 1;
        let x = if rtl {
            lefts[last_column]
        } else {
            lefts[cell.column]
        };
        let y = tops[cell.row];
        let size = Size { width, height };
        let mut output = tree.compute_child_layout(
            child,
            LayoutInput {
                run_mode: RunMode::PerformLayout,
                sizing_mode: SizingMode::InherentSize,
                axis: RequestedAxis::Both,
                known_dimensions: size.map(Some),
                known_dimensions_are_definite: Size {
                    width: true,
                    height: true,
                },
                parent_size: size.map(Some),
                available_space: size.map(AvailableSpace::Definite),
                vertical_margins_are_collapsible: Line::FALSE,
            },
        );
        let offset = match cell.align {
            Align::Top => 0.0,
            // Chromium halves the free space in 1/64 px layout units.
            Align::Middle => ((height - measure.content) * 32.0).floor().max(0.0) / 64.0,
            Align::Bottom => (height - measure.content).max(0.0),
            Align::Baseline => row_baselines[cell.row]
                .map_or(0.0, |baseline| (baseline - measure.baseline).max(0.0)),
        };
        shift_content(tree, child, offset);
        let style = &tree[child].style;
        let padding = style.padding.resolve_or_zero(Some(width), resolve);
        let border = style.border.resolve_or_zero(Some(width), resolve);
        let relative = crate::relative_offset(
            style,
            Size {
                width: Some(target),
                height: None,
            },
            rtl,
            &calcs,
        );
        let location = Point {
            x: x + relative.x,
            y: y + relative.y,
        };
        let scrollbar = |overflow: Overflow| {
            if overflow == Overflow::Scroll {
                style.scrollbar_width
            } else {
                0.0
            }
        };
        let scrollbar_size = Size {
            width: scrollbar(style.overflow.y),
            height: scrollbar(style.overflow.x),
        };
        let cell_overflow = style.overflow;
        let cell_contain = style.contain;
        let content_overflow = shift_overflow(output.scrollable_overflow_rect, offset);
        tree.set_unrounded_layout(
            child,
            &Layout {
                order: order as u32,
                location,
                size,
                scrollable_overflow_rect: content_overflow,
                scrollbar_size,
                padding,
                border,
                margin: Rect::ZERO,
            },
        );
        let shift = |baseline: Option<f32>| baseline.map(|baseline| baseline + offset);
        tree[child].baselines = Baselines {
            first: shift(output.baselines.first),
            last: shift(output.baselines.last),
        };
        let mut bubbled = output.oof_candidates.take();
        if !bubbled.is_empty() {
            bubbled.translate(Point {
                x: location.x,
                y: location.y + offset,
            });
            candidates.append(&mut bubbled);
        }
        if let Some(contribution) = overflow_contribution(
            location,
            size,
            content_overflow.rect(),
            border,
            cell_overflow,
            cell_contain.contains_scrollable_overflow(),
        ) {
            overflow_rect = Some(overflow_rect.map_or(contribution, |rect| rect.union(contribution)));
        }
    }
    // Cells are placed in the border-box frame; overflow is relative to the
    // padding box.
    let overflow_rect = overflow_rect.map_or(Rect::ZERO, |rect| Rect {
        left: rect.left - border.left,
        right: rect.right - border.left,
        top: rect.top - border.top,
        bottom: rect.bottom - border.top,
    });
    let rows = tops
        .iter()
        .zip(&heights)
        .map(|(&start, height)| Line {
            start,
            end: start + height,
        })
        .collect::<Vec<_>>();
    let columns = if column_count == 0 {
        [inset.left; 2]
    } else {
        let first = lefts.iter().copied().fold(f32::INFINITY, f32::min);
        let last = lefts
            .iter()
            .zip(&widths)
            .map(|(left, width)| left + width)
            .fold(f32::NEG_INFINITY, f32::max);
        [first, last]
    };
    table.tracks = Tracks {
        end: rows.last().map_or(inset.top, |row| row.end),
        rows,
        columns,
    };
    let is_scroll_container = overflow.x.is_scroll_container() || overflow.y.is_scroll_container();
    let mut output = LayoutOutput::from_sizes_and_baselines(
        size,
        finalize_overflow(overflow_rect, is_scroll_container),
        baselines,
    );
    output.oof_candidates = candidates;
    output.oof_positioning_area = Some(OofPositioningArea {
        size: Size {
            width: (size.width - border.left - border.right).max(0.0),
            height: (size.height - border.top - border.bottom).max(0.0),
        },
        offset: Point {
            x: border.left,
            y: border.top,
        },
    });
    output
}

/// Merges the cells' width contributions into column constraints. Fixed
/// table layout takes the columns from the first row's cell widths alone.
fn column_constraints(
    tree: &mut BoxTree<'_>,
    table: &Table,
    children: &[Id],
    fixed_layout: bool,
) -> Vec<Column> {
    let calcs = Rc::clone(&tree.calcs);
    let resolve = |handle, basis| calcs.resolve(handle, basis);
    let mut columns = vec![Column::default(); table.columns];
    if fixed_layout {
        for (cell, &child) in table.cells.iter().zip(children) {
            if cell.row != 0 {
                continue;
            }
            let span = cell.columns.min(table.columns - cell.column);
            let edges = horizontal_edges(tree, child);
            let column = match cell.size.width.expand() {
                ExpandedDimension::Percent(percent) => Column {
                    percent: Some(percent / span as f32),
                    ..Column::default()
                },
                _ => match cell.size.width.maybe_resolve(None, resolve) {
                    Some(width) => {
                        let width = (width + edges.adjustment).max(edges.sum) / span as f32;
                        Column {
                            min: width,
                            max: width,
                            percent: None,
                            fixed: true,
                        }
                    }
                    None => Column::default(),
                },
            };
            columns[cell.column..cell.column + span].fill(column);
        }
        return columns;
    }
    let mut order: Vec<usize> = (0..table.cells.len()).collect();
    order.sort_by_key(|&index| table.cells[index].columns);
    for index in order {
        let cell = &table.cells[index];
        let constraint = cell_constraint(tree, children[index], cell);
        let span = cell.columns.min(table.columns - cell.column);
        let spanned = &mut columns[cell.column..cell.column + span];
        if let [column] = spanned {
            column.encompass(constraint);
            continue;
        }
        // A spanning cell widens its automatic columns in proportion to
        // their maximums, or equally when they have none. Its minimum widens
        // fixed and percentage columns when there are no automatic ones, but
        // its maximum does not.
        let inner = table.spacing.width * (span - 1) as f32;
        let automatic: Vec<bool> = spanned
            .iter()
            .map(|column| !column.fixed && column.percent.is_none())
            .collect();
        let any_automatic = automatic.contains(&true);
        let weights = |spanned: &[Column]| -> Vec<f32> {
            spanned
                .iter()
                .zip(&automatic)
                .map(|(column, &automatic)| {
                    if automatic || !any_automatic {
                        column.max
                    } else {
                        0.0
                    }
                })
                .collect()
        };
        let candidates: Vec<bool> = automatic
            .iter()
            .map(|&automatic| automatic || !any_automatic)
            .collect();
        let mut mins: Vec<f32> = spanned.iter().map(|column| column.min).collect();
        let total = mins.iter().sum::<f32>();
        share_among(
            &mut mins,
            &weights(spanned),
            &candidates,
            constraint.min - inner - total,
        );
        for (column, min) in spanned.iter_mut().zip(mins) {
            column.min = min;
            column.max = column.max.max(min);
        }
        if any_automatic {
            let mut maxes: Vec<f32> = spanned.iter().map(|column| column.max).collect();
            let total = maxes.iter().sum::<f32>();
            share_among(
                &mut maxes,
                &weights(spanned),
                &automatic,
                constraint.max - inner - total,
            );
            for (column, max) in spanned.iter_mut().zip(maxes) {
                column.max = max;
            }
        }
    }
    columns
}

/// Adds `amount` to the `candidates` among `values` in proportion to their
/// `weights`, or equally when their weights are all zero.
fn share_among(values: &mut [f32], weights: &[f32], candidates: &[bool], amount: f32) {
    let weights: Vec<f32> = if weights
        .iter()
        .zip(candidates)
        .any(|(weight, &candidate)| candidate && *weight > 0.0)
    {
        weights
            .iter()
            .zip(candidates)
            .map(|(&weight, &candidate)| if candidate { weight } else { 0.0 })
            .collect()
    } else {
        candidates
            .iter()
            .map(|&candidate| f32::from(u8::from(candidate)))
            .collect()
    };
    share(values, &weights, amount);
}

struct HorizontalEdges {
    /// Horizontal padding and border.
    sum: f32,
    /// What a content-box width adds to become a border-box width.
    adjustment: f32,
}

/// A cell's horizontal padding and border for intrinsic sizing, where
/// percentages have no basis.
fn horizontal_edges(tree: &BoxTree<'_>, child: Id) -> HorizontalEdges {
    let style = &tree[child].style;
    let resolve = |handle, basis| tree.calcs.resolve(handle, basis);
    let padding = style.padding.resolve_or_zero(None, resolve);
    let border = style.border.resolve_or_zero(None, resolve);
    let sum = padding.left + padding.right + border.left + border.right;
    HorizontalEdges {
        sum,
        adjustment: match style.box_sizing {
            BoxSizing::ContentBox => sum,
            BoxSizing::BorderBox => 0.0,
        },
    }
}

/// A cell's minimum and maximum border-box widths: its min- and
/// max-content widths, raised by `min-width`, with a fixed `width` as the
/// maximum instead of the max-content width.
fn cell_constraint(tree: &mut BoxTree<'_>, child: Id, cell: &Cell) -> Column {
    let mut measure = |available| {
        tree.compute_child_layout(
            child,
            LayoutInput {
                run_mode: RunMode::ComputeSize,
                sizing_mode: SizingMode::InherentSize,
                axis: RequestedAxis::Horizontal,
                known_dimensions: Size::NONE,
                known_dimensions_are_definite: Size {
                    width: true,
                    height: true,
                },
                parent_size: Size::NONE,
                available_space: Size {
                    width: available,
                    height: AvailableSpace::MaxContent,
                },
                vertical_margins_are_collapsible: Line::FALSE,
            },
        )
        .size
        .width
    };
    let content_min = measure(AvailableSpace::MinContent);
    let content_max = measure(AvailableSpace::MaxContent);
    let calcs = Rc::clone(&tree.calcs);
    let resolve = |handle, basis| calcs.resolve(handle, basis);
    let edges = horizontal_edges(tree, child);
    let border_box = |width: f32| (width + edges.adjustment).max(edges.sum);
    let css_min = cell
        .min_size
        .width
        .maybe_resolve(None, resolve)
        .map(border_box);
    let css_max = cell.max_width.maybe_resolve(None, resolve).map(border_box);
    let (width, percent) = match cell.size.width.expand() {
        ExpandedDimension::Percent(percent) => (None, Some(percent)),
        _ => (
            cell.size.width.maybe_resolve(None, resolve).map(border_box),
            None,
        ),
    };
    let min = content_min.max(css_min.unwrap_or(0.0)).max(edges.sum);
    let mut max = width.unwrap_or(content_max).max(min);
    if let Some(css_max) = css_max {
        max = max.min(css_max).max(min);
    }
    Column {
        min,
        max,
        percent,
        fixed: width.is_some(),
    }
}

/// The columns' max-content width, widened so that percentage columns get
/// their share of it.
fn max_content(columns: &[Column]) -> f32 {
    let mut total: f32 = columns.iter().map(|column| column.max).sum();
    let percent = columns
        .iter()
        .filter_map(|column| column.percent)
        .sum::<f32>()
        .min(1.0);
    if percent > 0.0 {
        for column in columns {
            if let Some(share) = column.percent.filter(|share| *share > 0.0) {
                total = total.max(column.max / share);
            }
        }
        if percent < 1.0 {
            let rest: f32 = columns
                .iter()
                .filter(|column| column.percent.is_none())
                .map(|column| column.max)
                .sum();
            total = total.max(rest / (1.0 - percent));
        }
    }
    total
}

/// Fixed table layout: fixed and percentage columns take their widths and
/// automatic columns share what remains.
fn distribute_fixed(columns: &[Column], target: f32) -> Vec<f32> {
    let mut widths: Vec<f32> = columns
        .iter()
        .map(|column| column.percent.map_or(column.max, |percent| percent * target))
        .collect();
    let excess = target - widths.iter().sum::<f32>();
    let automatic: Vec<f32> = columns
        .iter()
        .map(|column| f32::from(u8::from(!column.fixed && column.percent.is_none())))
        .collect();
    if automatic.iter().any(|weight| *weight > 0.0) {
        share(&mut widths, &automatic, excess);
    } else {
        let weights = widths.clone();
        share(&mut widths, &weights, excess);
    }
    widths
}

/// Automatic table layout (CSS Tables 3 § 3.9.3): the used widths
/// interpolate between the min-content, percentage, specified and
/// max-content guesses, and width beyond the max-content guess goes to
/// automatic columns first.
fn distribute_auto(columns: &[Column], target: f32) -> Vec<f32> {
    let percent = |column: &Column| {
        column
            .percent
            .map_or(column.min, |percent| column.min.max(percent * target))
    };
    let guesses: [Vec<f32>; 4] = [
        columns.iter().map(|column| column.min).collect(),
        columns.iter().map(percent).collect(),
        columns
            .iter()
            .map(|column| match (column.percent, column.fixed) {
                (Some(_), _) => percent(column),
                (None, true) => column.max,
                (None, false) => column.min,
            })
            .collect(),
        columns
            .iter()
            .map(|column| {
                if column.percent.is_some() {
                    percent(column)
                } else {
                    column.max
                }
            })
            .collect(),
    ];
    let sums = guesses.each_ref().map(|guess| guess.iter().sum::<f32>());
    if target <= sums[0] {
        return guesses[0].clone();
    }
    for step in 0..3 {
        if target <= sums[step + 1] {
            let range = sums[step + 1] - sums[step];
            let t = if range > 0.0 {
                (target - sums[step]) / range
            } else {
                1.0
            };
            return guesses[step]
                .iter()
                .zip(&guesses[step + 1])
                .map(|(low, high)| low + (high - low) * t)
                .collect();
        }
    }
    let mut widths = guesses[3].clone();
    let excess = target - sums[3];
    let automatic = |column: &Column| !column.fixed && column.percent.is_none();
    // The excess goes to automatic columns, else to fixed ones, else to
    // percentage ones, in proportion to their widths.
    let candidates: Vec<bool> = if columns.iter().any(automatic) {
        columns.iter().map(automatic).collect()
    } else if columns.iter().any(|column| column.percent.is_none()) {
        columns
            .iter()
            .map(|column| column.percent.is_none())
            .collect()
    } else {
        vec![true; columns.len()]
    };
    let weights: Vec<f32> = columns
        .iter()
        .zip(&candidates)
        .map(|(column, &candidate)| match (candidate, column.percent) {
            (false, _) => 0.0,
            (true, Some(percent)) => percent,
            (true, None) => column.max,
        })
        .collect();
    if weights.iter().any(|weight| *weight > 0.0) {
        share(&mut widths, &weights, excess);
    } else {
        let equal: Vec<f32> = candidates
            .iter()
            .map(|&candidate| f32::from(u8::from(candidate)))
            .collect();
        share(&mut widths, &equal, excess);
    }
    widths
}

/// Adds `amount` to `values` in proportion to `weights`, or equally when
/// every weight is zero. Nothing is added when `amount` is not positive.
fn share(values: &mut [f32], weights: &[f32], amount: f32) {
    if amount <= 0.0 || values.is_empty() {
        return;
    }
    let total: f32 = weights.iter().sum();
    if total > 0.0 {
        for (value, weight) in values.iter_mut().zip(weights) {
            *value += amount * weight / total;
        }
    } else {
        let each = amount / values.len() as f32;
        for value in values {
            *value += each;
        }
    }
}

/// Grows rows by `amount` in proportion to their heights.
fn grow(heights: &mut [f32], amount: f32) {
    let weights = heights.to_vec();
    share(heights, &weights, amount);
}

/// Moves a cell's in-flow content down to its alignment offset. A cached
/// final layout leaves the children where the last shift put them, so only
/// the difference from that shift applies.
fn shift_content(tree: &mut BoxTree<'_>, cell: Id, offset: f32) {
    let delta = offset - tree[cell].content_offset;
    tree[cell].content_offset = offset;
    if delta == 0.0 {
        return;
    }
    for index in 0..tree[cell].children.len() {
        let child = tree[cell].children[index];
        if !tree[child].style.position.is_out_of_flow() {
            tree[child].layout.location.y += delta;
        }
    }
}

fn shift_overflow(overflow: ScrollableOverflow, offset: f32) -> ScrollableOverflow {
    let rect = overflow.rect();
    if offset == 0.0 || rect.right <= rect.left || rect.bottom <= rect.top {
        return overflow;
    }
    ScrollableOverflow::new(
        Rect {
            top: (rect.top + offset).min(0.0),
            bottom: (rect.bottom + offset).max(0.0),
            ..rect
        },
        overflow.is_reversed(),
    )
}

/// The part of the table's scrollable overflow a cell accounts for: its
/// border box and, unless it clips, its own overflow.
fn overflow_contribution(
    location: Point<f32>,
    size: Size<f32>,
    overflow: Rect<f32>,
    border: Rect<f32>,
    style: Point<Overflow>,
    contained: bool,
) -> Option<Rect<f32>> {
    let scrolls = style.x.is_scroll_container() || style.y.is_scroll_container();
    let propagates = |axis: Overflow| !scrolls && !contained && axis == Overflow::Visible;
    let (left, right) = if propagates(style.x) {
        (
            (overflow.left + border.left).min(0.0),
            size.width.max(overflow.right + border.left),
        )
    } else {
        (0.0, size.width)
    };
    let (top, bottom) = if propagates(style.y) {
        (
            (overflow.top + border.top).min(0.0),
            size.height.max(overflow.bottom + border.top),
        )
    } else {
        (0.0, size.height)
    };
    (right > 0.0 && bottom > 0.0).then_some(Rect {
        left: location.x + left,
        right: location.x + right,
        top: location.y + top,
        bottom: location.y + bottom,
    })
}

fn finalize_overflow(rect: Rect<f32>, is_scroll_container: bool) -> ScrollableOverflow {
    let (left, top) = if is_scroll_container {
        (0.0, 0.0)
    } else {
        (rect.left.min(0.0), rect.top.min(0.0))
    };
    ScrollableOverflow::new(
        Rect {
            left,
            top,
            right: rect.right.max(0.0),
            bottom: rect.bottom.max(0.0),
        },
        Point { x: false, y: false },
    )
}
