// SPDX-License-Identifier: MIT

use std::collections::{BTreeMap, BTreeSet};

use squark_mirror::Document;
use squark_types::{
    NodeId, Point, Rect,
    engine::BoxMetrics,
    fragment::{
        DisplayKind, Fragment, FragmentId, FragmentKind, FragmentTree, GeometryBox, Position2D,
        PositionKind, Transform,
    },
};

use crate::fragments::inset;

#[derive(Debug, Default)]
pub(crate) struct BoxIndex(BTreeMap<NodeId, Vec<FragmentId>>);

impl BoxIndex {
    /// Indexes the box fragments of each element, leaving out `anonymous`
    /// boxes, which layout generates for no element of their own.
    pub fn build(tree: &FragmentTree, anonymous: &BTreeSet<FragmentId>) -> Self {
        let mut boxes = BTreeMap::<_, Vec<_>>::new();
        let mut pending = vec![tree.root];
        while let Some(id) = pending.pop() {
            let fragment = &tree.nodes[&id];
            if matches!(fragment.kind, FragmentKind::Box | FragmentKind::Replaced)
                && !anonymous.contains(&id)
            {
                boxes.entry(fragment.owner).or_default().push(id);
            }
            pending.extend(fragment.children.iter().rev());
        }
        Self(boxes)
    }
}

pub(crate) struct Geometry<'a> {
    pub document: &'a Document,
    pub tree: &'a FragmentTree,
    pub index: &'a BoxIndex,
    pub margins: &'a crate::fragments::Margins,
}

impl<'a> Geometry<'a> {
    pub fn boxes(&self, owner: NodeId) -> impl Iterator<Item = &'a Fragment> + use<'a> {
        let nodes = &self.tree.nodes;
        self.index
            .0
            .get(&owner)
            .into_iter()
            .flatten()
            .map(|id| &nodes[id])
    }

    pub fn ancestors(&self, fragment: &Fragment) -> impl Iterator<Item = &'a Fragment> + use<'a> {
        let tree = self.tree;
        std::iter::successors(
            fragment.parent.and_then(|id| tree.nodes.get(&id)),
            move |f| f.parent.and_then(|id| tree.nodes.get(&id)),
        )
    }

    pub fn viewport(&self) -> &'a Fragment {
        &self.tree.nodes[&self.tree.root]
    }

    pub fn client_rect(&self, fragment: &Fragment, rect: Rect) -> Rect {
        let rect = euclid::rect(
            rect.origin.x,
            rect.origin.y,
            rect.size.width,
            rect.size.height,
        );
        self.client_transform(fragment)
            .outer_transformed_rect(&rect)
            .map_or(Rect::default(), |r| {
                Rect::new(r.origin.x, r.origin.y, r.size.width, r.size.height)
            })
    }

    pub fn document_point(&self, fragment: &Fragment, point: Point) -> Option<Point> {
        use euclid::default::Transform3D;
        let m = self.client_transform(fragment);
        // Invert the projection on the plane of the text itself, which
        // can be tilted in 3D while the input point only has viewport x/y.
        let plane = Transform3D::<f32>::new(
            m.m11, m.m12, 0.0, m.m14, m.m21, m.m22, 0.0, m.m24, 0.0, 0.0, 1.0, 0.0, m.m41, m.m42,
            0.0, m.m44,
        );
        let point = plane
            .inverse()?
            .transform_point2d(euclid::point2(point.x, point.y))?;
        Some(Point {
            x: point.x,
            y: point.y,
        })
    }

    fn client_transform(&self, fragment: &Fragment) -> euclid::default::Transform3D<f32> {
        use euclid::default::Transform3D;
        let mut transform = Transform3D::<f32>::identity();
        let root_fixed = |f: &Fragment| {
            f.style.stacking.position == PositionKind::Fixed
                && f.containing_block == Some(self.tree.root)
        };
        for (index, item) in std::iter::once(fragment)
            .chain(self.ancestors(fragment))
            .enumerate()
        {
            if index > 0 {
                if let Some(scroll) = item.scroll {
                    let point = scroll.offset;
                    transform = transform.then_translate(euclid::vec3(-point.x, -point.y, 0.0));
                }
                if let Some(depth) = item
                    .style
                    .properties
                    .perspective
                    .filter(|depth| *depth > 0.0)
                {
                    let origin =
                        reference_point(item.border_box, item.style.properties.perspective_origin);
                    transform = transform
                        .then_translate(euclid::vec3(-origin.x, -origin.y, 0.0))
                        .then(&Transform3D::perspective(depth.max(1.0)))
                        .then_translate(euclid::vec3(origin.x, origin.y, 0.0));
                }
            }
            if item.style.stacking.display != DisplayKind::Inline
                || item.kind == FragmentKind::Replaced
            {
                let props = &item.style.properties;
                let reference = transform_reference_box(item);
                let origin = reference_point(reference, props.transform_origin);
                let shift = euclid::vec3(origin.x, origin.y, props.transform_origin_z);
                let mut local = Transform3D::identity();
                // CSS multiplies the transform list in declaration order.
                for operation in props.transform.iter().rev() {
                    let operation = match *operation {
                        Transform::Translate { xy, z } => {
                            let p = reference_point(
                                Rect {
                                    origin: Point::default(),
                                    size: reference.size,
                                },
                                xy,
                            );
                            Transform3D::translation(p.x, p.y, z)
                        }
                        Transform::Matrix(m) => Transform3D::from_arrays(m),
                        Transform::Rotate { .. }
                        | Transform::Scale { .. }
                        | Transform::Skew { .. } => {
                            Transform3D::from_arrays(operation.matrix(reference.size))
                        }
                    };
                    local = local.then(&operation);
                }
                transform = transform
                    .then_translate(-shift)
                    .then(&local)
                    .then_translate(shift);
            }
            let sticky = self.sticky(item);
            transform = transform.then_translate(euclid::vec3(sticky.x, sticky.y, 0.0));
            if root_fixed(item) {
                break;
            }
        }
        transform
    }

    fn sticky(&self, item: &Fragment) -> Point {
        let Some(sticky) = item.sticky else {
            return Point::default();
        };
        let Some(frame) = self
            .ancestors(item)
            .filter_map(|f| f.scroll)
            .find(|s| s.owner == sticky.scroll_owner)
        else {
            return Point::default();
        };
        let offset = frame.offset;
        let bounds = item.border_box;
        let parent = sticky.containing_rect;
        let port = frame.scrollport;
        Point {
            x: sticky_axis(
                bounds.origin.x,
                bounds.size.width,
                parent.origin.x,
                parent.max_x(),
                port.origin.x + offset.x,
                port.size.width,
                sticky.insets.left,
                sticky.insets.right,
            ),
            y: sticky_axis(
                bounds.origin.y,
                bounds.size.height,
                parent.origin.y,
                parent.max_y(),
                port.origin.y + offset.y,
                port.size.height,
                sticky.insets.top,
                sticky.insets.bottom,
            ),
        }
    }

    pub fn box_metrics(&self, owner: NodeId) -> BoxMetrics {
        let boxes = self.boxes(owner);
        let Some(first) = self.boxes(owner).next() else {
            return BoxMetrics::default();
        };
        let bounds = bounding(boxes.map(|f| f.border_box));
        let offset_parent = self.offset_parent(owner, first);
        let origin = offset_parent
            .and_then(|id| self.boxes(id).next())
            .map_or(Point::default(), |f| inset(f.border_box, f.border).origin);
        let inline = first.style.stacking.display == DisplayKind::Inline
            && first.kind != FragmentKind::Replaced;
        let html_root = self.document.html_element(owner) == Some("html")
            && self.document.quirks() != style::context::QuirksMode::Quirks;
        let quirks_body = self.document.html_element(owner) == Some("body")
            && self.document.quirks() == style::context::QuirksMode::Quirks;
        let client = if html_root || quirks_body {
            self.viewport().border_box
        } else {
            inset(first.border_box, first.border)
        };
        let source = if html_root || quirks_body {
            self.viewport()
        } else {
            first
        };
        let scroll = source
            .scroll
            .map_or(source.scrollable_overflow, |frame| frame.content);
        let round = |value: f32| value.round() as i32;
        BoxMetrics {
            offset_parent,
            offset_top: round(first.border_box.origin.y - origin.y),
            offset_left: round(first.border_box.origin.x - origin.x),
            offset_width: round(bounds.size.width),
            offset_height: round(bounds.size.height),
            client_top: if inline { 0 } else { round(first.border.top) },
            client_left: if inline { 0 } else { round(first.border.left) },
            client_width: if inline { 0 } else { round(client.size.width) },
            client_height: if inline { 0 } else { round(client.size.height) },
            scroll_width: if inline {
                0
            } else {
                round((scroll.max_x() - client.origin.x).max(client.size.width))
            },
            scroll_height: if inline {
                0
            } else {
                round((scroll.max_y() - client.origin.y).max(client.size.height))
            },
        }
    }

    fn offset_parent(&self, owner: NodeId, first: &Fragment) -> Option<NodeId> {
        if matches!(self.document.html_element(owner), Some("html" | "body")) {
            return None;
        }
        if first.style.stacking.position == PositionKind::Fixed {
            return first
                .containing_block
                .filter(|&id| id != self.tree.root)
                .and_then(|id| self.tree.nodes.get(&id))
                .map(|fragment| fragment.owner);
        }
        std::iter::successors(self.document.parent(owner), |id| self.document.parent(*id)).find(
            |&id| {
                let Some(css) = self.document.computed_style(id) else {
                    return false;
                };
                use style::values::specified::box_::DisplayInside;
                let display = css.get_display();
                if display.is_none() || display.is_contents() {
                    return false;
                }
                self.document.html_element(id) == Some("body")
                    || css.get_box().position != crate::css::stylo::Position::Static
                    || crate::css::containing_block_claims(&css)
                        != taffy::ContainingBlockClaims::NONE
                    || (first.style.stacking.position == PositionKind::Static
                        && matches!(
                            display.inside(),
                            DisplayInside::Table | DisplayInside::TableCell
                        ))
            },
        )
    }

    pub fn focus_scroll(&self, owner: NodeId) -> Vec<(NodeId, Point)> {
        let boxes = self.boxes(owner);
        let Some(first) = self.boxes(owner).next() else {
            return Vec::new();
        };
        let mut target = bounding(boxes.map(|f| f.border_box));
        let mut offsets = Vec::new();
        let mut child = first;
        for ancestor in self.ancestors(first) {
            if child.style.stacking.position == PositionKind::Fixed
                && child.containing_block == Some(self.tree.root)
            {
                break;
            }
            child = ancestor;
            let Some(frame) = ancestor.scroll else {
                continue;
            };
            let old = frame.offset;
            let port = frame.scrollport;
            let nearest = |min: f32, max: f32, low: f32, high: f32| {
                if min < low && max > high {
                    0.0
                } else if min < low {
                    min - low
                } else {
                    (max - high).max(0.0)
                }
            };
            // Focusing uses center alignment in the block direction and nearest inline.
            let x = if frame.scroll_x {
                old.x
                    + nearest(
                        target.origin.x,
                        target.max_x(),
                        port.origin.x + old.x,
                        port.max_x() + old.x,
                    )
            } else {
                old.x
            };
            let outside =
                target.origin.y < port.origin.y + old.y || target.max_y() > port.max_y() + old.y;
            let y = if !frame.scroll_y {
                old.y
            } else if outside {
                (target.origin.y + target.max_y() - port.origin.y - port.max_y()) / 2.0
            } else {
                old.y
            };
            let offset = crate::scroll::clamp(frame, Point { x, y });
            offsets.push((frame.owner, offset));
            target.origin.x -= offset.x;
            target.origin.y -= offset.y;
        }
        offsets
    }
}

#[allow(clippy::too_many_arguments)]
fn sticky_axis(
    start: f32,
    length: f32,
    min: f32,
    max: f32,
    port: f32,
    port_length: f32,
    leading: Option<f32>,
    trailing: Option<f32>,
) -> f32 {
    let mut position = start;
    if let Some(end) = trailing {
        let end = end.min(port_length - length - leading.unwrap_or(0.0));
        position = position.min(port + port_length - end - length);
    }
    if let Some(lead) = leading {
        position = position.max(port + lead);
    }
    position.clamp(min.min(start), (max - length).max(start)) - start
}

fn transform_reference_box(fragment: &Fragment) -> Rect {
    match fragment.style.properties.transform_box {
        GeometryBox::Content | GeometryBox::Fill => inset(
            inset(fragment.border_box, fragment.border),
            fragment.padding,
        ),
        GeometryBox::Padding => inset(fragment.border_box, fragment.border),
        _ => fragment.border_box,
    }
}

fn reference_point(rect: Rect, position: Position2D) -> Point {
    Point {
        x: rect.origin.x + position.x.px + rect.size.width * position.x.fraction,
        y: rect.origin.y + position.y.px + rect.size.height * position.y.fraction,
    }
}

pub(crate) fn bounding(mut rects: impl Iterator<Item = Rect>) -> Rect {
    let Some(first) = rects.next() else {
        return Rect::default();
    };
    std::iter::once(first)
        .chain(rects)
        .filter(|r| r.size.width != 0.0 && r.size.height != 0.0)
        .reduce(|a, b| {
            let x = a.origin.x.min(b.origin.x);
            let y = a.origin.y.min(b.origin.y);
            Rect::new(
                x,
                y,
                a.max_x().max(b.max_x()) - x,
                a.max_y().max(b.max_y()) - y,
            )
        })
        .unwrap_or(first)
}

pub(crate) fn computed_style(
    document: &Document,
    cascade: &squark_style::Cascade<'_>,
    geometry: &Geometry<'_>,
    owner: NodeId,
    pseudo: Option<squark_types::engine::Pseudo>,
    property: &str,
) -> String {
    use squark_types::engine::Pseudo;
    use style::{
        properties::{
            Importance, LonghandId, PropertyDeclarationBlock, PropertyDeclarationId, PropertyId,
            SourcePropertyDeclaration, parse_one_declaration_into,
        },
        selector_parser::PseudoElement,
        stylesheets::{CssRuleType, Origin},
    };
    let pseudo_element = pseudo.map(|pseudo| match pseudo {
        Pseudo::Before => PseudoElement::Before,
        Pseudo::After => PseudoElement::After,
        Pseudo::Marker => PseudoElement::Marker,
        Pseudo::Placeholder => PseudoElement::Placeholder,
        Pseudo::FirstLetter => PseudoElement::FirstLetter,
        Pseudo::FirstLine => PseudoElement::FirstLine,
    });
    let Some(css) = cascade.computed_style(document, owner, pseudo_element.as_ref()) else {
        return String::new();
    };
    let css = if pseudo.is_none() {
        crate::controls::menu_style(document, owner, css)
    } else {
        css
    };
    let Ok(id) = PropertyId::parse_enabled_for_all_content(property) else {
        return String::new();
    };
    let first = if let Some(pseudo) = pseudo {
        let kind = match pseudo {
            Pseudo::Before => Some(FragmentKind::Before),
            Pseudo::After => Some(FragmentKind::After),
            Pseudo::Marker => Some(FragmentKind::Marker),
            _ => None,
        };
        kind.and_then(|kind| {
            geometry
                .tree
                .nodes
                .values()
                .find(|fragment| fragment.owner == owner && fragment.kind == kind)
        })
    } else {
        geometry.boxes(owner).next()
    };
    let resolve = |longhand: LonghandId| {
        let physical = longhand.to_physical(css.writing_mode);
        first
            .and_then(|fragment| used_value(geometry, &css, physical, fragment))
            .unwrap_or_else(|| {
                css.computed_value_to_string(PropertyDeclarationId::Longhand(longhand))
            })
    };
    match id.as_shorthand() {
        Err(PropertyDeclarationId::Longhand(longhand)) => resolve(longhand),
        Err(custom) => css.computed_value_to_string(custom),
        Ok(shorthand) => {
            let mut block = PropertyDeclarationBlock::default();
            for longhand in shorthand.longhands() {
                let mut declarations = SourcePropertyDeclaration::default();
                if parse_one_declaration_into(
                    &mut declarations,
                    PropertyId::NonCustom(longhand.into()),
                    &resolve(longhand),
                    Origin::Author,
                    &cascade.resources().document_url.clone().into(),
                    None,
                    style_traits::ParsingMode::DEFAULT,
                    document.quirks(),
                    CssRuleType::Style,
                )
                .is_err()
                {
                    return String::new();
                }
                block.extend(declarations.drain(), Importance::Normal);
            }
            let mut result = String::new();
            block
                .property_value_to_css(&id, &mut result)
                .expect("writing CSS to String");
            result
        }
    }
}

fn used_value(
    geometry: &Geometry<'_>,
    css: &style::properties::ComputedValues,
    property: style::properties::LonghandId,
    fragment: &Fragment,
) -> Option<String> {
    use crate::css::stylo::{BoxSizing as CssBoxSizing, InsetVal, Position as CssPosition};
    use style::{properties::LonghandId::*, values::computed::CSSPixelLength};
    let box_rect = fragment.border_box;
    let containing = fragment
        .containing_block
        .and_then(|id| geometry.tree.nodes.get(&id))
        .or_else(|| {
            geometry
                .ancestors(fragment)
                .find(|f| matches!(f.kind, FragmentKind::Box | FragmentKind::Viewport))
        })?;
    let padding_box = inset(containing.border_box, containing.border);
    let content_box = inset(padding_box, containing.padding);
    let content_width = CSSPixelLength::new(content_box.size.width);
    let value = match property {
        Transform => {
            let transform = css.get_transform();
            if transform.0.is_empty() {
                return Some("none".into());
            }
            let reference = transform_reference_box(fragment);
            let basis = euclid::rect(
                CSSPixelLength::new(0.0),
                CSSPixelLength::new(0.0),
                CSSPixelLength::new(reference.size.width),
                CSSPixelLength::new(reference.size.height),
            );
            let (matrix, is_3d) = transform.to_transform_3d_matrix_f64(Some(&basis)).ok()?;
            let (function, values): (_, &[f64]) = if is_3d {
                ("matrix3d", &matrix.to_array())
            } else {
                (
                    "matrix",
                    &[
                        matrix.m11, matrix.m12, matrix.m21, matrix.m22, matrix.m41, matrix.m42,
                    ],
                )
            };
            let values = values
                .iter()
                .map(|&n| if n == 0.0 { "0".into() } else { n.to_string() })
                .collect::<Vec<_>>()
                .join(", ");
            return Some(format!("{function}({values})"));
        }
        TransformOrigin => {
            let props = &fragment.style.properties;
            let rect = transform_reference_box(fragment);
            let point = reference_point(
                Rect {
                    origin: Point::default(),
                    ..rect
                },
                props.transform_origin,
            );
            let mut result = format!("{}px {}px", point.x, point.y);
            if props.transform_origin_z != 0.0 {
                use std::fmt::Write;
                write!(result, " {}px", props.transform_origin_z).expect("writing CSS to String");
            }
            return Some(result);
        }
        Width | Height
            if fragment.style.stacking.display != DisplayKind::Inline
                || fragment.kind == FragmentKind::Replaced =>
        {
            let size = if *css.get_box_sizing() == CssBoxSizing::BorderBox {
                box_rect.size
            } else {
                inset(inset(box_rect, fragment.border), fragment.padding).size
            };
            if property == Width {
                size.width
            } else {
                size.height
            }
        }
        PaddingTop => css.get_padding().padding_top.0.resolve(content_width).px(),
        PaddingRight => css
            .get_padding()
            .padding_right
            .0
            .resolve(content_width)
            .px(),
        PaddingBottom => css
            .get_padding()
            .padding_bottom
            .0
            .resolve(content_width)
            .px(),
        PaddingLeft => css.get_padding().padding_left.0.resolve(content_width).px(),
        MarginTop | MarginRight | MarginBottom | MarginLeft => {
            if let Some(margin) = geometry.margins.get(&fragment.id) {
                match property {
                    MarginTop => margin.top,
                    MarginRight => margin.right,
                    MarginBottom => margin.bottom,
                    _ => margin.left,
                }
            } else {
                let margin = css.get_margin();
                let side = match property {
                    MarginTop => &margin.margin_top,
                    MarginRight => &margin.margin_right,
                    MarginBottom => &margin.margin_bottom,
                    _ => &margin.margin_left,
                };
                match side {
                    crate::css::stylo::MarginVal::LengthPercentage(length) => {
                        length.resolve(content_width).px()
                    }
                    crate::css::stylo::MarginVal::Auto => 0.0,
                    _ => return None,
                }
            }
        }
        Top | Right | Bottom | Left
            if matches!(
                css.get_box().position,
                CssPosition::Absolute | CssPosition::Fixed
            ) =>
        {
            let margin = geometry
                .margins
                .get(&fragment.id)
                .copied()
                .unwrap_or_default();
            match property {
                Top => box_rect.origin.y - padding_box.origin.y - margin.top,
                Right => padding_box.max_x() - box_rect.max_x() - margin.right,
                Bottom => padding_box.max_y() - box_rect.max_y() - margin.bottom,
                _ => box_rect.origin.x - padding_box.origin.x - margin.left,
            }
        }
        Top | Right | Bottom | Left if css.get_box().position == CssPosition::Relative => {
            let position = css.get_position();
            let (side, opposite, basis) = match property {
                Top => (&position.top, &position.bottom, content_box.size.height),
                Right => (&position.right, &position.left, content_box.size.width),
                Bottom => (&position.bottom, &position.top, content_box.size.height),
                _ => (&position.left, &position.right, content_box.size.width),
            };
            let basis = CSSPixelLength::new(basis);
            match (side, opposite) {
                (InsetVal::LengthPercentage(length), _) => length.resolve(basis).px(),
                (_, InsetVal::LengthPercentage(length)) => -length.resolve(basis).px(),
                _ => 0.0,
            }
        }
        _ => return None,
    };
    Some(format!("{}px", if value == 0.0 { 0.0 } else { value }))
}
