// SPDX-License-Identifier: MIT
use std::collections::{BTreeMap, HashMap};

use rayon::prelude::*;
use squark_assets::{
    DecodedImage, ElementImage, ImageCatalog, Pack, RemoteResources, Resource, Resources,
};
use squark_mirror::Document;
use squark_types::{NodeId, engine::ImageEvent, render::FrameDemand};
use url::Url;

pub(crate) struct ImageLoads {
    remote: RemoteResources<DecodedImage>,
    pending: HashMap<Url, BTreeMap<NodeId, u32>>,
    events: Vec<ImageEvent>,
}

impl ImageLoads {
    pub fn new(pack: &Pack, url: &Url) -> Self {
        Self {
            remote: RemoteResources::from_pack(
                pack,
                url,
                "image/*,*/*;q=0.8",
                DecodedImage::decode,
            ),
            pending: HashMap::new(),
            events: Vec::new(),
        }
    }

    pub fn set_frame_demand(&mut self, demand: FrameDemand) {
        self.remote.set_frame_demand(demand);
    }

    pub fn start(
        &mut self,
        document: &mut Document,
        catalog: &mut ImageCatalog,
        resources: Resources<'_>,
    ) {
        let mut local = Vec::new();
        for source in document.take_image_sources() {
            let Some(src) = source.src else {
                continue;
            };
            let event = |natural_size| ImageEvent {
                target: source.target,
                id: source.generation,
                natural_size,
            };
            let Some(url) = (!src.trim().is_empty())
                .then(|| resources.resolve(&src))
                .flatten()
            else {
                self.events.push(event(None));
                continue;
            };
            // A cached decode remains valid for existing images after revocation.
            // A new src assignment must still resolve against the blob registry.
            let blob = if url.scheme() == "blob" {
                let Some(blob) = resources.resolve_blob(&url) else {
                    document.reject_blob_image(source.target, source.generation);
                    self.events.push(event(None));
                    continue;
                };
                Some(blob)
            } else {
                None
            };
            match catalog.element_state(&url) {
                Some(ElementImage::Finished(Some(size))) => {
                    self.events.push(event(Some(size)));
                    continue;
                }
                Some(ElementImage::Pending) => {}
                None | Some(ElementImage::Finished(None)) => {
                    let resource = match blob {
                        Some((bytes, mime)) => Some(Resource::from_blob(&bytes, mime)),
                        None => resources.get(&url),
                    };
                    if let Some(resource) = resource {
                        local.push((url.clone(), resource));
                    } else if !self.remote.start(url.clone()) {
                        catalog.finish_element(url, None);
                        self.events.push(event(None));
                        continue;
                    }
                    catalog.begin_element(url.clone());
                }
            }
            self.pending
                .entry(url)
                .or_default()
                .insert(source.target, source.generation);
        }
        let decoded: Vec<_> = local
            .into_par_iter()
            .map(|(url, resource)| (url, DecodedImage::decode(resource)))
            .collect();
        for (url, image) in decoded {
            self.finish(catalog, url, image);
        }
    }

    pub fn complete(&mut self, catalog: &mut ImageCatalog) -> bool {
        let finished = self.remote.take_finished();
        let changed = !finished.is_empty();
        for (url, image) in finished {
            self.finish(catalog, url, image);
        }
        changed
    }

    fn finish(&mut self, catalog: &mut ImageCatalog, url: Url, image: Option<DecodedImage>) {
        let waiters = self.pending.remove(&url).unwrap_or_default();
        let size = catalog.finish_element(url, image);
        self.events
            .extend(waiters.into_iter().map(|(target, id)| ImageEvent {
                target,
                id,
                natural_size: size,
            }));
    }

    pub fn take_events(&mut self, document: &Document) -> Vec<ImageEvent> {
        let mut events = std::mem::take(&mut self.events);
        events.retain(|event| document.image_generation(event.target) == Some(event.id));
        events
    }
}
