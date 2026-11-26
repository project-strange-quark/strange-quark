// SPDX-License-Identifier: MIT

use guillotiere::{SimpleAtlasAllocator, size2};
use squark_paint::{ClipShape, FillRule, Frame, GradientGeometry, Matrix, PathBrush, Primitive};
use squark_types::Rect;
use vello::{
    kurbo::{Affine, BezPath, Shape, Vec2},
    peniko::{Brush, Color, ColorStop, Extend, Fill, Gradient},
};

use super::color::{dynamic, hue, srgb, tag};
use crate::{Error, SCENE_FORMAT, extent};

#[derive(Clone, Copy, Debug)]
pub(super) struct Sprite {
    pub bounds: Rect,
    pub texels: [u32; 4],
    pub aligned: bool,
}

#[derive(Debug)]
pub(super) struct Paths {
    pub view: wgpu::TextureView,
    pub clips: Vec<Option<Sprite>>,
    pub items: Vec<Option<Sprite>>,
}

#[derive(Clone, Copy)]
enum Source<'a> {
    Clip(&'a BezPath, FillRule),
    Item(&'a squark_paint::PathPaint),
}

impl Source<'_> {
    fn bounds(self) -> vello::kurbo::Rect {
        match self {
            Self::Clip(path, _) => path.bounding_box(),
            Self::Item(item) => {
                let bounds = item.path.bounding_box();
                let stroke = item.stroke.as_ref().map_or(0.0, |(_, stroke)| {
                    stroke.width * stroke.miter_limit.max(1.0) * 0.5
                });
                bounds.inflate(stroke, stroke)
            }
        }
    }

    fn draw(self, scene: &mut vello::Scene, matrix: Affine) {
        match self {
            Self::Clip(path, rule) => scene.fill(fill(rule), matrix, Color::WHITE, None, path),
            Self::Item(item) => {
                if let Some((value, rule)) = &item.fill {
                    let (brush, transform) = brush(value);
                    scene.fill(fill(*rule), matrix, &brush, transform, item.path.as_ref());
                }
                if let Some((value, stroke)) = &item.stroke {
                    let (brush, transform) = brush(value);
                    scene.stroke(stroke, matrix, &brush, transform, item.path.as_ref());
                }
            }
        }
    }
}

fn brush(value: &PathBrush) -> (Brush, Option<Affine>) {
    let PathBrush::Gradient(value) = value else {
        let PathBrush::Color(color) = value else {
            unreachable!()
        };
        return (Color::new(srgb(*color)).into(), None);
    };
    let first = value.stops.first().map_or(0.0, |stop| stop.0);
    let last = value.stops.last().map_or(1.0, |stop| stop.0);
    if first >= last {
        let rgba = value.stops.last().map_or([0.0; 4], |stop| srgb(stop.1));
        return (Color::new(rgba).into(), None);
    }
    let point = |p: squark_types::Point| (f64::from(p.x), f64::from(p.y));
    let (gradient, transform) = match value.geometry {
        GradientGeometry::Linear { start, end } => {
            let along = |t: f32| {
                (
                    f64::from(start.x + (end.x - start.x) * t),
                    f64::from(start.y + (end.y - start.y) * t),
                )
            };
            (Gradient::new_linear(along(first), along(last)), None)
        }
        GradientGeometry::Radial { center, radii } => {
            let center = point(center);
            let rx = radii.width.max(1e-5);
            let scale = Affine::translate(center)
                * Affine::scale_non_uniform(1.0, f64::from(radii.height.max(1e-5) / rx))
                * Affine::translate((-center.0, -center.1));
            (
                Gradient::new_two_point_radial(
                    center,
                    rx * first.max(0.0),
                    center,
                    rx * last.max(1e-5),
                ),
                Some(scale),
            )
        }
        GradientGeometry::Conic {
            center,
            start_angle,
        } => {
            let start = start_angle - std::f32::consts::FRAC_PI_2;
            let angle = |position| start + position * std::f32::consts::TAU;
            (
                Gradient::new_sweep(point(center), angle(first), angle(last)),
                None,
            )
        }
    };
    let stops: Vec<_> = value
        .stops
        .iter()
        .map(|(position, color)| ColorStop {
            offset: (position - first) / (last - first),
            color: dynamic(*color),
        })
        .collect();
    let spread = if value.repeating {
        Extend::Repeat
    } else {
        Extend::Pad
    };
    let gradient = gradient
        .with_stops(stops.as_slice())
        .with_extend(spread)
        .with_interpolation_cs(tag(value.interpolation))
        .with_hue_direction(hue(value.hue));
    (gradient.into(), transform)
}

fn fill(rule: FillRule) -> Fill {
    match rule {
        FillRule::NonZero => Fill::NonZero,
        FillRule::EvenOdd => Fill::EvenOdd,
    }
}

struct Job<'a> {
    source: Source<'a>,
    index: usize,
    bounds: vello::kurbo::Rect,
    size: [u32; 2],
    pixel_grid: Affine,
    origin: Vec2,
    aligned: bool,
}

impl Paths {
    pub fn prepare(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Option<vello::Renderer>,
        placeholder: &wgpu::TextureView,
        frame: &Frame,
        scale: f64,
        matrices: &[Matrix],
    ) -> Result<Self, Error> {
        let mut paths = Self {
            view: placeholder.clone(),
            clips: vec![None; frame.property_trees.clips.len()],
            items: vec![None; frame.display_list.len()],
        };
        let clips = frame
            .property_trees
            .clips
            .iter()
            .enumerate()
            .filter_map(|(index, clip)| {
                if let Some(ClipShape::Path(path, rule)) = &clip.shape {
                    Some((index, Source::Clip(path, *rule), clip.space.0))
                } else {
                    None
                }
            });
        let items = frame
            .display_list
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                if let Primitive::Path(path) = &item.primitive {
                    Some((index, Source::Item(path), item.properties.transform.0))
                } else {
                    None
                }
            });
        let limit = device.limits().max_texture_dimension_2d;
        let mut jobs: Vec<_> = clips
            .chain(items)
            .filter_map(|(index, source, space)| {
                let bounds = source.bounds();
                if bounds.width() <= 0.0 || bounds.height() <= 0.0 || !bounds.is_finite() {
                    return None;
                }
                let aligned = path_pixel_grid(matrices[space as usize], scale);
                let allowed = f64::from(limit - 4);
                let grid = aligned.filter(|grid| {
                    let device_bounds = grid.transform_rect_bbox(bounds);
                    device_bounds.width().max(device_bounds.height()) <= allowed
                });
                let pixel_grid = grid.unwrap_or_else(|| {
                    let scale = scale.min(allowed / bounds.width().max(bounds.height()));
                    Affine::scale(scale)
                });
                let raster = pixel_grid.transform_rect_bbox(bounds);
                let origin = Vec2::new(raster.x0.floor() - 1.0, raster.y0.floor() - 1.0);
                let size = [
                    (raster.x1.ceil() + 1.0 - origin.x) as u32,
                    (raster.y1.ceil() + 1.0 - origin.y) as u32,
                ];
                let bounds = pixel_grid
                    .inverse()
                    .transform_rect_bbox(vello::kurbo::Rect::new(
                        origin.x,
                        origin.y,
                        origin.x + f64::from(size[0]),
                        origin.y + f64::from(size[1]),
                    ));
                Some(Job {
                    source,
                    index,
                    bounds,
                    size,
                    pixel_grid,
                    origin,
                    aligned: grid.is_some(),
                })
            })
            .collect();
        if jobs.is_empty() {
            return Ok(paths);
        }
        jobs.sort_by_key(|job| std::cmp::Reverse(job.size[0].max(job.size[1])));

        let mut side = jobs[0].size[0]
            .max(jobs[0].size[1])
            .max(32)
            .next_power_of_two()
            .min(limit);
        let placements = loop {
            let mut allocator = SimpleAtlasAllocator::new(size2(side as i32, side as i32));
            let placements: Option<Vec<_>> = jobs
                .iter()
                .map(|job| allocator.allocate(size2(job.size[0] as i32, job.size[1] as i32)))
                .collect();
            if let Some(placements) = placements {
                break placements;
            }
            if side == limit {
                return Err("frame paths exceed the Vello atlas extent".into());
            }
            side = (side * 2).min(limit);
        };
        let width = placements
            .iter()
            .map(|rect| rect.max.x as u32)
            .max()
            .unwrap();
        let height = placements
            .iter()
            .map(|rect| rect.max.y as u32)
            .max()
            .unwrap();
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squark Vello path atlas"),
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCENE_FORMAT,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        paths.view = atlas.create_view(&Default::default());
        let mut scene = vello::Scene::new();
        for (job, placement) in jobs.into_iter().zip(placements) {
            let origin = placement.min;
            let bounds = job.bounds;
            let matrix = Affine::translate((
                f64::from(origin.x) - job.origin.x,
                f64::from(origin.y) - job.origin.y,
            )) * job.pixel_grid;
            job.source.draw(&mut scene, matrix);
            let sprite = Some(Sprite {
                bounds: Rect::new(
                    bounds.x0 as f32,
                    bounds.y0 as f32,
                    bounds.width() as f32,
                    bounds.height() as f32,
                ),
                texels: [origin.x as u32, origin.y as u32, job.size[0], job.size[1]],
                aligned: job.aligned,
            });
            match job.source {
                Source::Clip(..) => paths.clips[job.index] = sprite,
                Source::Item(..) => paths.items[job.index] = sprite,
            }
        }
        if renderer.is_none() {
            let _pipelines = crate::StartupSpan::new("vello-paths-pipelines");
            *renderer = Some(vello::Renderer::new(
                device,
                squark_canvas::vello_options(device),
            )?);
        }
        renderer
            .as_mut()
            .expect("Vello is initialized")
            .render_to_texture(
                device,
                queue,
                &scene,
                &paths.view,
                &vello::RenderParams {
                    base_color: Color::TRANSPARENT,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )?;
        Ok(paths)
    }
}

/// Rasterize axis-aligned paths on the root device grid. Integer world
/// translations affect only placement, so the atlas needs the phase alone.
fn path_pixel_grid(world: Matrix, scale: f64) -> Option<Affine> {
    let w = f64::from(world[3][3]);
    if world[0][1] != 0.0
        || world[1][0] != 0.0
        || world[0][3] != 0.0
        || world[1][3] != 0.0
        || w <= 0.0
        || world[0][0] <= 0.0
        || world[1][1] <= 0.0
    {
        return None;
    }
    let scale = scale / w;
    let phase = (
        (f64::from(world[3][0]) * scale).rem_euclid(1.0),
        (f64::from(world[3][1]) * scale).rem_euclid(1.0),
    );
    let grid = Affine::translate(phase)
        * Affine::scale_non_uniform(
            f64::from(world[0][0]) * scale,
            f64::from(world[1][1]) * scale,
        );
    grid.is_finite().then_some(grid)
}
