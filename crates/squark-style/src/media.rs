// SPDX-License-Identifier: MIT

use cssparser::Parser;
use squark_types::engine::{Contrast, MediaState};
use style::{media_queries::MediaList, parser::ParserContext, queries::QueryCondition};

/// Features not represented by the pinned Servo Device's MediaData. Resolve
/// their parsed leaves before Stylo evaluates the rest of the media AST.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Preferences {
    reduced_motion: bool,
    contrast: Contrast,
}

impl From<&MediaState> for Preferences {
    fn from(state: &MediaState) -> Self {
        Self {
            reduced_motion: state.reduced_motion,
            contrast: state.contrast,
        }
    }
}

impl Preferences {
    pub fn resolve(self, media: &mut MediaList, context: &mut ParserContext<'_>) {
        for query in &mut media.media_queries {
            if let Some(condition) = &mut query.condition {
                self.resolve_condition(condition, context);
            }
        }
    }

    fn resolve_condition(self, condition: &mut QueryCondition, context: &mut ParserContext<'_>) {
        match condition {
            QueryCondition::InParens(inner) | QueryCondition::Not(inner) => {
                self.resolve_condition(inner, context)
            }
            QueryCondition::Operation(parts, _) => {
                for part in parts {
                    self.resolve_condition(part, context);
                }
            }
            QueryCondition::GeneralEnclosed(css, ..) if let Some(value) = self.feature(css) => {
                let query = if value {
                    "(min-width: 0px)"
                } else {
                    "(width < 0px)"
                };
                if let Some(replacement) = MediaList::parse(context, &mut Parser::new(query))
                    .media_queries
                    .pop()
                    .and_then(|query| query.condition)
                {
                    *condition = replacement;
                }
            }
            _ => {}
        }
    }

    fn feature(self, css: &str) -> Option<bool> {
        let mut parser = Parser::new(css);
        if !matches!(parser.next(), Ok(cssparser::Token::ParenthesisBlock)) {
            return None;
        }
        let matched = parser
            .parse_nested_block(|input| {
                let name = input.expect_ident()?.to_ascii_lowercase();
                let state = if input.is_exhausted() {
                    None
                } else {
                    input.expect_colon()?;
                    Some(input.expect_ident()?.to_ascii_lowercase())
                };
                let matched = match (name.as_str(), state.as_deref()) {
                    ("prefers-reduced-motion", None | Some("reduce")) => Some(self.reduced_motion),
                    ("prefers-reduced-motion", Some("no-preference")) => Some(!self.reduced_motion),
                    ("prefers-contrast", None) => Some(self.contrast != Contrast::NoPreference),
                    ("prefers-contrast", Some("no-preference")) => {
                        Some(self.contrast == Contrast::NoPreference)
                    }
                    ("prefers-contrast", Some("more")) => Some(self.contrast == Contrast::More),
                    ("prefers-contrast", Some("less")) => Some(self.contrast == Contrast::Less),
                    ("prefers-contrast", Some("custom")) => Some(self.contrast == Contrast::Custom),
                    _ => None,
                };
                Ok::<_, cssparser::ParseError<()>>(matched)
            })
            .ok()?;
        parser.expect_exhausted().ok()?;
        matched
    }
}
