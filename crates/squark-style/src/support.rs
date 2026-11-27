// SPDX-License-Identifier: MIT
//! Source-located declarations whose effect the current consumers skip:
//! ones Stylo drops because a preference keeps the property off, ones layout
//! or paint ignore, and rules Stylo drops for a selector it cannot parse.

use cssparser::{
    AtRuleParser, CowRcStr, DeclarationParser, ParseError, Parser, ParserState,
    QualifiedRuleParser, RuleBodyItemParser, RuleBodyParser, StyleSheetParser,
};
use squark_types::NodeId;
use url::Url;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Consumer {
    /// Stylo drops the declaration or rule while parsing, so it never
    /// applies and `CSS.supports` reports it unsupported.
    Style,
    Layout,
    Paint,
}

impl std::fmt::Display for Consumer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Style => f.write_str("style"),
            Self::Layout => f.write_str("layout"),
            Self::Paint => f.write_str("paint"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CssWarning {
    pub stylesheet: Url,
    /// Inline sheet owner. Its line and column are relative to the CSS text.
    pub inline_owner: Option<NodeId>,
    pub style_attribute: bool,
    pub line: u32,
    pub column: u32,
    /// The declaration's property, or the selector of a dropped rule.
    pub property: String,
    /// The declaration's value, or empty for a dropped rule.
    pub value: String,
    pub consumer: Consumer,
}

impl std::fmt::Display for CssWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.stylesheet)?;
        if let Some(owner) = self.inline_owner {
            let source = if self.style_attribute {
                "style attribute on"
            } else {
                "<style>"
            };
            write!(f, " {source} node {}", owner.raw())?;
        }
        write!(f, ":{}:{}: {} ", self.line, self.column, self.consumer)?;
        if self.value.is_empty() {
            write!(f, "drops the rule `{} {{ … }}`", self.property)
        } else {
            write!(f, "ignores `{}: {}`", self.property, self.value)
        }
    }
}

pub(crate) fn stylesheet(css: &str, url: &Url, inline_owner: Option<NodeId>) -> Vec<CssWarning> {
    let mut input = Parser::new(css);
    let mut parser = Scan {
        url,
        inline_owner,
        style_attribute: false,
        warnings: Vec::new(),
    };
    for _ in StyleSheetParser::new(&mut input, &mut parser) {}
    parser.warnings
}

pub(crate) fn attribute(css: &str, url: &Url, owner: NodeId) -> Vec<CssWarning> {
    let mut input = Parser::new(css);
    let mut parser = Scan {
        url,
        inline_owner: Some(owner),
        style_attribute: true,
        warnings: Vec::new(),
    };
    parser.body(&mut input);
    parser.warnings
}

struct Scan<'a> {
    url: &'a Url,
    inline_owner: Option<NodeId>,
    style_attribute: bool,
    warnings: Vec<CssWarning>,
}

impl Scan<'_> {
    fn body(&mut self, input: &mut Parser<'_>) {
        for _ in RuleBodyParser::new(input, self) {}
    }
}

impl<'i> DeclarationParser<'i> for Scan<'_> {
    type Declaration = ();
    type Error = ();

    fn parse_value(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
        start: &ParserState,
    ) -> Result<(), ParseError<()>> {
        let value_start = input.position();
        while input.next_including_whitespace_and_comments().is_ok() {}
        let value = input.slice_from(value_start).trim();
        let property = name.to_ascii_lowercase();
        if let Some(consumer) = ignored(&property, value) {
            let location = start.source_location();
            self.warnings.push(CssWarning {
                stylesheet: self.url.clone(),
                inline_owner: self.inline_owner,
                style_attribute: self.style_attribute,
                line: location.line + 1,
                column: location.column,
                property,
                value: value.to_owned(),
                consumer,
            });
        }
        Ok(())
    }
}

impl<'i> QualifiedRuleParser<'i> for Scan<'_> {
    type Prelude = ();
    type QualifiedRule = ();
    type Error = ();

    fn parse_prelude(&mut self, input: &mut Parser<'i>) -> Result<(), ParseError<()>> {
        let location = input.current_source_location();
        let start = input.position();
        while input.next().is_ok() {}
        let selector = input.slice_from(start).trim();
        // Stylo has no ::-webkit-scrollbar pseudo-elements, and one selector
        // it cannot parse drops the whole rule, other selectors included.
        if selector
            .to_ascii_lowercase()
            .contains("::-webkit-scrollbar")
        {
            self.warnings.push(CssWarning {
                stylesheet: self.url.clone(),
                inline_owner: self.inline_owner,
                style_attribute: self.style_attribute,
                line: location.line + 1,
                column: location.column,
                property: selector.to_owned(),
                value: String::new(),
                consumer: Consumer::Style,
            });
        }
        Ok(())
    }

    fn parse_block(
        &mut self,
        _: (),
        _: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        self.body(input);
        Ok(())
    }
}

impl<'i> AtRuleParser<'i> for Scan<'_> {
    type Prelude = bool;
    type AtRule = ();
    type Error = ();

    fn parse_prelude(
        &mut self,
        name: CowRcStr<'i>,
        input: &mut Parser<'i>,
    ) -> Result<bool, ParseError<()>> {
        while input.next().is_ok() {}
        Ok(matches!(
            name.to_ascii_lowercase().as_str(),
            "media" | "layer" | "supports" | "scope" | "container" | "starting-style" | "keyframes"
        ))
    }

    fn parse_block(
        &mut self,
        rules: bool,
        _: &ParserState,
        input: &mut Parser<'i>,
    ) -> Result<(), ParseError<()>> {
        if rules {
            self.body(input);
        }
        Ok(())
    }
}

impl RuleBodyItemParser<'_, (), ()> for Scan<'_> {
    fn parse_declarations(&self) -> bool {
        true
    }
    fn parse_qualified(&self) -> bool {
        true
    }
}

fn ignored(property: &str, value: &str) -> Option<Consumer> {
    use Consumer::{Layout, Paint, Style};
    let value = value.to_ascii_lowercase();
    let value = value
        .split_once('!')
        .map_or(value.as_str(), |(value, _)| value)
        .trim();
    if matches!(
        value,
        "" | "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    ) {
        return None;
    }
    if property.starts_with("--") || matches_engine(property, value) {
        return None;
    }
    if dropped_by_style(property) {
        return Some(Style);
    }
    Some(match (property, value) {
        (
            "float"
            | "clear"
            | "shape-outside"
            | "shape-margin"
            | "shape-image-threshold"
            | "container"
            | "container-type"
            | "container-name"
            | "anchor-name"
            | "position-anchor"
            | "position-area"
            | "column-width"
            | "column-count"
            | "columns"
            | "column-fill"
            | "column-span"
            | "hyphens"
            | "-webkit-line-clamp"
            | "line-clamp",
            _,
        ) => Layout,
        ("tab-size" | "-moz-tab-size", _) => Layout,
        ("writing-mode", _) => Layout,
        ("display", value) if value.contains("table") || value.contains("ruby") => Layout,
        ("grid-template" | "grid-template-columns" | "grid-template-rows", value)
            if value.contains("subgrid") =>
        {
            Layout
        }
        ("text-wrap" | "text-wrap-style", "balance" | "pretty" | "stable") => Layout,
        ("background-attachment", value) if value != "scroll" => Paint,
        ("background-clip", "text") => Paint,
        ("background-blend-mode", value) if value != "normal" => Paint,
        (
            "mask"
            | "mask-image"
            | "mask-position"
            | "mask-repeat"
            | "mask-size"
            | "border-image"
            | "border-image-source"
            | "border-image-slice"
            | "border-image-width"
            | "backface-visibility"
            | "transform-box"
            | "clip",
            _,
        ) => Paint,
        ("filter" | "backdrop-filter", value) if value.contains("url(") => Paint,
        ("clip-path", value)
            if ["url(", "path(", "shape(", "margin-box"]
                .iter()
                .any(|token| value.contains(token)) =>
        {
            Paint
        }
        _ => return None,
    })
}

/// Values that ask for what the engine does anyway, so leaving them out
/// changes nothing: the keyword that turns an unsupported feature off, or
/// its initial value.
fn matches_engine(property: &str, value: &str) -> bool {
    matches!(
        (property, value),
        ("hyphens", "none" | "manual")
            | ("column-width" | "column-count" | "columns", "auto")
            | ("clip", "auto")
            | ("transform-box", "border-box")
            | ("backface-visibility", "visible")
            | ("writing-mode", "horizontal-tb")
            | ("text-orientation", "mixed")
            | ("zoom", "1" | "100%")
            | ("tab-size" | "-moz-tab-size", "8")
            | (
                "float"
                    | "clear"
                    | "shape-outside"
                    | "container-type"
                    | "container-name"
                    | "container",
                "none" | "normal",
            )
            | (
                "mask" | "mask-image" | "border-image" | "border-image-source",
                "none"
            )
    ) || dropped_by_style(property) && matches!(value, "none" | "auto" | "normal")
}

/// Whether Stylo knows the property but drops it because its preference is
/// off. Properties the Servo build lacks altogether are dropped too, but
/// as silently as browsers drop unknown properties.
fn dropped_by_style(property: &str) -> bool {
    style::properties::PropertyId::parse_unchecked(property, None)
        .is_ok_and(|id| !id.enabled_for_all_content())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_declaration_positions_inside_rules_and_skips_supported_values() {
        crate::enable_preferences();
        let url = Url::parse("https://example.test/css/limited.css").unwrap();
        let css = "\
@font-face { font-family: Demo; src: url(font.woff2); }
@media screen {
  .fixture {
    float: left !important;
    background: linear-gradient(red, blue);
    background-clip: text;
    --test: 'column-count: 2';
    & > b { writing-mode: vertical-rl; }
  }
}
p { clear: none; writing-mode: horizontal-tb; }
.code { tab-size: 4; text-shadow: 0 1px red; contain: strict; scroll-behavior: auto; }
.list { scroll-behavior: smooth; scrollbar-width: thin; scrollbar-color: red blue;
  mask-image: url(fade.svg); overscroll-behavior: contain; }
.list, .list::-webkit-scrollbar-thumb { background: gray; }";
        let warnings = stylesheet(css, &url, None);
        let actual: Vec<_> = warnings
            .iter()
            .map(|warning| {
                assert_eq!(warning.stylesheet, url);
                (
                    warning.property.as_str(),
                    warning.value.as_str(),
                    warning.line,
                    warning.column,
                    warning.consumer,
                )
            })
            .collect();
        // Stylo drops the properties whose preference is off, and the whole
        // rule for a selector it cannot parse. Text shadows paint, and
        // containment lays out.
        assert_eq!(
            actual,
            [
                ("float", "left !important", 4, 5, Consumer::Layout),
                ("background-clip", "text", 6, 5, Consumer::Paint),
                ("writing-mode", "vertical-rl", 8, 13, Consumer::Style),
                ("tab-size", "4", 12, 9, Consumer::Layout),
                ("scroll-behavior", "smooth", 13, 9, Consumer::Style),
                ("scrollbar-width", "thin", 13, 34, Consumer::Style),
                ("scrollbar-color", "red blue", 13, 57, Consumer::Style),
                ("mask-image", "url(fade.svg)", 14, 3, Consumer::Style),
                (
                    ".list, .list::-webkit-scrollbar-thumb",
                    "",
                    15,
                    1,
                    Consumer::Style
                ),
            ]
        );
        assert_eq!(
            warnings.last().unwrap().to_string(),
            "https://example.test/css/limited.css:15:1: style drops the rule \
             `.list, .list::-webkit-scrollbar-thumb { … }`"
        );
    }
}
