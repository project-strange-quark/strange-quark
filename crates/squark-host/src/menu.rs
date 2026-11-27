// SPDX-License-Identifier: MIT

//! The window menu's behaviour that needs no drawing: accelerators and
//! activation.
//!
//! As in Chromium and Electron on Linux and Windows, a key goes to the page
//! first. A keydown that matches an accelerator is sent with a reply
//! channel, and the item runs only if the page did not cancel it. So
//! a terminal that handles Ctrl+C keeps it, and a menu item that duplicates
//! an editor keybinding runs once. Editing roles (undo, copy, …) register
//! no accelerators: their keys are the page's own default actions.

use std::{
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
};

use serde_json::{Value, json};
use squark_js::document::{DocumentError, InputResult};
use squark_types::{
    host::{Input, InputTask, KeyKind, Modifiers},
    native::{Menu, MenuItem, MenuRole},
};

/// What activating a menu item does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Activation {
    /// A `MenuItem::Command`: tell the modules and the page.
    Command(String),
    /// Close and Quit roles: a close request, as the title bar's button.
    Close,
}

/// A parsed accelerator: exact modifiers and a physical key
/// (`KeyboardEvent.code`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Accelerator {
    modifiers: Modifiers,
    code: String,
}

impl Accelerator {
    /// Electron's and Tauri's syntax: `CmdOrCtrl+Shift+F`, `Alt+F4`, `Ctrl+=`.
    /// `CmdOrCtrl` is Command on macOS and Control elsewhere.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let mut modifiers = Modifiers::default();
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let Some((key, held)) = parts.split_last() else {
            return Err("empty accelerator".into());
        };
        for modifier in held {
            match modifier.to_ascii_lowercase().as_str() {
                "commandorcontrol" | "commandorctrl" | "cmdorctrl" | "cmdorcontrol" => {
                    if cfg!(target_os = "macos") {
                        modifiers.meta = true;
                    } else {
                        modifiers.ctrl = true;
                    }
                }
                "control" | "ctrl" => modifiers.ctrl = true,
                "command" | "cmd" | "super" | "meta" => modifiers.meta = true,
                "alt" | "option" => modifiers.alt = true,
                "shift" => modifiers.shift = true,
                other => return Err(format!("unknown modifier {other:?} in {text:?}")),
            }
        }
        let (code, shifted) =
            key_code(key).ok_or_else(|| format!("unknown key {key:?} in {text:?}"))?;
        modifiers.shift |= shifted;
        Ok(Self { modifiers, code })
    }

    fn matches(&self, modifiers: Modifiers, code: &str) -> bool {
        self.modifiers == modifiers && self.code == code
    }
}

/// A key name as `KeyboardEvent.code`, and whether it implies Shift (`Plus`,
/// `!` and the other shifted US-layout characters Electron accepts).
fn key_code(name: &str) -> Option<(String, bool)> {
    let shifted = match name {
        "Plus" | "plus" => Some("Equal"),
        ")" => Some("Digit0"),
        "!" => Some("Digit1"),
        "@" => Some("Digit2"),
        "#" => Some("Digit3"),
        "$" => Some("Digit4"),
        "%" => Some("Digit5"),
        "^" => Some("Digit6"),
        "&" => Some("Digit7"),
        "*" => Some("Digit8"),
        "(" => Some("Digit9"),
        ":" => Some("Semicolon"),
        "<" => Some("Comma"),
        "_" => Some("Minus"),
        ">" => Some("Period"),
        "?" => Some("Slash"),
        "~" => Some("Backquote"),
        "{" => Some("BracketLeft"),
        "|" => Some("Backslash"),
        "}" => Some("BracketRight"),
        "\"" => Some("Quote"),
        _ => None,
    };
    if let Some(code) = shifted {
        return Some((code.into(), true));
    }
    let mut chars = name.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        let code = match single {
            'a'..='z' | 'A'..='Z' => format!("Key{}", single.to_ascii_uppercase()),
            '0'..='9' => format!("Digit{single}"),
            '=' => "Equal".into(),
            '-' => "Minus".into(),
            ',' => "Comma".into(),
            '.' => "Period".into(),
            '/' => "Slash".into(),
            '\\' => "Backslash".into(),
            '[' => "BracketLeft".into(),
            ']' => "BracketRight".into(),
            ';' => "Semicolon".into(),
            '\'' => "Quote".into(),
            '`' => "Backquote".into(),
            _ => return None,
        };
        return Some((code, false));
    }
    let lower = name.to_ascii_lowercase();
    if let Some(number) = lower.strip_prefix('f')
        && let Ok(number) = number.parse::<u8>()
        && (1..=24).contains(&number)
    {
        return Some((format!("F{number}"), false));
    }
    if let Some(digit) = lower.strip_prefix("num")
        && let [digit @ b'0'..=b'9'] = digit.as_bytes()
    {
        return Some((format!("Numpad{}", *digit as char), false));
    }
    let code = match lower.as_str() {
        "space" => "Space",
        "tab" => "Tab",
        "backspace" => "Backspace",
        "delete" => "Delete",
        "insert" => "Insert",
        "return" | "enter" => "Enter",
        "up" | "arrowup" => "ArrowUp",
        "down" | "arrowdown" => "ArrowDown",
        "left" | "arrowleft" => "ArrowLeft",
        "right" | "arrowright" => "ArrowRight",
        "home" => "Home",
        "end" => "End",
        "pageup" => "PageUp",
        "pagedown" => "PageDown",
        "escape" | "esc" => "Escape",
        "capslock" => "CapsLock",
        "numlock" => "NumLock",
        "scrolllock" => "ScrollLock",
        "printscreen" => "PrintScreen",
        "numadd" => "NumpadAdd",
        "numsub" => "NumpadSubtract",
        "nummult" => "NumpadMultiply",
        "numdiv" => "NumpadDivide",
        "numdec" => "NumpadDecimal",
        "equal" => "Equal",
        "minus" => "Minus",
        "comma" => "Comma",
        "period" => "Period",
        "slash" => "Slash",
        "backslash" => "Backslash",
        "bracketleft" => "BracketLeft",
        "bracketright" => "BracketRight",
        "semicolon" => "Semicolon",
        "quote" => "Quote",
        "backquote" => "Backquote",
        // W3C code names, as muda accepts them: KeyA, Digit1, NumpadEnter.
        _ => return w3c_code(&lower),
    };
    Some((code.into(), false))
}

fn w3c_code(lower: &str) -> Option<(String, bool)> {
    let single = |rest: &str| (rest.len() == 1).then(|| rest.as_bytes()[0]);
    if let Some(letter) = lower.strip_prefix("key").and_then(single)
        && letter.is_ascii_lowercase()
    {
        return Some((format!("Key{}", letter.to_ascii_uppercase() as char), false));
    }
    if let Some(digit) = lower.strip_prefix("digit").and_then(single)
        && digit.is_ascii_digit()
    {
        return Some((format!("Digit{}", digit as char), false));
    }
    let rest = lower.strip_prefix("numpad")?;
    let code = match rest {
        "enter" => "NumpadEnter".into(),
        "add" => "NumpadAdd".into(),
        "subtract" => "NumpadSubtract".into(),
        "multiply" => "NumpadMultiply".into(),
        "divide" => "NumpadDivide".into(),
        "decimal" => "NumpadDecimal".into(),
        "equal" => "NumpadEqual".into(),
        _ => match single(rest) {
            Some(digit) if digit.is_ascii_digit() => format!("Numpad{}", digit as char),
            _ => return None,
        },
    };
    Some((code, false))
}

/// The window's menu and its accelerator table.
#[derive(Default)]
pub(crate) struct MenuState {
    menu: Option<Menu>,
    accelerators: Vec<(Accelerator, Activation)>,
}

impl MenuState {
    pub(crate) fn set(&mut self, menu: Option<Menu>) {
        self.accelerators.clear();
        if let Some(menu) = &menu {
            collect(&menu.items, &mut self.accelerators);
        }
        self.menu = menu;
    }

    /// The menu action a key starts, if the page leaves the key alone.
    pub(crate) fn accelerator(&self, task: &InputTask) -> Option<Activation> {
        let Input::Key {
            kind: KeyKind::Down,
            code,
            composing: false,
            ..
        } = &task.input
        else {
            return None;
        };
        self.accelerators
            .iter()
            .find(|(accelerator, _)| accelerator.matches(task.modifiers, code))
            .map(|(_, activation)| activation.clone())
    }

    /// The enabled command `id`, as an activation, if the menu has it.
    pub(crate) fn command(&self, id: &str) -> Option<Activation> {
        fn find(items: &[MenuItem], id: &str) -> bool {
            items.iter().any(|item| match item {
                MenuItem::Command {
                    id: item, enabled, ..
                } => *enabled && item == id,
                MenuItem::Submenu { enabled, items, .. } => *enabled && find(items, id),
                _ => false,
            })
        }
        let menu = self.menu.as_ref()?;
        find(&menu.items, id).then(|| Activation::Command(id.to_owned()))
    }

    /// The menu in the page API's template shape, for test control.
    pub(crate) fn to_json(&self) -> Value {
        self.menu
            .as_ref()
            .map_or(Value::Null, |menu| Value::Array(template(&menu.items)))
    }
}

fn collect(items: &[MenuItem], into: &mut Vec<(Accelerator, Activation)>) {
    for item in items {
        let (text, activation) = match item {
            MenuItem::Submenu { enabled, items, .. } => {
                if *enabled {
                    collect(items, into);
                }
                continue;
            }
            MenuItem::Command {
                id,
                accelerator: Some(text),
                enabled: true,
                ..
            } => (text.as_str(), Activation::Command(id.clone())),
            MenuItem::Role {
                role: role @ (MenuRole::Quit | MenuRole::CloseWindow),
                accelerator,
                ..
            } => {
                let default = match role {
                    MenuRole::Quit if cfg!(windows) => None,
                    MenuRole::Quit => Some("CmdOrCtrl+Q"),
                    _ => Some("CmdOrCtrl+W"),
                };
                let Some(text) = accelerator.as_deref().or(default) else {
                    continue;
                };
                (text, Activation::Close)
            }
            _ => continue,
        };
        match Accelerator::parse(text) {
            Ok(accelerator) => into.push((accelerator, activation)),
            Err(error) => eprintln!("squark-runtime: menu accelerator ignored: {error}"),
        }
    }
}

fn template(items: &[MenuItem]) -> Vec<Value> {
    items
        .iter()
        .map(|item| match item {
            MenuItem::Command {
                id,
                label,
                accelerator,
                enabled,
                checked,
            } => {
                let mut value = json!({ "id": id, "label": label, "enabled": enabled });
                if let Some(accelerator) = accelerator {
                    value["accelerator"] = json!(accelerator);
                }
                if let Some(checked) = checked {
                    value["type"] = json!("checkbox");
                    value["checked"] = json!(checked);
                }
                value
            }
            MenuItem::Submenu {
                label,
                enabled,
                items,
            } => json!({ "label": label, "enabled": enabled, "submenu": template(items) }),
            MenuItem::Separator => json!({ "type": "separator" }),
            MenuItem::Role {
                role,
                label,
                accelerator,
            } => {
                let name = match role {
                    MenuRole::Undo => "undo",
                    MenuRole::Redo => "redo",
                    MenuRole::Cut => "cut",
                    MenuRole::Copy => "copy",
                    MenuRole::Paste => "paste",
                    MenuRole::SelectAll => "selectAll",
                    MenuRole::CloseWindow => "close",
                    MenuRole::Quit => "quit",
                    MenuRole::About(_) => "about",
                };
                let mut value = json!({ "role": name });
                if let Some(label) = label {
                    value["label"] = json!(label);
                }
                if let Some(accelerator) = accelerator {
                    value["accelerator"] = json!(accelerator);
                }
                value
            }
        })
        .collect()
}

type Decision = Receiver<Result<InputResult, DocumentError>>;

/// Waits for the page's decision on accelerator keys, in order, on a thread
/// started with the first menu that has accelerators.
pub(crate) struct KeyDecisions {
    jobs: Sender<(Decision, Activation)>,
}

/// How long a page may take to handle a key before the accelerator is
/// dropped (a page that blocks its event loop that long has bigger issues).
const DECISION_TIMEOUT: Duration = Duration::from_secs(10);

impl KeyDecisions {
    /// `activate` runs on the decision thread; it returns false once the
    /// window has gone.
    pub(crate) fn start(activate: impl Fn(Activation) -> bool + Send + 'static) -> Self {
        let (jobs, received) = mpsc::channel::<(Decision, Activation)>();
        let _ = thread::Builder::new()
            .name("squark-menu-keys".into())
            .spawn(move || {
                for (decision, activation) in received {
                    let allowed = matches!(
                        decision.recv_timeout(DECISION_TIMEOUT),
                        Ok(Ok(result)) if !result.default_prevented
                    );
                    if allowed && !activate(activation) {
                        break;
                    }
                }
            });
        Self { jobs }
    }

    /// The reply channel to send with the key, whose decision activates
    /// `activation` unless the page cancels the key.
    pub(crate) fn watch(
        &self,
        activation: Activation,
    ) -> Sender<Result<InputResult, DocumentError>> {
        let (sender, decision) = mpsc::channel();
        let _ = self.jobs.send((decision, activation));
        sender
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use squark_types::{NodeId, native::AboutInfo};

    use super::*;

    fn ctrl() -> Modifiers {
        Modifiers {
            ctrl: !cfg!(target_os = "macos"),
            meta: cfg!(target_os = "macos"),
            ..Modifiers::default()
        }
    }

    fn key(code: &str, modifiers: Modifiers) -> InputTask {
        InputTask {
            request: 1,
            target: NodeId::NONE,
            related: NodeId::NONE,
            time: Instant::now(),
            modifiers,
            input: Input::Key {
                kind: KeyKind::Down,
                key: String::new(),
                code: code.into(),
                location: 0,
                repeat: false,
                composing: false,
            },
        }
    }

    #[test]
    fn accelerators_parse_electron_and_tauri_syntax() {
        let parse = |text| Accelerator::parse(text).unwrap();
        assert_eq!(
            parse("CmdOrCtrl+Shift+F"),
            Accelerator {
                modifiers: Modifiers {
                    shift: true,
                    ..ctrl()
                },
                code: "KeyF".into()
            }
        );
        assert_eq!(parse("CmdOrCtrl+=").code, "Equal");
        assert_eq!(parse("CmdOrCtrl+,").code, "Comma");
        assert_eq!(parse("CmdOrCtrl+Return").code, "Enter");
        assert_eq!(parse("CmdOrCtrl+F5").code, "F5");
        assert_eq!(parse("Alt+num7").code, "Numpad7");
        assert_eq!(parse("ctrl+KeyP").code, "KeyP");
        assert_eq!(parse("Ctrl+Digit3").code, "Digit3");
        assert_eq!(parse("Ctrl+NumpadEnter").code, "NumpadEnter");
        let plus = parse("Ctrl+Plus");
        assert_eq!((plus.code.as_str(), plus.modifiers.shift), ("Equal", true));
        assert!(parse("Super+Up").modifiers.meta);
        assert!(Accelerator::parse("Hyper+A").is_err());
        assert!(Accelerator::parse("Ctrl+Banana").is_err());
        assert!(Accelerator::parse("").is_err());
    }

    #[test]
    fn enabled_commands_and_close_roles_have_accelerators_editing_roles_do_not() {
        let mut state = MenuState::default();
        state.set(Some(Menu {
            items: vec![
                MenuItem::submenu(
                    "&File",
                    vec![
                        MenuItem::command("file.open", "Open").with_accelerator("CmdOrCtrl+O"),
                        MenuItem::Command {
                            id: "file.save".into(),
                            label: "Save".into(),
                            accelerator: Some("CmdOrCtrl+S".into()),
                            enabled: false,
                            checked: None,
                        },
                        MenuItem::role(MenuRole::Quit),
                        MenuItem::command("bad", "Bad").with_accelerator("Ctrl+Banana"),
                    ],
                ),
                MenuItem::submenu(
                    "Edit",
                    vec![
                        MenuItem::role(MenuRole::Copy),
                        MenuItem::role(MenuRole::About(AboutInfo::default())),
                    ],
                ),
            ],
        }));
        assert_eq!(
            state.accelerator(&key("KeyO", ctrl())),
            Some(Activation::Command("file.open".into()))
        );
        assert_eq!(state.accelerator(&key("KeyS", ctrl())), None, "disabled");
        assert_eq!(
            state.accelerator(&key("KeyC", ctrl())),
            None,
            "the page's copy"
        );
        assert_eq!(
            state.accelerator(&key("KeyO", Modifiers::default())),
            None,
            "modifiers must match exactly"
        );
        let quit = state.accelerator(&key("KeyQ", ctrl()));
        assert_eq!(quit, (!cfg!(windows)).then_some(Activation::Close));
        let mut up = key("KeyO", ctrl());
        if let Input::Key { kind, .. } = &mut up.input {
            *kind = KeyKind::Up;
        }
        assert_eq!(state.accelerator(&up), None);

        assert_eq!(
            state.command("file.open"),
            Some(Activation::Command("file.open".into()))
        );
        assert_eq!(state.command("file.save"), None);
        let json = state.to_json();
        assert_eq!(json[0]["label"], "&File");
        assert_eq!(json[0]["submenu"][0]["accelerator"], "CmdOrCtrl+O");
        assert_eq!(json[0]["submenu"][2], json!({ "role": "quit" }));
        assert_eq!(json[1]["submenu"][1]["role"], "about");

        state.set(None);
        assert_eq!(state.accelerator(&key("KeyO", ctrl())), None);
        assert_eq!(state.to_json(), Value::Null);
    }

    #[test]
    fn decisions_activate_only_keys_the_page_left_alone() {
        let (seen, activated) = mpsc::channel();
        let decisions = KeyDecisions::start(move |activation| seen.send(activation).is_ok());
        let first = decisions.watch(Activation::Command("kept".into()));
        let second = decisions.watch(Activation::Command("cancelled".into()));
        let third = decisions.watch(Activation::Close);
        third
            .send(Ok(InputResult {
                request: 3,
                default_prevented: false,
            }))
            .unwrap();
        second
            .send(Ok(InputResult {
                request: 2,
                default_prevented: true,
            }))
            .unwrap();
        first
            .send(Ok(InputResult {
                request: 1,
                default_prevented: false,
            }))
            .unwrap();
        let timeout = Duration::from_secs(5);
        assert_eq!(
            activated.recv_timeout(timeout),
            Ok(Activation::Command("kept".into()))
        );
        assert_eq!(activated.recv_timeout(timeout), Ok(Activation::Close));
        drop(decisions);
        assert!(activated.recv_timeout(timeout).is_err());
    }
}
