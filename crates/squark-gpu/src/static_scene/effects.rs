// SPDX-License-Identifier: MIT

use bytemuck::{Pod, Zeroable};
use squark_paint::Filter;
use squark_types::fragment::BlendMode;
use wgpu::util::DeviceExt;

use super::{
    Target,
    color::{premul, srgb},
    texture_entry,
    tiles::PixelRect,
};
use crate::SCENE_FORMAT;

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub(super) struct Params {
    options: [u32; 4],
    values: [f32; 4],
    color: [f32; 4],
}

impl Params {
    pub fn opacity(amount: f32) -> Self {
        Self {
            values: [amount, 0.0, 0.0, 0.0],
            ..Self::zeroed()
        }
    }

    pub fn blend(mode: BlendMode, amount: f32) -> Self {
        let mode = match mode {
            BlendMode::Normal => 0,
            BlendMode::Multiply => 1,
            BlendMode::Screen => 2,
            BlendMode::Overlay => 3,
            BlendMode::Darken => 4,
            BlendMode::Lighten => 5,
            BlendMode::ColorDodge => 6,
            BlendMode::ColorBurn => 7,
            BlendMode::HardLight => 8,
            BlendMode::SoftLight => 9,
            BlendMode::Difference => 10,
            BlendMode::Exclusion => 11,
            BlendMode::Hue => 12,
            BlendMode::Saturation => 13,
            BlendMode::Color => 14,
            BlendMode::Luminosity => 15,
            BlendMode::PlusLighter => 16,
        };
        Self {
            options: [4, mode, 0, 0],
            ..Self::opacity(amount)
        }
    }

    pub fn backdrop(amount: f32) -> Self {
        Self {
            options: [5, 0, 0, 0],
            ..Self::opacity(amount)
        }
    }

    pub fn with_mask(mut self, enabled: bool) -> Self {
        self.options[3] = enabled as u32;
        self
    }
}

#[derive(Debug)]
pub(super) struct Effects {
    bindings: wgpu::BindGroupLayout,
    replace: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
}

impl Effects {
    pub fn new(device: &wgpu::Device) -> Self {
        let bindings = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("squark effect bindings"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        min_binding_size: wgpu::BufferSize::new(size_of::<Params>() as u64),
                        has_dynamic_offset: false,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("squark effect pipeline layout"),
            bind_group_layouts: &[Some(&bindings)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("squark effect shader"),
            source: wgpu::ShaderSource::Wgsl(
                format!(
                    "{}\n{}",
                    crate::TEXT_COVERAGE_WGSL,
                    include_str!("../../shaders/effects.wgsl")
                )
                .into(),
            ),
        });
        let pipeline = |label, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("draw"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: SCENE_FORMAT,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        Self {
            replace: pipeline("squark effect replace", None),
            over: pipeline(
                "squark effect over",
                Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            ),
            bindings,
        }
    }

    pub fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        inputs: [&wgpu::TextureView; 3],
        params: Params,
        over: bool,
    ) {
        self.draw_in(device, encoder, target, inputs, params, over, None);
    }

    /// Like `draw`, limited to `area` of the target when one is given.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_in(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        target: &Target,
        inputs: [&wgpu::TextureView; 3],
        params: Params,
        over: bool,
        area: Option<PixelRect>,
    ) {
        let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("squark effect parameters"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("squark effect inputs"),
            layout: &self.bindings,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(inputs[0]),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(inputs[1]),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(inputs[2]),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: uniform.as_entire_binding(),
                },
            ],
        });
        let mut pass = target.pass(
            encoder,
            if over {
                wgpu::LoadOp::Load
            } else {
                wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
            },
        );
        if let Some([x, y, width, height]) = area {
            pass.set_scissor_rect(x, y, width, height);
        }
        pass.set_pipeline(if over { &self.over } else { &self.replace });
        pass.set_bind_group(0, &bind, &[]);
        pass.draw(0..3, 0..1);
    }

    pub fn filter<'a>(
        &'a self,
        device: &'a wgpu::Device,
        encoder: &'a mut wgpu::CommandEncoder,
        source: Target,
        size: [u32; 2],
        scale: f32,
        backdrop: bool,
    ) -> Filters<'a> {
        Filters {
            effects: self,
            device,
            encoder,
            source,
            size,
            scale,
            backdrop,
        }
    }

    pub fn canvas_text_coverage(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        mask: wgpu::Texture,
        paint: squark_canvas::TextPaint,
    ) -> wgpu::Texture {
        let mut encoder = device.create_command_encoder(&Default::default());
        let coverage = mask.create_view(&Default::default());
        let target = Target::new(device, [mask.width(), mask.height()]);
        let (params, colors) = match paint {
            squark_canvas::TextPaint::Solid(color) => (
                Params {
                    options: [8, 0, 0, 0],
                    color,
                    ..Params::zeroed()
                },
                coverage.clone(),
            ),
            squark_canvas::TextPaint::Texture(texture) => (
                Params {
                    options: [8, 1, 0, 0],
                    ..Params::zeroed()
                },
                texture.create_view(&Default::default()),
            ),
        };
        self.draw(
            device,
            &mut encoder,
            &target,
            [&coverage, &colors, &coverage],
            params,
            false,
        );
        queue.submit([encoder.finish()]);
        target.texture
    }

    pub fn canvas_filter(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        source: wgpu::Texture,
        filters: &[squark_types::fragment::Filter],
    ) -> wgpu::Texture {
        let size = [source.width(), source.height()];
        let mut encoder = device.create_command_encoder(&Default::default());
        let view = source.create_view(&Default::default());
        let premultiplied = Target::new(device, size);
        self.draw(
            device,
            &mut encoder,
            &premultiplied,
            [&view; 3],
            Params {
                options: [6, 0, 0, 0],
                ..Params::zeroed()
            },
            false,
        );
        let filters: Vec<_> = filters.iter().copied().map(Filter::from).collect();
        let filtered = self
            .filter(device, &mut encoder, premultiplied, size, 1.0, false)
            .apply(&filters);
        let straight = Target::new(device, size);
        self.draw(
            device,
            &mut encoder,
            &straight,
            [&filtered.view; 3],
            Params {
                options: [7, 0, 0, 0],
                ..Params::zeroed()
            },
            false,
        );
        queue.submit([encoder.finish()]);
        straight.texture
    }
}

pub(super) struct Filters<'a> {
    effects: &'a Effects,
    device: &'a wgpu::Device,
    encoder: &'a mut wgpu::CommandEncoder,
    source: Target,
    size: [u32; 2],
    scale: f32,
    backdrop: bool,
}

impl Filters<'_> {
    pub fn apply(mut self, filters: &[Filter]) -> Target {
        for operation in filters {
            match *operation {
                Filter::Blur(sigma) => self.source = self.blur(sigma),
                Filter::DropShadow(shadow) | Filter::TextShadow(shadow) => {
                    let shadow_only = matches!(operation, Filter::TextShadow(_));
                    let sigma = if shadow_only {
                        shadow.blur * 0.5
                    } else {
                        shadow.blur
                    };
                    let blurred = self.blur(sigma);
                    let params = Params {
                        options: [3, shadow_only as u32, 0, 0],
                        values: [
                            shadow.offset.x * self.scale,
                            shadow.offset.y * self.scale,
                            0.0,
                            0.0,
                        ],
                        color: premul(srgb(shadow.color)),
                    };
                    self.source = self.pass(params, &blurred.view);
                }
                _ => {
                    let (kind, value) = match *operation {
                        Filter::Brightness(v) => (1, v.max(0.0)),
                        Filter::Contrast(v) => (2, v.max(0.0)),
                        Filter::Grayscale(v) => (3, v.clamp(0.0, 1.0)),
                        Filter::HueRotate(v) => (4, v),
                        Filter::Invert(v) => (5, v.clamp(0.0, 1.0)),
                        Filter::Opacity(v) => (6, v),
                        Filter::Saturate(v) => (7, v.max(0.0)),
                        Filter::Sepia(v) => (8, v.clamp(0.0, 1.0)),
                        Filter::Blur(_) | Filter::DropShadow(_) | Filter::TextShadow(_) => {
                            unreachable!()
                        }
                    };
                    self.source = self.pass(
                        Params {
                            options: [1, kind, 0, 0],
                            ..Params::opacity(value)
                        },
                        &self.source.view.clone(),
                    );
                }
            }
        }
        self.source
    }

    fn blur(&mut self, sigma: f32) -> Target {
        if sigma <= 0.0 {
            return self.source.clone();
        }
        let mut params = Params {
            options: [2, 0, self.backdrop as u32, 0],
            ..Params::opacity(sigma * self.scale)
        };
        let horizontal = self.pass(params, &self.source.view.clone());
        params.options[1] = 1;
        let vertical = Target::new(self.device, self.size);
        self.effects.draw(
            self.device,
            self.encoder,
            &vertical,
            [&horizontal.view; 3],
            params,
            false,
        );
        vertical
    }

    fn pass(&mut self, params: Params, secondary: &wgpu::TextureView) -> Target {
        let target = Target::new(self.device, self.size);
        self.effects.draw(
            self.device,
            self.encoder,
            &target,
            [&self.source.view, secondary, secondary],
            params,
            false,
        );
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{StaticRenderer, readback_rgba};
    use squark_canvas::TextPaint;

    #[test]
    fn canvas_text_uses_the_foreground_to_correct_glyph_coverage() {
        if std::env::var("SQUARK_GPU_TESTS").as_deref() != Ok("1") {
            return;
        }
        let renderer = pollster::block_on(StaticRenderer::headless()).unwrap();
        let device = renderer.device();
        let queue = renderer.queue();
        let upload = |rgba: [u8; 8]| {
            let source = Target::new(device, [2, 1]);
            queue.write_texture(
                source.texture.as_image_copy(),
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(8),
                    rows_per_image: None,
                },
                source.texture.size(),
            );
            source.texture
        };
        let mask = upload([255, 255, 255, 128, 255, 255, 255, 128]);
        let black = renderer.effects.canvas_text_coverage(
            device,
            queue,
            mask.clone(),
            TextPaint::Solid([0.0, 0.0, 0.0, 1.0]),
        );
        assert_eq!(
            readback_rgba(device, queue, &black).unwrap(),
            [0, 0, 0, 125].repeat(2)
        );

        let paint = upload([0, 0, 0, 255, 255, 255, 255, 128]);
        let colored =
            renderer
                .effects
                .canvas_text_coverage(device, queue, mask, TextPaint::Texture(paint));
        assert_eq!(
            readback_rgba(device, queue, &colored).unwrap(),
            [0, 0, 0, 125, 255, 255, 255, 72],
        );
    }
}
