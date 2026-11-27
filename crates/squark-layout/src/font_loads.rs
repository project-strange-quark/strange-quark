// SPDX-License-Identifier: MIT
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{Arc, mpsc},
};

use squark_assets::{Pack, RemoteResources, Resource, Resources};
use squark_mirror::Document;
use squark_style::Cascade;
use squark_text::{DocumentFont, FaceAttributes, FontRequest, decode_font, matching_faces};
use squark_types::{
    engine::{DecodedBatch, EngineError, FaceId},
    journal::{FontFaceAction, Record},
    render::FrameDemand,
};
use style::{
    font_face::{DescriptorId, Source},
    properties::ComputedValues,
    values::computed::font::SingleFontFamily,
};
use url::Url;

type ResultBytes = Option<Arc<[u8]>>;
type Token = (u32, u64);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Sources {
    Binary(Arc<[u8]>),
    Urls(Vec<Url>),
}

#[derive(Clone, Debug)]
pub(crate) struct Definition {
    family: String,
    attributes: FaceAttributes,
    sources: Sources,
}

pub(crate) enum FontMutation {
    Define(u32, Option<Definition>),
    Membership(u32, bool),
}

pub use squark_types::engine::FontCompletion;

enum Status {
    Unloaded,
    Loading(VecDeque<Url>),
    Loaded(Arc<[u8]>),
    Failed,
}

struct Face {
    definition: Option<Definition>,
    member: bool,
    token: Token,
    status: Status,
}

pub(crate) struct FontLoads {
    faces: BTreeMap<u32, Face>,
    remote: RemoteResources<Arc<[u8]>>,
    pending: HashMap<Url, Vec<Token>>,
    send: mpsc::Sender<(Token, ResultBytes)>,
    incoming: mpsc::Receiver<(Token, ResultBytes)>,
    demand: FrameDemand,
    starts: Vec<FaceId>,
    events: Vec<FontCompletion>,
}

impl FontMutation {
    pub fn parse(
        record: &Record,
        batch: &DecodedBatch<'_>,
        document: &Document,
        cascade: &Cascade<'_>,
        buffers: &mut BTreeSet<u32>,
    ) -> Result<Self, &'static str> {
        if record.a == 0 {
            return Err("zero font face id");
        }
        match FontFaceAction::from_raw(record.flags).ok_or("invalid FONT_FACE action")? {
            FontFaceAction::Add => Ok(Self::Membership(record.a, true)),
            FontFaceAction::Remove => Ok(Self::Membership(record.a, false)),
            FontFaceAction::Define => {
                let string = |index: u32| {
                    batch
                        .strings
                        .get(index as usize)
                        .map(|text| String::from_utf16_lossy(text))
                };
                let family = string(record.b).ok_or("missing font family")?;
                let json = string(record.c).ok_or("missing font descriptor")?;
                let json: serde_json::Value =
                    serde_json::from_str(&json).map_err(|_| "invalid font descriptor JSON")?;
                let source = json.get("source").ok_or("missing font source")?;
                let document_url = cascade.resources().document_url;
                let (base, bytes, css) = if let Some(id) = source.get("bufferId") {
                    let id = id
                        .as_u64()
                        .and_then(|id| u32::try_from(id).ok())
                        .ok_or("invalid font buffer id")?;
                    if !buffers.insert(id) {
                        return Err("duplicate staged font buffer");
                    }
                    let bytes = batch
                        .staged_bytes
                        .iter()
                        .find(|(key, _)| *key == id)
                        .map(|(_, bytes)| Arc::from(*bytes))
                        .ok_or("missing font buffer")?;
                    (document_url.clone(), Some(bytes), None)
                } else {
                    let css = source
                        .get("css")
                        .and_then(|value| value.as_str())
                        .ok_or("missing CSS font source")?;
                    let base = source
                        .get("baseUrl")
                        .and_then(|value| value.as_str())
                        .and_then(|value| document_url.join(value).ok())
                        .ok_or("invalid font base URL")?;
                    (base, None, Some(css))
                };
                let mut values = [
                    (DescriptorId::FontWeight, "font-weight"),
                    (DescriptorId::FontStyle, "font-style"),
                    (DescriptorId::FontWidth, "font-stretch"),
                    (DescriptorId::UnicodeRange, "unicode-range"),
                ]
                .into_iter()
                .filter_map(|(id, key)| {
                    json.get(key)
                        .and_then(|value| value.as_str())
                        .map(|value| (id, value))
                })
                .collect::<Vec<_>>();
                values.extend(css.map(|source| (DescriptorId::Src, source)));
                let definition =
                    cascade
                        .face_descriptors(document, &base, &values)
                        .and_then(|descriptors| {
                            let attributes = FaceAttributes::from_descriptors(&descriptors)?;
                            let sources = match bytes {
                                Some(bytes) => Sources::Binary(bytes),
                                None => Sources::Urls(
                                    descriptors
                                        .src?
                                        .0
                                        .into_iter()
                                        .filter_map(|source| match source {
                                            Source::Url(source) => {
                                                source.url.url().map(|url| url.as_ref().clone())
                                            }
                                            Source::Local(_) => None,
                                        })
                                        .collect(),
                                ),
                            };
                            (!family.trim().is_empty()).then_some(Definition {
                                family,
                                attributes,
                                sources,
                            })
                        });
                Ok(Self::Define(record.a, definition))
            }
        }
    }
}

impl FontLoads {
    pub fn new(pack: &Pack, document: &Url) -> Self {
        let (send, incoming) = mpsc::channel();
        Self {
            faces: BTreeMap::new(),
            remote: RemoteResources::from_pack(
                pack,
                document,
                "font/*,*/*;q=0.8",
                |resource: Resource<'_>| decode_font(&resource.bytes),
            ),
            pending: HashMap::new(),
            send,
            incoming,
            demand: FrameDemand::default(),
            starts: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn set_frame_demand(&mut self, demand: FrameDemand) {
        self.remote.set_frame_demand(demand.clone());
        self.demand = demand;
    }

    pub fn apply(&mut self, mutation: FontMutation) {
        match mutation {
            FontMutation::Define(id, definition) => {
                if let Some(face) = self.faces.get_mut(&id) {
                    if face.definition.as_ref().map(|def| &def.sources)
                        != definition.as_ref().map(|def| &def.sources)
                    {
                        face.token.1 += 1;
                        face.status = Status::Unloaded;
                    }
                    face.definition = definition;
                } else {
                    self.faces.insert(
                        id,
                        Face {
                            definition,
                            member: false,
                            token: (id, 1),
                            status: Status::Unloaded,
                        },
                    );
                }
            }
            FontMutation::Membership(id, member) => {
                if let Some(face) = self.faces.get_mut(&id) {
                    face.member = member;
                }
            }
        }
    }

    /// The member faces CSS font matching picks for the font's families
    /// whose `unicode-range` covers some of the text, for
    /// `FontFaceSet.load()` and `check()`.
    pub fn matches(&self, font: &ComputedValues, text: &[u16]) -> Vec<FaceId> {
        let font = font.get_font();
        let request = FontRequest::from_font(font);
        let mut selected = BTreeSet::new();
        for family in font.font_family.families.list.iter() {
            let SingleFontFamily::FamilyName(family) = family else {
                continue;
            };
            let faces = self
                .faces
                .values()
                .filter_map(|face| {
                    let def = face.definition.as_ref()?;
                    (face.member && def.family.eq_ignore_ascii_case(family.name.as_str()))
                        .then_some((face.token.0, &def.attributes))
                })
                .collect::<Vec<_>>();
            selected.extend(matching_faces(&faces, request).into_iter().filter(|id| {
                self.faces[id]
                    .definition
                    .as_ref()
                    .is_some_and(|def| def.attributes.covers_any(text))
            }));
        }
        selected.into_iter().map(FaceId).collect()
    }

    pub fn start_used(
        &mut self,
        document: &Document,
        resources: Resources<'_>,
    ) -> Result<(), EngineError> {
        let unloaded = self
            .faces
            .values()
            .filter(|face| face.member && matches!(face.status, Status::Unloaded))
            .filter_map(|face| Some(&face.definition.as_ref()?.family))
            .collect::<BTreeSet<_>>();
        if unloaded.is_empty() {
            return Ok(());
        }

        let mut requested = BTreeSet::new();
        let mut pending = vec![document.root()];
        while let Some(owner) = pending.pop() {
            let css = document.computed_style(owner);
            if css.as_ref().is_some_and(|css| css.get_display().is_none()) {
                continue;
            }
            for &child in document.flat_children(owner).iter() {
                let Some(text) = document.text(child) else {
                    pending.push(child);
                    continue;
                };
                if let Some(css) = &css
                    && css.get_font().font_family.families.list.iter().any(|family| {
                        matches!(family, SingleFontFamily::FamilyName(family)
                            if unloaded.iter().any(|name| name.eq_ignore_ascii_case(family.name.as_str())))
                    })
                {
                    requested.extend(self.matches(css, text).into_iter().map(|face| face.0));
                }
            }
        }
        for face in requested {
            if matches!(self.faces[&face].status, Status::Unloaded) {
                let face = FaceId(face);
                self.starts.push(face);
                self.start(face, resources)?;
            }
        }
        Ok(())
    }

    pub fn start_matching(
        &mut self,
        font: &ComputedValues,
        text: &[u16],
        resources: Resources<'_>,
    ) -> Result<(), EngineError> {
        let has_unloaded = font.get_font().font_family.families.list.iter().any(|family| {
            matches!(family, SingleFontFamily::FamilyName(family)
                if self.faces.values().any(|face| {
                    face.member
                        && matches!(face.status, Status::Unloaded)
                        && face
                            .definition
                            .as_ref()
                            .is_some_and(|def| def.family.eq_ignore_ascii_case(family.name.as_str()))
                }))
        });
        if !has_unloaded {
            return Ok(());
        }
        for face in self.matches(font, text) {
            if matches!(self.faces[&face.0].status, Status::Unloaded) {
                self.starts.push(face);
                self.start(face, resources)?;
            }
        }
        Ok(())
    }

    pub fn active_fonts(&self) -> Vec<DocumentFont> {
        self.faces
            .values()
            .filter_map(|face| {
                let definition = face.definition.as_ref()?;
                let Status::Loaded(ref bytes) = face.status else {
                    return None;
                };
                face.member.then(|| DocumentFont {
                    id: face.token.0,
                    family: definition.family.clone(),
                    attributes: definition.attributes.clone(),
                    bytes: bytes.clone(),
                })
            })
            .collect()
    }

    pub fn start(&mut self, id: FaceId, resources: Resources<'_>) -> Result<(), EngineError> {
        let face = self
            .faces
            .get_mut(&id.0)
            .ok_or_else(|| EngineError::Resource("unknown FontFace".into()))?;
        if !matches!(face.status, Status::Unloaded) {
            return Ok(());
        }
        let token = face.token;
        match face.definition.as_ref().map(|def| &def.sources) {
            Some(Sources::Urls(urls)) => {
                face.status = Status::Loading(urls.iter().cloned().collect());
                self.next_source(token, resources);
            }
            Some(Sources::Binary(bytes)) => {
                let bytes = bytes.clone();
                face.status = Status::Loading(VecDeque::new());
                self.decode(token, bytes);
            }
            None => {
                face.status = Status::Loading(VecDeque::new());
                self.next_source(token, resources);
            }
        }
        Ok(())
    }

    fn decode(&self, token: Token, bytes: Arc<[u8]>) {
        let send = self.send.clone();
        let demand = self.demand.clone();
        rayon::spawn(move || {
            if send.send((token, decode_font(&bytes))).is_ok() {
                demand.request_frame();
            }
        });
    }

    fn next_source(&mut self, token: Token, resources: Resources<'_>) {
        let Some(face) = self
            .faces
            .get_mut(&token.0)
            .filter(|face| face.token == token)
        else {
            return;
        };
        let Status::Loading(urls) = &mut face.status else {
            return;
        };
        while let Some(url) = urls.pop_front() {
            if let Some(resource) = resources.get(&url) {
                self.decode(token, Arc::from(resource.bytes.as_ref()));
                return;
            }
            if let Some(waiters) = self.pending.get_mut(&url) {
                waiters.push(token);
                return;
            }
            if self.remote.start(url.clone()) {
                self.pending.insert(url, vec![token]);
                return;
            }
        }
        face.status = Status::Failed;
        self.events.push(FontCompletion {
            face: FaceId(token.0),
            result: Err("no usable font source".into()),
        });
        self.demand.request_frame();
    }

    pub fn collect(&mut self, resources: Resources<'_>) -> bool {
        let mut results: Vec<_> = self.incoming.try_iter().collect();
        for (url, result) in self.remote.take_finished() {
            if let Some(waiters) = self.pending.remove(&url) {
                results.extend(waiters.into_iter().map(|token| (token, result.clone())));
            }
        }
        let mut changed = false;
        for (token, result) in results {
            let Some(face) = self
                .faces
                .get_mut(&token.0)
                .filter(|face| face.token == token)
            else {
                continue;
            };
            if let Some(bytes) = result {
                face.status = Status::Loaded(bytes);
                changed |= face.member;
                self.events.push(FontCompletion {
                    face: FaceId(token.0),
                    result: Ok(()),
                });
            } else {
                self.next_source(token, resources);
            }
        }
        changed
    }

    pub fn drain(&mut self) -> Vec<FontCompletion> {
        std::mem::take(&mut self.events)
    }

    pub fn drain_starts(&mut self) -> Vec<FaceId> {
        std::mem::take(&mut self.starts)
    }
}
