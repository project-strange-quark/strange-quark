// SPDX-License-Identifier: MIT

use std::ops::Range;

use bytemuck::{Pod, Zeroable};
use squark_paint::display_items::{Color, DisplayItem, Frame, Rect, RoundedRect};
use wgpu::util::DeviceExt;

use crate::{SCENE_FORMAT, extent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pipeline {
    Quad,
    Border,
    Shadow,
    Gradient,
    Glyph,
}

impl Pipeline {
    const ALL: [Self; 5] = [
        Self::Quad,
        Self::Border,
        Self::Shadow,
        Self::Gradient,
        Self::Glyph,
    ];

    fn entry_point(self) -> &'static str {
        match self {
            Self::Quad => "quad",
            Self::Border => "border",
            Self::Shadow => "shadow",
            Self::Gradient => "gradient",
            Self::Glyph => "glyph",
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Instance {
    bounds: [f32; 4],
    shape: [f32; 4],
    params: [f32; 4],
    color: [f32; 4],
    second_color: [f32; 4],
    source: [f32; 4],
}

impl Instance {
    fn shape(shape: RoundedRect, color: Color, padding: f32) -> Self {
        let [x, y, w, h] = rect_floats(shape.rect);
        Self {
            bounds: [
                x - padding,
                y - padding,
                w + 2.0 * padding,
                h + 2.0 * padding,
            ],
            shape: [x, y, w, h],
            params: [shape.radius, 0.0, 0.0, 0.0],
            color: color_floats(color),
            ..Self::zeroed()
        }
    }
}

#[derive(Default)]
struct Batches {
    instances: Vec<Instance>,
    draws: Vec<(Pipeline, Range<u32>)>,
}

impl Batches {
    fn push(&mut self, pipeline: Pipeline, item: Instance) {
        let index = self.instances.len() as u32;
        self.instances.push(item);
        if let Some((previous, range)) = self.draws.last_mut()
            && *previous == pipeline
        {
            range.end += 1;
        } else {
            self.draws.push((pipeline, index..index + 1));
        }
    }

    fn from_frame(frame: &Frame) -> Self {
        let mut batches = Self::default();
        for item in &frame.display_list {
            match item {
                DisplayItem::Quad { shape, color } => {
                    batches.push(Pipeline::Quad, Instance::shape(*shape, *color, 1.0));
                }
                DisplayItem::Border {
                    shape,
                    width,
                    color,
                } => {
                    let mut instance = Instance::shape(*shape, *color, 1.0);
                    instance.params[1] = *width;
                    batches.push(Pipeline::Border, instance);
                }
                DisplayItem::Shadow {
                    shape,
                    sigma,
                    color,
                } => {
                    let mut instance = Instance::shape(*shape, *color, sigma * 4.0);
                    instance.params[1] = *sigma;
                    batches.push(Pipeline::Shadow, instance);
                }
                DisplayItem::Gradient { shape, left, right } => {
                    let mut instance = Instance::shape(*shape, *left, 1.0);
                    instance.second_color = color_floats(*right);
                    batches.push(Pipeline::Gradient, instance);
                }
                DisplayItem::GlyphRun { color, glyphs } => {
                    for glyph in glyphs {
                        batches.push(
                            Pipeline::Glyph,
                            Instance {
                                bounds: rect_floats(glyph.dest),
                                color: color_floats(*color),
                                source: glyph.source.map(|value| value as f32),
                                ..Instance::zeroed()
                            },
                        );
                    }
                }
            }
        }
        batches
    }
}

fn rect_floats(rect: Rect) -> [f32; 4] {
    [
        rect.origin.x,
        rect.origin.y,
        rect.size.width,
        rect.size.height,
    ]
}

fn color_floats(color: Color) -> [f32; 4] {
    color.map(|value| f32::from(value) / 255.0)
}

#[derive(Debug)]
pub(crate) struct Primitives {
    bindings: wgpu::BindGroupLayout,
    pipelines: [wgpu::RenderPipeline; Pipeline::ALL.len()],
}

impl Primitives {
    pub(crate) fn new(device: &wgpu::Device) -> Self {
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("squark primitive bindings"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(16),
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("squark primitive layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("../shaders/hello.wgsl"));
        let pipelines = Pipeline::ALL.map(|pipeline| {
            let entry = pipeline.entry_point();
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: size_of::<Instance>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &wgpu::vertex_attr_array![
                            0 => Float32x4, 1 => Float32x4, 2 => Float32x4,
                            3 => Float32x4, 4 => Float32x4, 5 => Float32x4
                        ],
                    })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: SCENE_FORMAT,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleStrip,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        });
        Self {
            bindings,
            pipelines,
        }
    }

    pub(crate) fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame: &Frame,
        [width, height]: [u32; 2],
    ) {
        let batches = Batches::from_frame(frame);
        let [r, g, b, a] = color_floats(frame.clear).map(f64::from);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("squark primitive pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        if batches.instances.is_empty() {
            return;
        }

        let instances = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("squark instances"),
            contents: bytemuck::cast_slice(&batches.instances),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let viewport = [width as f32, height as f32, 0.0, 0.0];
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("squark viewport"),
            contents: bytemuck::bytes_of(&viewport),
            usage: wgpu::BufferUsages::UNIFORM,
        });

        let [atlas_width, atlas_height] = frame.glyph_atlas.size;
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("squark glyph atlas"),
            size: extent(atlas_width, atlas_height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        queue.write_texture(
            atlas.as_image_copy(),
            &frame.glyph_atlas.coverage,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(atlas_width),
                rows_per_image: Some(atlas_height),
            },
            atlas.size(),
        );
        let atlas_view = atlas.create_view(&Default::default());
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("squark scene bindings"),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
            ],
        });
        pass.set_bind_group(0, &bindings, &[]);
        pass.set_vertex_buffer(0, instances.slice(..));
        for (pipeline, range) in batches.draws {
            pass.set_pipeline(&self.pipelines[pipeline as usize]);
            pass.draw(0..4, range);
        }
    }
}

#[cfg(test)]
mod tests {
    use squark_paint::display_items::GlyphAtlas;

    use super::*;

    #[test]
    fn merges_only_adjacent_pipeline_runs() {
        let shape = RoundedRect {
            rect: Rect::new(0.0, 0.0, 10.0, 10.0),
            radius: 2.0,
        };
        let color = [255; 4];
        let quad = || DisplayItem::Quad { shape, color };
        let frame = Frame {
            clear: color,
            display_list: vec![
                quad(),
                quad(),
                DisplayItem::Border {
                    shape,
                    width: 1.0,
                    color,
                },
                quad(),
            ],
            glyph_atlas: GlyphAtlas {
                size: [1, 1],
                coverage: vec![0],
            },
        };
        let batches = Batches::from_frame(&frame);
        assert_eq!(
            batches.draws,
            [
                (Pipeline::Quad, 0..2),
                (Pipeline::Border, 2..3),
                (Pipeline::Quad, 3..4)
            ]
        );
    }
}
