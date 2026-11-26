// SPDX-License-Identifier: MIT

use std::collections::{HashMap, HashSet};

use squark_paint::{DisplayItem, InversePlane, Matrix, PropertyTrees, TransformId};
use squark_types::{Rect, fragment::FragmentId};

#[derive(Clone, Copy)]
struct Plane {
    context: TransformId,
    transform: TransformId,
}

pub(super) fn paint_order(
    items: &[DisplayItem],
    properties: &PropertyTrees,
    matrices: &[Matrix],
    warned: &mut HashSet<FragmentId>,
) -> Vec<usize> {
    let mut planes: Vec<Option<Plane>> = vec![None; matrices.len()];
    for (i, node) in properties.transforms.iter().enumerate().skip(1) {
        let parent = node.parent.0 as usize;
        let transform = TransformId(i as u32);
        planes[i] = if properties.transforms[parent].preserve_3d {
            let context = planes[parent].map_or(node.parent, |plane| plane.context);
            Some(Plane { context, transform })
        } else if node.preserve_3d && planes[parent].is_none() {
            Some(Plane {
                context: transform,
                transform,
            })
        } else {
            planes[parent]
        };
    }
    let mut order: Vec<_> = (0..items.len()).collect();
    if !planes.iter().any(Option::is_some) {
        return order;
    }
    let crossing = intersections(items, matrices, &planes);
    for &context in &crossing {
        let node = &properties.transforms[context.0 as usize];
        if let Some(fragment) = node.fragment
            && warned.insert(fragment)
        {
            eprintln!(
                "squark-gpu: intersecting preserve-3d planes in stacking context {} (node {}), using CSS paint order",
                fragment.0,
                node.owner.raw(),
            );
        }
    }

    let mut depth = vec![None; matrices.len()];
    let mut start = 0;
    while start < items.len() {
        let item = &items[start];
        let Some(plane) = planes[item.properties.transform.0 as usize] else {
            start += 1;
            continue;
        };
        let effect = item.properties.effect;
        let mut end = start + 1;
        while end < items.len()
            && items[end].properties.effect == effect
            && planes[items[end].properties.transform.0 as usize]
                .is_some_and(|other| other.context == plane.context)
        {
            end += 1;
        }
        if !crossing.contains(&plane.context) {
            for item in &items[start..end] {
                let plane = planes[item.properties.transform.0 as usize].unwrap();
                let transform = plane.transform.0 as usize;
                depth[transform].get_or_insert_with(|| {
                    let x = item.bounds.origin.x + item.bounds.size.width * 0.5;
                    let y = item.bounds.origin.y + item.bounds.size.height * 0.5;
                    let m = matrices[transform];
                    let z = m[0][2] * x + m[1][2] * y + m[3][2];
                    let w = m[0][3] * x + m[1][3] * y + m[3][3];
                    z / w.max(1e-6)
                });
            }
            let z = |index: usize| {
                let plane = planes[items[index].properties.transform.0 as usize].unwrap();
                depth[plane.transform.0 as usize].unwrap()
            };
            order[start..end].sort_by(|&a, &b| z(a).total_cmp(&z(b)));
        }
        start = end;
    }
    order
}

type Point = [f64; 2];

struct Surface {
    transform: TransformId,
    polygon: Vec<Point>,
    /// Z/W is affine in projected X/W and Y/W for a single plane.
    depth: [f64; 3],
}

fn intersections(
    items: &[DisplayItem],
    matrices: &[Matrix],
    planes: &[Option<Plane>],
) -> HashSet<TransformId> {
    let mut groups: HashMap<_, Vec<Surface>> = HashMap::new();
    let mut crossing = HashSet::new();
    for item in items {
        let transform = item.properties.transform.0 as usize;
        let Some(plane) = planes[transform] else {
            continue;
        };
        if crossing.contains(&plane.context) {
            continue;
        }
        let world = matrices[plane.transform.0 as usize];
        let Some(InversePlane(inverse)) = InversePlane::new(world) else {
            continue;
        };
        let z = [world[0][2], world[1][2], world[3][2]].map(f64::from);
        let depth = std::array::from_fn(|column| (0..3).map(|i| z[i] * inverse[i][column]).sum());
        let polygon = project(item.bounds, matrices[transform]);
        if polygon.len() < 3 || area(&polygon).abs() < 1e-6 {
            continue;
        }
        let surface = Surface {
            transform: plane.transform,
            polygon,
            depth,
        };
        let siblings = groups.entry(plane.context).or_default();
        if siblings
            .iter()
            .any(|other| other.transform != surface.transform && intersects(other, &surface))
        {
            crossing.insert(plane.context);
            siblings.clear();
        } else {
            siblings.push(surface);
        }
    }
    crossing
}

fn project(rect: Rect, matrix: Matrix) -> Vec<Point> {
    let m = matrix.map(|column| column.map(f64::from));
    let [x, y, right, bottom] =
        [rect.origin.x, rect.origin.y, rect.max_x(), rect.max_y()].map(f64::from);
    let corners = [[x, y], [right, y], [right, bottom], [x, bottom]]
        .map(|[x, y]| [0, 1, 3].map(|row| m[0][row] * x + m[1][row] * y + m[3][row]));
    clip(&corners, |p| p[2] - 1e-6)
        .into_iter()
        .map(|[x, y, w]| [x / w, y / w])
        .collect()
}

fn intersects(a: &Surface, b: &Surface) -> bool {
    let mut overlap = a.polygon.clone();
    let orientation = area(&b.polygon).signum();
    for (start, end) in edges(&b.polygon) {
        overlap = clip(&overlap, |point| {
            orientation * cross(delta(end, start), delta(point, start))
        });
        if overlap.len() < 3 {
            return false;
        }
    }
    if area(&overlap).abs() < 1e-6 {
        return false;
    }
    let [x, y, b] = std::array::from_fn(|i| a.depth[i] - b.depth[i]);
    let mut above = false;
    let mut below = false;
    for [px, py] in overlap {
        let delta = x * px + y * py + b;
        above |= delta > 1e-4;
        below |= delta < -1e-4;
    }
    above && below
}

fn edges(polygon: &[Point]) -> impl Iterator<Item = (Point, Point)> {
    polygon
        .iter()
        .copied()
        .zip(polygon.iter().copied().cycle().skip(1))
}

fn area(polygon: &[Point]) -> f64 {
    edges(polygon).map(|(p, q)| cross(p, q)).sum::<f64>() * 0.5
}

fn delta(a: Point, b: Point) -> Point {
    [a[0] - b[0], a[1] - b[1]]
}

fn cross(a: Point, b: Point) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

fn clip<const N: usize>(polygon: &[[f64; N]], distance: impl Fn([f64; N]) -> f64) -> Vec<[f64; N]> {
    let Some(&last) = polygon.last() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut previous = last;
    let mut previous_distance = distance(previous);
    for &point in polygon {
        let next_distance = distance(point);
        if (previous_distance < 0.0) != (next_distance < 0.0) {
            let t = previous_distance / (previous_distance - next_distance);
            out.push(std::array::from_fn(|i| {
                previous[i] + (point[i] - previous[i]) * t
            }));
        }
        if next_distance >= 0.0 {
            out.push(point);
        }
        previous = point;
        previous_distance = next_distance;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDENTITY: Matrix = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    #[test]
    fn only_depth_crossings_inside_the_projected_overlap_trigger_fallback() {
        let a = Surface {
            transform: TransformId(1),
            polygon: project(Rect::new(0.0, 0.0, 10.0, 10.0), IDENTITY),
            depth: [0.0; 3],
        };
        let mut b = Surface {
            transform: TransformId(2),
            polygon: project(Rect::new(3.0, 0.0, 8.0, 10.0), IDENTITY),
            depth: [1.0, 0.0, -6.0],
        };
        assert!(intersects(&a, &b));
        b.depth[2] = -1.0;
        assert!(!intersects(&a, &b));
        b.depth = [0.0; 3];
        assert!(!intersects(&a, &b));
        b.depth = [1.0, 0.0, -6.0];
        b.polygon = project(Rect::new(3.0, 11.0, 8.0, 10.0), IDENTITY);
        assert!(!intersects(&a, &b));
    }
}
