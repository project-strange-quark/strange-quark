// SPDX-License-Identifier: MIT

use squark_assets::{ImageCatalog, Resources};
use squark_mirror::Document;
use squark_types::{
    NodeId, Rect,
    fragment::{Edges, PaintValues},
};
use style::values::computed::CSSPixelLength;

use crate::{Css, paint};

#[derive(Default)]
pub(crate) struct Slices(Vec<Slice>);

struct Slice {
    owner: NodeId,
    css: Css,
    x: [f32; 2],
    y: [f32; 2],
    edges: [bool; 2],
}

pub(crate) struct PaintedSlice {
    pub owner: NodeId,
    pub rect: Rect,
    pub border: Edges<f32>,
    pub padding: Edges<f32>,
    pub style: PaintValues,
}

impl Slices {
    pub fn edge(
        &mut self,
        document: &Document,
        owner: NodeId,
        container: NodeId,
        x: f32,
        width: f32,
        left: bool,
    ) {
        self.include(document, owner, container, |slice| {
            slice.extend_x(x, width);
            if slice.owner == owner {
                slice.edges[usize::from(!left)] = true;
            }
        });
    }

    pub fn text(&mut self, document: &Document, owner: NodeId, container: NodeId, rect: Rect) {
        self.include(document, owner, container, |slice| {
            slice.extend_x(rect.origin.x, rect.size.width);
            slice.y[0] = slice.y[0].min(rect.origin.y);
            slice.y[1] = slice.y[1].max(rect.max_y());
        });
    }

    fn include(
        &mut self,
        document: &Document,
        mut owner: NodeId,
        container: NodeId,
        apply: impl Fn(&mut Slice),
    ) {
        let mut ancestors = Vec::new();
        while owner != container {
            let Some(css) = document.computed_style(owner) else {
                break;
            };
            use style::values::specified::box_::{DisplayInside, DisplayOutside};
            let display = css.get_display();
            if display.inside() == DisplayInside::Flow
                && display.outside() == DisplayOutside::Inline
            {
                ancestors.push((owner, css));
            }
            let Some(parent) = document.parent(owner) else {
                break;
            };
            owner = parent;
        }
        for (owner, css) in ancestors.into_iter().rev() {
            let index = self
                .0
                .iter()
                .position(|slice| slice.owner == owner)
                .unwrap_or_else(|| {
                    self.0.push(Slice {
                        owner,
                        css,
                        x: [f32::INFINITY, f32::NEG_INFINITY],
                        y: [f32::INFINITY, f32::NEG_INFINITY],
                        edges: [false; 2],
                    });
                    self.0.len() - 1
                });
            apply(&mut self.0[index]);
        }
    }

    pub fn finish(
        self,
        containing_width: f32,
        resources: Resources<'_>,
        images: &mut ImageCatalog,
    ) -> Vec<PaintedSlice> {
        self.0
            .into_iter()
            .filter(|slice| slice.y[0].is_finite())
            .map(|slice| {
                let css = &slice.css;
                let pad = css.get_padding();
                let border = css.get_border();
                let resolve = |length: &style::values::computed::NonNegativeLengthPercentage| {
                    length.0.resolve(CSSPixelLength::new(containing_width)).px()
                };
                let border_width =
                    |style: style::values::specified::border::BorderStyle,
                     width: &style::values::computed::BorderSideWidth| {
                        if style.none_or_hidden() {
                            0.0
                        } else {
                            width.0.to_f32_px()
                        }
                    };
                let [left, right] = slice.edges;
                let padding = Edges {
                    left: if left {
                        resolve(&pad.padding_left)
                    } else {
                        0.0
                    },
                    right: if right {
                        resolve(&pad.padding_right)
                    } else {
                        0.0
                    },
                    top: resolve(&pad.padding_top),
                    bottom: resolve(&pad.padding_bottom),
                };
                let border = Edges {
                    left: if left {
                        border_width(border.border_left_style, &border.border_left_width)
                    } else {
                        0.0
                    },
                    right: if right {
                        border_width(border.border_right_style, &border.border_right_width)
                    } else {
                        0.0
                    },
                    top: border_width(border.border_top_style, &border.border_top_width),
                    bottom: border_width(border.border_bottom_style, &border.border_bottom_width),
                };
                let rect = Rect::new(
                    slice.x[0],
                    slice.y[0] - padding.top - border.top,
                    (slice.x[1] - slice.x[0]).max(0.0),
                    (slice.y[1] - slice.y[0]
                        + padding.top
                        + padding.bottom
                        + border.top
                        + border.bottom)
                        .max(0.0),
                );
                let padding_size = crate::fragments::inset(rect, border).size;
                let content_size =
                    crate::fragments::inset(crate::fragments::inset(rect, border), padding).size;
                let mut style = paint::project(
                    css,
                    resources,
                    images,
                    rect.size,
                    padding_size,
                    content_size,
                );
                if !left {
                    style.box_paint.radii[0] = Default::default();
                    style.box_paint.radii[3] = Default::default();
                }
                if !right {
                    style.box_paint.radii[1] = Default::default();
                    style.box_paint.radii[2] = Default::default();
                }
                PaintedSlice {
                    owner: slice.owner,
                    rect,
                    border,
                    padding,
                    style,
                }
            })
            .collect()
    }
}

impl Slice {
    fn extend_x(&mut self, x: f32, width: f32) {
        self.x[0] = self.x[0].min(x);
        self.x[1] = self.x[1].max(x + width);
    }
}
