// SPDX-License-Identifier: MIT

use markup5ever::{LocalName, ns};
use squark_types::{NodeId, journal::ElementState};

use crate::{Document, Element};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputKind {
    Text,
    Search,
    Password,
    Email,
    Url,
    Tel,
    Number,
    Checkbox,
    Radio,
    Button,
    Submit,
    Reset,
    Other,
}

impl Element {
    pub(crate) fn is_text_control(&self) -> bool {
        (self.namespace == ns!(html) && &*self.name == "textarea")
            || matches!(
                self.input_kind(),
                Some(
                    InputKind::Text
                        | InputKind::Search
                        | InputKind::Password
                        | InputKind::Email
                        | InputKind::Url
                        | InputKind::Tel
                        | InputKind::Number
                )
            )
    }

    pub(crate) fn input_kind(&self) -> Option<InputKind> {
        if self.namespace != ns!(html) || &*self.name != "input" {
            return None;
        }
        use std::borrow::Cow;
        let name = self
            .attr(&ns!(), &markup5ever::local_name!("type"))
            .map(|attr| {
                if attr.scalar.bytes().any(|byte| byte.is_ascii_uppercase()) {
                    Cow::Owned(attr.scalar.to_ascii_lowercase())
                } else {
                    Cow::Borrowed(attr.scalar.as_str())
                }
            });
        Some(match name.as_deref() {
            Some("search") => InputKind::Search,
            Some("password") => InputKind::Password,
            Some("email") => InputKind::Email,
            Some("url") => InputKind::Url,
            Some("tel") => InputKind::Tel,
            Some("number") => InputKind::Number,
            Some("checkbox") => InputKind::Checkbox,
            Some("radio") => InputKind::Radio,
            Some("button") => InputKind::Button,
            Some("submit") => InputKind::Submit,
            Some("reset") => InputKind::Reset,
            Some(
                "color" | "date" | "datetime-local" | "file" | "hidden" | "image" | "month"
                | "range" | "time" | "week",
            ) => InputKind::Other,
            _ => InputKind::Text,
        })
    }

    /// A snapshot without a state write uses the packed HTML defaults.
    /// Once journaled, both true and false bits supersede those defaults.
    pub(crate) fn form_state(&self) -> u32 {
        use ElementState as S;
        let mut state = self.state;
        if self.namespace != ns!(html) {
            return state;
        }
        let name: &str = &self.name;
        let checkable = matches!(
            self.input_kind(),
            Some(InputKind::Checkbox | InputKind::Radio)
        );
        let mut default = |applicable: bool, bit: S, attr: &str| {
            if !applicable {
                state &= !bit.bits();
            } else if self.state_mask & bit.bits() == 0
                && self.attr(&ns!(), &LocalName::from(attr)).is_some()
            {
                state |= bit.bits();
            }
        };
        default(checkable, S::CHECKED, "checked");
        default(name == "option", S::SELECTED, "selected");
        default(
            matches!(
                name,
                "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
            ),
            S::DISABLED,
            "disabled",
        );
        default(
            matches!(name, "input" | "textarea"),
            S::READ_ONLY,
            "readonly",
        );
        state
    }
}

/// The journaled editing operands. The displayed value is still SET_VALUE.
#[derive(Clone, Debug, Default)]
pub struct TextEdit {
    pub start: u32,
    pub end: u32,
    pub backward: bool,
    pub preedit: Box<[u16]>,
    pub cursor: u32,
}

impl Document {
    pub fn text_edit(&self, id: NodeId) -> Option<&TextEdit> {
        self.node(id)?.element()?.text_edit.as_deref()
    }

    pub fn input_kind(&self, id: NodeId) -> Option<InputKind> {
        self.node(id)?.element()?.input_kind()
    }

    /// An explicit empty value is distinct from a control with no SET_VALUE.
    pub fn live_value(&self, id: NodeId) -> Option<&[u16]> {
        self.node(id)?.element()?.value.as_deref()
    }

    pub fn control_state(&self, id: NodeId) -> ElementState {
        let mut bits = self
            .node(id)
            .and_then(|node| node.element())
            .map_or(0, Element::form_state);
        if self.inherited_disabled(id) {
            bits |= ElementState::DISABLED.bits();
        }
        ElementState::from_bits(bits).expect("validated state")
    }

    pub(crate) fn inherited_disabled(&self, id: NodeId) -> bool {
        let disabled = |id| {
            self.node(id)
                .and_then(|node| node.element())
                .is_some_and(|element| element.form_state() & ElementState::DISABLED.bits() != 0)
        };
        match self.html_element(id) {
            Some("option") => {
                return self.parent(id).is_some_and(|parent| {
                    self.html_element(parent) == Some("optgroup") && disabled(parent)
                });
            }
            Some("button" | "fieldset" | "input" | "select" | "textarea") => {}
            _ => return false,
        }
        let mut child = id;
        while let Some(parent) = self.parent(child) {
            if self.html_element(parent) == Some("fieldset")
                && disabled(parent)
                && self.first_legend(parent) != Some(child)
            {
                return true;
            }
            child = parent;
        }
        false
    }

    pub(crate) fn first_legend(&self, fieldset: NodeId) -> Option<NodeId> {
        self.children(fieldset)?
            .iter()
            .copied()
            .find(|&child| self.html_element(child) == Some("legend"))
    }
}
