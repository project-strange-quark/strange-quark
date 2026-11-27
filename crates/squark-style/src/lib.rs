// SPDX-License-Identifier: MIT
//! Static UA and author cascade. Stylesheet metadata preserves tree order.

mod media;
mod support;
pub use support::{Consumer, CssWarning};

use std::collections::{BTreeMap, HashSet};

use squark_types::{NodeId, engine::MediaState};

use cssparser::Parser;
use squark_assets::{Pack, ScriptKind};
use squark_journal::BatchDecoder;
use squark_mirror::Document;
use style::context::QuirksMode;
use style::device::Device;
use style::device::servo::{FontMetricsProvider, MediaData};
use style::font_metrics::FontMetrics;
use style::media_queries::{MediaList, MediaType};
use style::parser::ParserContext;
use style::properties::{ComputedValues, style_structs::Font};
use style::queries::values::PrefersColorScheme;
use style::servo_arc::Arc;
use style::shared_lock::{StylesheetGuards, ToCssWithGuard};
use style::stylesheets::{
    AllRules, AllowImportRules, CssRule, CssRuleType, CustomMediaEvaluator, CustomMediaMap,
    DocumentStyleSheet, InsertRuleContext, Origin, Stylesheet,
};
use style::stylist::Stylist;
use style::values::computed::font::{GenericFontFamily, QueryFontMetricsFlags};
use style::values::computed::{CSSPixelLength, Length};
use style_traits::{ParsingMode, ToCss};
use url::Url;

#[derive(Debug)]
pub enum LoadError {
    Pack(squark_assets::PackError),
    Replay(squark_mirror::ReplayError),
    Journal(squark_journal::JournalError),
}

impl From<squark_assets::PackError> for LoadError {
    fn from(error: squark_assets::PackError) -> Self {
        Self::Pack(error)
    }
}

impl From<squark_mirror::ReplayError> for LoadError {
    fn from(error: squark_mirror::ReplayError) -> Self {
        Self::Replay(error)
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pack(error) => error.fmt(f),
            Self::Replay(error) => error.fmt(f),
            Self::Journal(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LoadError {}

/// Each update cascades against the mirror's existing style data unless the
/// device or stylesheet set changed.
pub struct Cascade<'a> {
    pack: &'a Pack,
    blob_resolver: Option<std::sync::Arc<squark_assets::BlobResolver>>,
    document_url: Url,
    media: MediaData,
    stylist: Stylist,
    author_sheets: Vec<DocumentStyleSheet>,
    sources: Vec<Source>,
    inline_styles: Vec<(NodeId, String)>,
    warnings: Vec<CssWarning>,
    reported: HashSet<WarningLocation>,
    device_changed: bool,
    preferences: media::Preferences,
    preferences_changed: bool,
    font_metrics: Option<MetricsProvider>,
}

/// The provider Stylo asks for the metrics of the primary font, which
/// font-relative units (`ex`, `ch`, `cap`, `ic`) resolve against.
pub type MetricsProvider = std::sync::Arc<dyn FontMetricsProvider + Send>;

impl std::fmt::Debug for Cascade<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cascade")
            .field("document_url", &self.document_url)
            .field("media", &self.media)
            .field("author_sheet_count", &self.author_sheets.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct WarningLocation {
    stylesheet: Url,
    inline_owner: Option<NodeId>,
    style_attribute: bool,
    line: u32,
    column: u32,
    property: String,
}

#[derive(Debug, PartialEq)]
struct Source {
    css: String,
    url: Url,
    media: String,
    inline_owner: Option<NodeId>,
    owner: Option<NodeId>,
}

type AuthorSources = (Vec<Source>, Vec<(NodeId, String)>);

impl<'a> Cascade<'a> {
    pub fn keyframes(
        &self,
        document: &Document,
        owner: NodeId,
        name: &style::Atom,
    ) -> Option<style::stylesheets::keyframes_rule::KeyframesAnimation> {
        document.keyframes(&self.stylist, owner, name)
    }

    /// Replay the startup journal without starting a JS runtime.
    pub fn from_pack(
        pack: &'a Pack,
        document_url: Url,
        media: MediaData,
    ) -> Result<(Document, BatchDecoder, Self), LoadError> {
        enable_preferences();
        let initial = pack.document()?;
        let quirks = match initial.quirks {
            squark_assets::QuirksMode::Standards => QuirksMode::NoQuirks,
            squark_assets::QuirksMode::LimitedQuirks => QuirksMode::LimitedQuirks,
            squark_assets::QuirksMode::Quirks => QuirksMode::Quirks,
        };
        let mut document = Document::new(initial.id, quirks)?;
        let mut decoder = BatchDecoder::new(initial.id).map_err(LoadError::Journal)?;
        for bytes in initial.batches() {
            document.apply_journal(&mut decoder, bytes)?;
        }

        let mut media = media;
        media.quirks_mode = quirks;
        let mut stylist = Stylist::new(device(media.clone(), None), quirks);
        let ua = Source {
            css: include_str!("ua.css").into(),
            media: String::new(),
            inline_owner: None,
            owner: None,
            url: document_url.clone(),
        };
        stylist.append_stylesheet(
            parse_sheet(&ua, &document, Origin::UserAgent),
            &document.style_lock().read(),
        );
        Ok((
            document,
            decoder,
            Self {
                pack,
                blob_resolver: None,
                document_url,
                media,
                stylist,
                author_sheets: Vec::new(),
                sources: Vec::new(),
                inline_styles: Vec::new(),
                warnings: Vec::new(),
                reported: HashSet::new(),
                device_changed: false,
                preferences: Default::default(),
                preferences_changed: false,
                font_metrics: None,
            },
        ))
    }

    pub fn update(&mut self, document: &mut Document, media: MediaData) -> Result<(), LoadError> {
        self.set_media(document, media);
        let (sources, inline_styles) = self.sources(document)?;
        let inline_changed = inline_styles != self.inline_styles;
        let sheets_changed = self.sources != sources;
        if self.device_changed || sheets_changed || self.preferences_changed {
            if sheets_changed || self.preferences_changed {
                {
                    let guard = document.style_lock().read();
                    for sheet in self.author_sheets.drain(..) {
                        self.stylist.remove_stylesheet(sheet, &guard);
                    }
                }
                for source in &sources {
                    let sheet = parse_sheet(source, document, Origin::Author);
                    self.resolve_sheet_media(document, &sheet, &source.url);
                    self.stylist
                        .append_stylesheet(sheet.clone(), &document.style_lock().read());
                    self.author_sheets.push(sheet);
                }
                self.sources = sources;
            }
            document.cascade(&mut self.stylist, &self.document_url.clone().into());
            self.device_changed = false;
            self.preferences_changed = false;
        } else {
            document.restyle(&mut self.stylist, &self.document_url.clone().into());
        }
        if sheets_changed || inline_changed {
            self.inline_styles = inline_styles;
            self.update_warnings();
        }
        Ok(())
    }

    pub fn restyle(&mut self, document: &mut Document) -> Result<(), LoadError> {
        self.update(document, self.media.clone())
    }

    pub fn sample_animations(&mut self, document: &mut Document) {
        document.restyle(&mut self.stylist, &self.document_url.clone().into());
    }

    /// Resolves font-relative units against `provider` from the next
    /// update on. Without one they use the CSS fallbacks, such as 0.5em
    /// for `ch` and `ex`.
    pub fn set_font_metrics(&mut self, document: &Document, provider: MetricsProvider) {
        self.font_metrics = Some(provider);
        let guard = document.style_lock().read();
        let guards = StylesheetGuards {
            author: &guard,
            ua_or_user: &guard,
        };
        let affected = self.stylist.set_device(
            device(self.media.clone(), self.font_metrics.clone()),
            &guards,
        );
        self.stylist.force_stylesheet_origins_dirty(affected);
        self.device_changed = true;
    }

    /// The primary fonts behind font-relative units changed, as when a web
    /// font loads: the next update computes every style again.
    pub fn invalidate_font_metrics(&mut self) {
        self.device_changed = true;
    }

    pub fn set_preferences(&mut self, state: &MediaState) -> bool {
        let preferences = media::Preferences::from(state);
        if preferences == self.preferences {
            return false;
        }
        self.preferences = preferences;
        self.preferences_changed = true;
        true
    }

    fn resolve_sheet_media(&self, document: &Document, sheet: &DocumentStyleSheet, url: &Url) {
        let lock = document.style_lock();
        let lists = {
            let guard = lock.read();
            let contents = sheet.0.contents.read_with(&guard);
            let custom = CustomMediaMap::default();
            std::iter::once(sheet.0.media.clone())
                .chain(
                    contents
                        .iter_rules::<AllRules, _>(self.stylist.device(), &custom, &guard)
                        .filter_map(|rule| match rule {
                            CssRule::Media(media) => Some(media.media_queries.clone()),
                            _ => None,
                        }),
                )
                .collect::<Vec<_>>()
        };
        let url = url.clone().into();
        let mut context = media_context(&url, document.quirks());
        let mut guard = lock.write();
        for list in lists {
            self.preferences
                .resolve(list.write_with(&mut guard), &mut context);
        }
    }

    pub fn set_media(&mut self, document: &Document, mut media: MediaData) -> bool {
        media.quirks_mode = document.quirks();
        if self.media == media {
            return false;
        }
        let guard = document.style_lock().read();
        let guards = StylesheetGuards {
            author: &guard,
            ua_or_user: &guard,
        };
        let affected = self
            .stylist
            .set_device(device(media.clone(), self.font_metrics.clone()), &guards);
        self.stylist.force_stylesheet_origins_dirty(affected);
        self.media = media;
        self.device_changed = true;
        true
    }

    pub fn evaluate_media(&self, query: &[u16]) -> (Vec<u16>, bool) {
        let url = self.document_url.clone().into();
        let mut context = media_context(&url, self.media.quirks_mode);
        let mut query = MediaList::parse(
            &mut context,
            &mut Parser::new(&String::from_utf16_lossy(query)),
        );
        let serialized = query.to_css_string().encode_utf16().collect();
        self.preferences.resolve(&mut query, &mut context);
        let matches = query.evaluate(
            self.stylist.device(),
            self.media.quirks_mode,
            &mut CustomMediaEvaluator::none(),
        );
        (serialized, matches)
    }

    pub fn parse_rule(&self, document: &Document, text: &[u16]) -> Option<Vec<u16>> {
        let source = Source {
            css: String::new(),
            url: self.document_url.clone(),
            media: String::new(),
            inline_owner: None,
            owner: None,
        };
        let sheet = parse_sheet(&source, document, Origin::Author);
        let lock = document.style_lock();
        let guard = lock.read();
        let context = InsertRuleContext {
            rule_list: &[],
            index: 0,
            containing_rule_types: Default::default(),
            parse_relative_rule_type: None,
        };
        let rule = CssRule::parse(
            &String::from_utf16_lossy(text),
            context,
            sheet.0.contents.read_with(&guard),
            lock,
            None,
            AllowImportRules::No,
        )
        .ok()?;
        Some(rule.to_css_string(&guard).encode_utf16().collect())
    }

    pub fn css_supports(&self, condition: &[u16]) -> bool {
        let url = self.document_url.clone().into();
        let mut context = ParserContext::new(
            Origin::Author,
            &url,
            Some(CssRuleType::Style),
            ParsingMode::DEFAULT,
            self.media.quirks_mode,
            Default::default(),
            None,
            None,
            Default::default(),
        );
        Parser::new(&String::from_utf16_lossy(condition))
            .parse_entirely(style::stylesheets::supports_rule::parse_condition_or_declaration)
            .is_ok_and(|condition| condition.eval(&mut context))
    }

    pub fn has_sheet(&self, owner: NodeId) -> bool {
        self.sources
            .iter()
            .any(|source| source.owner == Some(owner))
    }

    pub fn sheet_rules(&self, document: &Document, owner: NodeId) -> Vec<String> {
        self.sources
            .iter()
            .find(|source| source.owner == Some(owner))
            .map(|source| {
                let sheet = parse_sheet(source, document, Origin::Author);
                let guard = document.style_lock().read();
                sheet
                    .0
                    .contents
                    .read_with(&guard)
                    .rules(&guard)
                    .iter()
                    .map(|rule| rule.to_css_string(&guard))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn update_warnings(&mut self) {
        self.warnings = self
            .sources
            .iter()
            .flat_map(|source| support::stylesheet(&source.css, &source.url, source.inline_owner))
            .chain(
                self.inline_styles
                    .iter()
                    .flat_map(|(owner, css)| support::attribute(css, &self.document_url, *owner)),
            )
            .collect();
        if cfg!(debug_assertions) {
            for warning in &self.warnings {
                if self.reported.insert(WarningLocation {
                    stylesheet: warning.stylesheet.clone(),
                    inline_owner: warning.inline_owner,
                    style_attribute: warning.style_attribute,
                    line: warning.line,
                    column: warning.column,
                    property: warning.property.clone(),
                }) {
                    eprintln!("CSS warning: {warning}");
                }
            }
        }
    }

    /// The descriptors of the @font-face rules in effect, in cascade order.
    /// `src` URLs resolve against their stylesheet.
    pub fn font_faces(&self, document: &Document) -> Vec<style::font_face::Descriptors> {
        let guard = document.style_lock().read();
        self.stylist
            .iter_extra_data_origins()
            .flat_map(|(data, _)| data.font_faces.iter())
            .map(|(rule, _)| rule.read_with(&guard).descriptors.clone())
            .collect()
    }

    /// Unsupported declarations in the current author stylesheet set.
    pub fn css_warnings(&self) -> &[CssWarning] {
        &self.warnings
    }

    pub fn computed_style(
        &self,
        document: &Document,
        node: NodeId,
        pseudo: Option<&style::selector_parser::PseudoElement>,
    ) -> Option<Arc<ComputedValues>> {
        if !std::iter::successors(Some(node), |id| document.parent(*id))
            .any(|id| id == document.root())
        {
            return None;
        }
        match pseudo {
            Some(pseudo) => document.query_pseudo_style(node, pseudo, &self.stylist),
            None => document.computed_style(node),
        }
    }

    pub fn canvas_font(
        &self,
        document: &Document,
        environment: Option<NodeId>,
        font: &str,
    ) -> Option<Arc<ComputedValues>> {
        self.canvas_declaration(document, environment, "font", font)
    }

    pub fn canvas_font_css(
        &self,
        document: &Document,
        canvas: NodeId,
        text: &[u16],
    ) -> Option<Vec<u16>> {
        use style::values::computed::font::{FontFamilyNameSyntax, SingleFontFamily};
        use style::values::computed::{FontStyle, FontWeight, FontWidth};
        let css = self.canvas_font(document, Some(canvas), &String::from_utf16_lossy(text))?;
        let font = css.get_font();
        let mut parts = Vec::new();
        if font.font_style != FontStyle::NORMAL {
            parts.push(font.font_style.to_css_string());
        }
        if font.font_variant_caps.to_css_string() == "small-caps" {
            parts.push("small-caps".into());
        }
        if font.font_weight == FontWeight::BOLD {
            parts.push("bold".into());
        } else if font.font_weight != FontWeight::NORMAL {
            parts.push(font.font_weight.to_css_string());
        }
        if font.font_width != FontWidth::NORMAL {
            parts.push(font.font_width.to_css_string());
        }
        parts.push(font.font_size.used_size.0.to_css_string());
        let families: Vec<_> = font
            .font_family
            .families
            .iter()
            .map(|family| match family {
                SingleFontFamily::FamilyName(name)
                    if name.name.contains(' ') || name.name.starts_with("--") =>
                {
                    let mut name = name.clone();
                    name.syntax = FontFamilyNameSyntax::Quoted;
                    name.to_css_string()
                }
                _ => family.to_css_string(),
            })
            .collect();
        parts.push(families.join(", "));
        Some(parts.join(" ").encode_utf16().collect())
    }

    pub fn canvas_filter(
        &self,
        document: &Document,
        environment: Option<NodeId>,
        filter: &[u16],
    ) -> Option<Arc<ComputedValues>> {
        self.canvas_declaration(
            document,
            environment,
            "filter",
            &String::from_utf16_lossy(filter),
        )
    }

    pub fn canvas_filter_uses_url(&self, document: &Document, filter: &[u16]) -> bool {
        use style::{parser::Parse, values::specified::effects::SpecifiedFilter};
        let url = self.document_url.clone().into();
        let context = ParserContext::new(
            Origin::Author,
            &url,
            Some(CssRuleType::Style),
            ParsingMode::DEFAULT,
            document.quirks(),
            Default::default(),
            None,
            None,
            Default::default(),
        );
        let filter = String::from_utf16_lossy(filter);
        let mut input = Parser::new(&filter);
        let mut has_url = false;
        while !input.is_exhausted() {
            // Stylo's Servo branch skips SVG URL filters. Recognize the
            // unsupported operation in the Canvas list and let Stylo check
            // that every other operation uses a valid filter function.
            if input.try_parse(|input| input.expect_url()).is_ok() {
                has_url = true;
            } else if SpecifiedFilter::parse(&context, &mut input).is_err() {
                return false;
            }
        }
        has_url
    }

    pub fn canvas_color(
        &self,
        document: &Document,
        canvas: NodeId,
        text: &[u16],
    ) -> Option<[u8; 4]> {
        use style::{color::AbsoluteColor, values::specified::Color};
        let url = self.document_url.clone().into();
        let context = ParserContext::new(
            Origin::Author,
            &url,
            Some(CssRuleType::Style),
            ParsingMode::DEFAULT,
            document.quirks(),
            Default::default(),
            None,
            None,
            Default::default(),
        );
        let current = document.computed_style(canvas);
        let current = current
            .as_ref()
            .map_or(&AbsoluteColor::BLACK, |css| css.get_color());
        let text = String::from_utf16_lossy(text);
        let color = Color::parse_and_compute(
            &context,
            &mut Parser::new(&text),
            Some(self.stylist.device()),
        )
        .ok()?
        .resolve_to_absolute(current);
        Some(canvas_rgba8(&color))
    }

    fn canvas_declaration(
        &self,
        document: &Document,
        environment: Option<NodeId>,
        name: &str,
        value: &str,
    ) -> Option<Arc<ComputedValues>> {
        use style::properties::{
            Importance, PropertyDeclaration, PropertyDeclarationBlock, PropertyId,
            SourcePropertyDeclaration, parse_one_declaration_into,
        };
        let mut declarations = SourcePropertyDeclaration::default();
        parse_one_declaration_into(
            &mut declarations,
            PropertyId::parse_enabled_for_all_content(name).ok()?,
            value,
            Origin::Author,
            &self.document_url.clone().into(),
            None,
            ParsingMode::DEFAULT,
            document.quirks(),
            CssRuleType::Style,
        )
        .ok()?;
        let mut block = PropertyDeclarationBlock::default();
        block.extend(declarations.drain(), Importance::Normal);
        if block.declarations().iter().any(|declaration| {
            matches!(
                declaration,
                PropertyDeclaration::CSSWideKeyword(_) | PropertyDeclaration::WithVariables(_)
            )
        }) {
            return None;
        }
        Some(document.compute_declarations(&self.stylist, environment, block))
    }

    pub fn device_pixel_ratio(&self) -> f32 {
        self.media.device_pixel_ratio.0
    }

    /// The style of an anonymous box, such as a table wrapper that layout
    /// generates around misparented table parts: inherited properties come
    /// from the parent box and the rest take their initial values.
    pub fn anonymous_style(
        &self,
        parent: &ComputedValues,
        display: style::values::computed::Display,
    ) -> Arc<ComputedValues> {
        let mut builder = style::properties::StyleBuilder::for_inheritance(
            self.stylist.device(),
            Some(&self.stylist),
            Some(parent),
            None,
        );
        builder.mutate_box().set_display(display);
        builder.build()
    }

    pub fn set_blob_resolver(&mut self, resolver: std::sync::Arc<squark_assets::BlobResolver>) {
        self.blob_resolver = Some(resolver);
    }

    pub fn face_descriptors(
        &self,
        document: &Document,
        base: &Url,
        declarations: &[(style::font_face::DescriptorId, &str)],
    ) -> Option<style::font_face::Descriptors> {
        let url = base.clone().into();
        let context = ParserContext::new(
            Origin::Author,
            &url,
            Some(CssRuleType::FontFace),
            ParsingMode::DEFAULT,
            document.quirks(),
            Default::default(),
            None,
            None,
            Default::default(),
        );
        let mut descriptors = style::font_face::Descriptors::default();
        for &(id, value) in declarations {
            descriptors
                .set(id, &context, &mut Parser::new(value))
                .ok()?;
        }
        Some(descriptors)
    }

    pub fn resources(&self) -> squark_assets::Resources<'_> {
        squark_assets::Resources {
            pack: Some(self.pack),
            blob_resolver: self.blob_resolver.as_deref(),
            document_url: &self.document_url,
        }
    }

    fn sources(&self, document: &Document) -> Result<AuthorSources, LoadError> {
        let initial = self.pack.document()?;
        let links: BTreeMap<_, _> = initial
            .scripts()
            .filter(|entry| entry.kind == ScriptKind::ExternalStylesheet)
            .map(|entry| (entry.node, entry))
            .collect();
        let mut sheets = Vec::new();
        let mut inline_styles = Vec::new();
        let mut pending = vec![document.root()];
        while let Some(id) = pending.pop() {
            let attr = |name| document.attribute(id, name).map(String::from_utf16_lossy);
            let children = document.children(id).unwrap_or_default();
            pending.extend(children.iter().rev().copied());
            if let Some(style) = attr("style") {
                inline_styles.push((id, style));
            }
            let (css, url) = match document.html_element(id) {
                Some("style") => {
                    if attr("type").is_some_and(|mime| {
                        !mime.is_empty() && !mime.eq_ignore_ascii_case("text/css")
                    }) {
                        continue;
                    }
                    let css = if let Some(rules) = document.sheet_rules(id.raw()) {
                        rules
                            .iter()
                            .map(|rule| String::from_utf16_lossy(rule))
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        let units: Vec<u16> = children
                            .iter()
                            .filter(|&&child| document.is_text(child))
                            .filter_map(|&child| document.text(child))
                            .flatten()
                            .copied()
                            .collect();
                        String::from_utf16_lossy(&units)
                    };
                    (css, self.document_url.clone())
                }
                Some("link") => {
                    let rel = attr("rel").unwrap_or_default();
                    let has_rel = |name| {
                        rel.split_ascii_whitespace()
                            .any(|part| part.eq_ignore_ascii_case(name))
                    };
                    if !has_rel("stylesheet") || has_rel("alternate") || attr("disabled").is_some()
                    {
                        continue;
                    }
                    let (asset, url) = if let Some(entry) = links.get(&id) {
                        let path = entry.payload.to_string_lossy();
                        let Some(asset) = self.pack.entry(&path) else {
                            continue;
                        };
                        let base = entry.base_url.to_string_lossy();
                        let url = self
                            .document_url
                            .join(&base)
                            .and_then(|base| base.join(&path))
                            .unwrap_or_else(|_| self.document_url.clone());
                        (asset, url)
                    } else {
                        // A link inserted after load: same-origin hrefs read the pack,
                        // as same-origin fetches do. Anything else does not load.
                        let Some((asset, url)) = attr("href")
                            .and_then(|href| self.document_url.join(&href).ok())
                            .filter(|url| url.origin() == self.document_url.origin())
                            .and_then(|url| {
                                let path = percent_decode_path(url.path())?;
                                Some((self.pack.entry(&path)?, url))
                            })
                        else {
                            continue;
                        };
                        (asset, url)
                    };
                    (
                        String::from_utf8_lossy(asset.bytes)
                            .trim_start_matches('\u{feff}')
                            .to_owned(),
                        url,
                    )
                }
                _ => continue,
            };
            sheets.push(Source {
                css,
                url,
                media: attr("media").unwrap_or_default(),
                inline_owner: (document.html_element(id) == Some("style")).then_some(id),
                owner: Some(id),
            });
        }
        Ok((sheets, inline_styles))
    }
}

/// Turns on the Stylo preferences for the features layout and paint honour.
/// Stylo drops a declaration whose property's preference is off, so
/// `CSS.supports` reports it unsupported.
pub(crate) fn enable_preferences() {
    stylo_static_prefs::set_pref!("layout.css.has-selector.enabled", true);
    stylo_static_prefs::set_pref!("layout.css.appearance.enabled", true);
    stylo_static_prefs::set_pref!("layout.css.text-overflow.enabled", true);
    // Layout sizes, lays out and clips contained boxes.
    stylo_static_prefs::set_pref!("layout.css.contain.enabled", true);
    // Paint and the GPU renderer filter what lies behind the box.
    stylo_static_prefs::set_pref!("layout.css.backdrop-filter.enabled", true);
    // Text shaping sets the variation axes and the optical size.
    stylo_static_prefs::set_pref!("layout.css.font-variations.enabled", true);
    // The compositor stops scroll chaining at a box that contains its
    // overscroll.
    stylo_static_prefs::set_pref!("layout.css.overscroll-behavior.enabled", true);
}

/// Canvas stream colours are unpremultiplied, sRGB, and stored as RGBA8.
pub fn canvas_rgba8(value: &style::color::AbsoluteColor) -> [u8; 4] {
    let color = value.to_color_space(style::color::ColorSpace::Srgb);
    [
        color.components.0,
        color.components.1,
        color.components.2,
        color.alpha,
    ]
    .map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn parse_sheet(source: &Source, document: &Document, origin: Origin) -> DocumentStyleSheet {
    let lock = document.style_lock();
    let quirks = document.quirks();
    let url = source.url.clone().into();
    let mut context = ParserContext::new(
        origin,
        &url,
        Some(CssRuleType::Media),
        ParsingMode::DEFAULT,
        quirks,
        Default::default(),
        None,
        None,
        Default::default(),
    );
    let media = MediaList::parse(&mut context, &mut Parser::new(&source.media));
    let sheet = Stylesheet::from_str(
        &source.css,
        url,
        origin,
        Arc::new(lock.wrap(media)),
        lock.clone(),
        None,
        None,
        quirks,
        AllowImportRules::No,
    );
    DocumentStyleSheet(Arc::new(sheet))
}

fn media_context(url: &style::stylesheets::UrlExtraData, quirks: QuirksMode) -> ParserContext<'_> {
    ParserContext::new(
        Origin::Author,
        url,
        Some(CssRuleType::Media),
        ParsingMode::DEFAULT,
        quirks,
        Default::default(),
        None,
        None,
        Default::default(),
    )
}

/// Screen media for a CSS viewport at a given device scale.
pub fn screen(width: f32, height: f32, scale: f32) -> MediaData {
    MediaData {
        media_type: MediaType::screen(),
        viewport_size: euclid::Size2D::new(width, height),
        device_size: euclid::Size2D::new(width * scale, height * scale),
        device_pixel_ratio: euclid::Scale::new(scale),
        quirks_mode: QuirksMode::NoQuirks,
        prefers_color_scheme: PrefersColorScheme::Light,
        primary_pointer_capabilities: Default::default(),
        all_pointer_capabilities: Default::default(),
    }
}

fn device(media: MediaData, font_metrics: Option<MetricsProvider>) -> Device {
    let provider: Box<dyn FontMetricsProvider> = match font_metrics {
        Some(provider) => Box::new(SharedFontMetrics(provider)),
        None => Box::new(DefaultFontMetrics),
    };
    Device::new(
        ComputedValues::initial_values_with_font_override(Font::initial_values()),
        provider,
        "Submit".into(),
        media,
    )
}

/// Each device the cascade creates asks the same provider.
#[derive(Debug)]
struct SharedFontMetrics(MetricsProvider);

impl FontMetricsProvider for SharedFontMetrics {
    fn query_font_metrics(
        &self,
        vertical: bool,
        font: &Font,
        base_size: CSSPixelLength,
        flags: QueryFontMetricsFlags,
    ) -> FontMetrics {
        self.0.query_font_metrics(vertical, font, base_size, flags)
    }

    fn base_size_for_generic(&self, family: GenericFontFamily) -> Length {
        self.0.base_size_for_generic(family)
    }
}

#[derive(Debug)]
struct DefaultFontMetrics;

impl FontMetricsProvider for DefaultFontMetrics {
    fn query_font_metrics(
        &self,
        _: bool,
        _: &Font,
        _: CSSPixelLength,
        _: QueryFontMetricsFlags,
    ) -> FontMetrics {
        FontMetrics::default()
    }

    fn base_size_for_generic(&self, family: GenericFontFamily) -> Length {
        Length::new(if family == GenericFontFamily::Monospace {
            13.0
        } else {
            16.0
        })
    }
}

/// A URL path as a pack path: percent escapes decoded, which must yield UTF-8.
fn percent_decode_path(path: &str) -> Option<String> {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}
