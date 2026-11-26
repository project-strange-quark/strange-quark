// SPDX-License-Identifier: MIT

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use rayon::prelude::*;
use squark_assets::{ImageCatalog, Resources};
use squark_text::{Fonts, GlyphRasterizer};
use squark_types::{
    engine::EngineError,
    fragment::{BackgroundImage, Fragment, FragmentContent},
    render::{
        Bitmap, BitmapFormat, FontInstanceId, GlyphKey, ImageKey, ResourceKey, ResourceSender,
        ResourceUpdate,
    },
};

use crate::LayoutEngine;

pub(crate) struct Uploads {
    sender: ResourceSender,
    sent: HashSet<ResourceKey>,
    sent_fonts: HashSet<FontInstanceId>,
    font_bytes: HashMap<u64, Arc<[u8]>>,
}

impl Uploads {
    pub fn new(sender: ResourceSender) -> Self {
        Self {
            sender,
            sent: HashSet::new(),
            sent_fonts: HashSet::new(),
            font_bytes: HashMap::new(),
        }
    }

    pub fn send(&self, update: ResourceUpdate) -> Result<(), EngineError> {
        self.sender
            .send(update)
            .map(|_| ())
            .map_err(|error| EngineError::Resource(error.to_string()))
    }

    pub fn canvas_fonts(
        &mut self,
        fonts: &Fonts,
        used: Vec<FontInstanceId>,
    ) -> Result<(), EngineError> {
        for id in used {
            if self.sent_fonts.contains(&id) {
                continue;
            }
            let font = fonts
                .instance(id)
                .ok_or_else(|| EngineError::Resource(format!("missing canvas font {}", id.0)))?;
            let data = &font.font.data;
            let bytes = self
                .font_bytes
                .entry(data.id())
                .or_insert_with(|| Arc::from(data.as_ref()))
                .clone();
            self.send(ResourceUpdate::PutFont {
                id,
                bytes,
                index: font.font.index,
                size_bits: font.size.to_bits(),
                coords: font.coords.as_slice().into(),
            })?;
            self.sent_fonts.insert(id);
        }
        Ok(())
    }

    pub fn canvas_images(
        &mut self,
        catalog: &mut ImageCatalog,
        used: Vec<ImageKey>,
    ) -> Result<(), EngineError> {
        for key in used {
            if self.sent.contains(&ResourceKey::Image(key)) {
                continue;
            }
            let bitmap = catalog.bitmap(key).cloned().ok_or_else(|| {
                EngineError::Resource(format!("missing decoded canvas image {}", key.0))
            })?;
            self.send_image(catalog, key, bitmap)?;
        }
        Ok(())
    }

    fn send_image(
        &mut self,
        catalog: &mut ImageCatalog,
        key: ImageKey,
        bitmap: Bitmap,
    ) -> Result<(), EngineError> {
        self.send(ResourceUpdate::PutImage { key, bitmap })?;
        self.sent.insert(ResourceKey::Image(key));
        catalog.did_upload(key);
        Ok(())
    }

    pub fn prepare<'a>(
        &mut self,
        fragments: impl Iterator<Item = &'a Fragment>,
        layout: &mut LayoutEngine,
        resources: Resources<'_>,
        dpr: f64,
    ) -> Result<(), EngineError> {
        let mut glyphs = Vec::new();
        let mut images = Vec::new();
        let mut pending = HashSet::new();
        let catalog = layout.images();
        let mut add_image = |key: ImageKey, pending: &mut HashSet<ResourceKey>| {
            let resource = ResourceKey::Image(key);
            if !self.sent.contains(&resource) && pending.insert(resource) {
                images.push(key);
            }
        };
        for fragment in fragments {
            for layer in &fragment.style.box_paint.backgrounds {
                if let BackgroundImage::Image(key) = layer.image {
                    add_image(key, &mut pending);
                }
            }
            match &fragment.content {
                FragmentContent::Image { key, .. } => add_image(*key, &mut pending),
                FragmentContent::Text(runs) => {
                    for run in runs {
                        let font = layout.fonts().instance(run.font).ok_or_else(|| {
                            EngineError::Resource(format!("missing font {}", run.font.0))
                        })?;
                        for glyph in &run.glyphs {
                            let key = GlyphKey::for_position(
                                run.font,
                                glyph.glyph_id,
                                glyph.position,
                                dpr,
                            );
                            let resource = ResourceKey::Glyph(key);
                            if !self.sent.contains(&resource) && pending.insert(resource) {
                                glyphs.push((key, font));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let (glyphs, images): (Vec<_>, Vec<_>) = rayon::join(
            || {
                glyphs
                    .into_par_iter()
                    .map_init(GlyphRasterizer::default, |raster, (key, font)| {
                        (ResourceKey::Glyph(key), raster.rasterize(key, font))
                    })
                    .collect()
            },
            || {
                images
                    .into_par_iter()
                    .map(|key| {
                        let bitmap = catalog
                            .bitmap(key)
                            .cloned()
                            .or_else(|| {
                                catalog
                                    .url(key)
                                    .and_then(|url| resources.get(url))
                                    .and_then(|resource| resource.decode_image())
                            })
                            .unwrap_or_else(transparent_image);
                        (key, bitmap)
                    })
                    .collect()
            },
        );
        // Install the exact prefix this snapshot needs before paint can read it.
        for (key, update) in glyphs {
            self.send(update)?;
            self.sent.insert(key);
        }
        for (key, bitmap) in images {
            self.send_image(layout.images_mut(), key, bitmap)?;
        }
        Ok(())
    }
}

fn transparent_image() -> Bitmap {
    Bitmap {
        size: [1, 1],
        stride: 4,
        format: BitmapFormat::Rgba8SrgbPremultiplied,
        pixels: [0; 4].into(),
    }
}
