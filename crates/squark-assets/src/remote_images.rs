// SPDX-License-Identifier: MIT
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, Mutex, Weak, mpsc},
};

use squark_net::{Allowlist, Event, Redirect, Request, Transport};
use squark_types::{pack::MANIFEST_PATH, render::FrameDemand};
use url::Url;

use crate::{Pack, Resource};

const CREDIT: usize = 64 * 1024;
// Bound encoded input by the same allocation budget as the image decoder.
const MAX_ENCODED_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug)]
pub struct RemoteResources<T> {
    policy: Allowlist,
    demand: FrameDemand,
    client: Option<Client<T>>,
    decode: fn(Resource<'_>) -> Option<T>,
    accept: &'static str,
}

#[derive(Debug)]
struct Client<T> {
    transport: Arc<Mutex<Transport>>,
    results: mpsc::Receiver<(u32, Option<T>)>,
    urls: HashMap<u32, Url>,
    next: u32,
}

impl<T: Send + 'static> RemoteResources<T> {
    pub fn from_pack(
        pack: &Pack,
        document_url: &Url,
        accept: &'static str,
        decode: fn(Resource<'_>) -> Option<T>,
    ) -> Self {
        let manifest = pack
            .entry(MANIFEST_PATH)
            .and_then(|asset| serde_json::from_slice::<serde_json::Value>(asset.bytes).ok());
        let allowed = manifest
            .as_ref()
            .and_then(|m| m.pointer("/network/allow"))
            .and_then(|entries| serde_json::from_value::<Vec<String>>(entries.clone()).ok())
            .unwrap_or_default();
        let dev = manifest
            .as_ref()
            .and_then(|m| m.pointer("/dev/origin"))
            .and_then(serde_json::Value::as_str)
            .and_then(|origin| Url::parse(origin).ok())
            .filter(|dev| dev.origin() == document_url.origin());
        Self {
            policy: Allowlist::new(&allowed, dev),
            demand: FrameDemand::default(),
            client: None,
            decode,
            accept,
        }
    }

    pub fn set_frame_demand(&mut self, demand: FrameDemand) {
        self.demand = demand;
    }

    pub fn start(&mut self, url: Url) -> bool {
        if !matches!(url.scheme(), "http" | "https") || !self.policy.permits(&url) {
            return false;
        }
        let client = match self.client.as_mut() {
            Some(client) => client,
            None => {
                let Some(client) =
                    Client::new(self.policy.clone(), self.demand.clone(), self.decode)
                else {
                    return false;
                };
                self.client.insert(client)
            }
        };
        let id = client.next;
        let Some(next) = id.checked_add(1) else {
            return false;
        };
        client.next = next;
        client.urls.insert(id, url.clone());
        client.transport.lock().expect("asset transport").fetch(
            id,
            Request {
                url,
                method: "GET".into(),
                headers: vec![("accept".into(), self.accept.into())],
                body: None,
                redirect: Redirect::Follow,
            },
        );
        true
    }

    pub fn take_finished(&mut self) -> Vec<(Url, Option<T>)> {
        let Some(client) = self.client.as_mut() else {
            return Vec::new();
        };
        client
            .results
            .try_iter()
            .filter_map(|(id, image)| Some((client.urls.remove(&id)?, image)))
            .collect()
    }
}

impl<T: Send + 'static> Client<T> {
    fn new(
        policy: Allowlist,
        demand: FrameDemand,
        decode: fn(Resource<'_>) -> Option<T>,
    ) -> Option<Self> {
        let (events, incoming) = mpsc::channel();
        let (send, results) = mpsc::channel();
        let transport = Arc::new(Mutex::new(Transport::new(policy, events)));
        let weak = Arc::downgrade(&transport);
        std::thread::Builder::new()
            .name("squark-asset-decode".into())
            .spawn(move || decode_responses(weak, incoming, send, demand, decode))
            .ok()?;
        Some(Self {
            transport,
            results,
            urls: HashMap::new(),
            next: 1,
        })
    }
}

struct Body {
    mime: String,
    bytes: Vec<u8>,
}

fn decode_responses<T>(
    transport: Weak<Mutex<Transport>>,
    incoming: mpsc::Receiver<(u32, Event)>,
    results: mpsc::Sender<(u32, Option<T>)>,
    demand: FrameDemand,
    decode: fn(Resource<'_>) -> Option<T>,
) {
    let mut bodies = HashMap::<u32, Body>::new();
    for (id, event) in incoming {
        let result = match event {
            Event::Headers(response) if (200..300).contains(&response.status) => {
                let mime = response
                    .headers
                    .into_iter()
                    .find_map(|(name, value)| (name == "content-type").then_some(value))
                    .unwrap_or_default();
                bodies.insert(
                    id,
                    Body {
                        mime,
                        bytes: Vec::new(),
                    },
                );
                None
            }
            Event::Chunk(bytes) => {
                let Some(body) = bodies.get_mut(&id) else {
                    continue;
                };
                if bytes.len() > MAX_ENCODED_BYTES - body.bytes.len() {
                    Some(None)
                } else {
                    body.bytes.extend(bytes);
                    None
                }
            }
            Event::End => Some(bodies.remove(&id).and_then(|body| {
                decode(Resource {
                    mime: Cow::Owned(body.mime),
                    bytes: Cow::Owned(body.bytes),
                })
            })),
            Event::Headers(_) | Event::Error => Some(None),
            _ => continue,
        };
        let Some(transport) = transport.upgrade() else {
            break;
        };
        let mut transport = transport.lock().expect("asset transport");
        if let Some(image) = result {
            transport.cancel(id);
            drop(transport);
            bodies.remove(&id);
            if results.send((id, image)).is_err() {
                break;
            }
            demand.request_frame();
        } else {
            transport.credit(id, CREDIT);
        }
    }
}
