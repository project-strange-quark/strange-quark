// SPDX-License-Identifier: MIT

//! Menu templates from the page's native API (`window.setMenu`), in the
//! shape Electron's `Menu.buildFromTemplate` takes, read into the seam's
//! [`Menu`].

use serde_json::{Map, Value};
use squark_types::native::{AboutInfo, Menu, MenuItem, MenuRole};

/// Reads `null` (no menu) or an array of top-level items.
pub(crate) fn parse_menu(json: &str) -> Result<Option<Menu>, String> {
    let value: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    match value {
        Value::Null => Ok(None),
        Value::Array(values) => Ok(Some(Menu {
            items: items(&values, 0)?,
        })),
        _ => Err("a menu is an array of items, or null".into()),
    }
}

/// Deeper menus than this are certainly a mistake (or a cycle the page's
/// JSON.stringify would already have refused).
const MAX_DEPTH: usize = 16;

fn items(values: &[Value], depth: usize) -> Result<Vec<MenuItem>, String> {
    if depth > MAX_DEPTH {
        return Err("menu nests too deeply".into());
    }
    values.iter().map(|value| item(value, depth)).collect()
}

fn text(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("menu item {key} must be a string")),
    }
}

fn flag(object: &Map<String, Value>, key: &str, default: bool) -> Result<bool, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(format!("menu item {key} must be a boolean")),
    }
}

fn item(value: &Value, depth: usize) -> Result<MenuItem, String> {
    let Value::Object(object) = value else {
        return Err("a menu item is an object".into());
    };
    let kind = text(object, "type")?;
    if kind.as_deref() == Some("separator") {
        return Ok(MenuItem::Separator);
    }
    let label = text(object, "label")?;
    let accelerator = text(object, "accelerator")?;
    if let Some(role) = text(object, "role")? {
        return Ok(MenuItem::Role {
            role: role_named(&role, object)?,
            label,
            accelerator,
        });
    }
    let enabled = flag(object, "enabled", true)?;
    if let Some(submenu) = object.get("submenu") {
        let Value::Array(children) = submenu else {
            return Err("menu item submenu must be an array".into());
        };
        return Ok(MenuItem::Submenu {
            label: label.unwrap_or_default(),
            enabled,
            items: items(children, depth + 1)?,
        });
    }
    let id = text(object, "id")?.ok_or("a menu command needs an id")?;
    let checked = match kind.as_deref() {
        Some("checkbox") => Some(flag(object, "checked", false)?),
        None | Some("normal") => None,
        Some(other) => return Err(format!("unknown menu item type {other:?}")),
    };
    Ok(MenuItem::Command {
        id,
        label: label.unwrap_or_default(),
        accelerator,
        enabled,
        checked,
    })
}

fn role_named(name: &str, object: &Map<String, Value>) -> Result<MenuRole, String> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "undo" => MenuRole::Undo,
        "redo" => MenuRole::Redo,
        "cut" => MenuRole::Cut,
        "copy" => MenuRole::Copy,
        "paste" => MenuRole::Paste,
        "selectall" => MenuRole::SelectAll,
        "close" => MenuRole::CloseWindow,
        "quit" => MenuRole::Quit,
        "about" => {
            let about = match object.get("about") {
                None | Some(Value::Null) => Map::new(),
                Some(Value::Object(about)) => about.clone(),
                Some(_) => return Err("menu item about must be an object".into()),
            };
            MenuRole::About(AboutInfo {
                name: text(&about, "name")?,
                version: text(&about, "version")?,
                comments: text(&about, "comments")?,
                copyright: text(&about, "copyright")?,
                website: text(&about, "website")?,
            })
        }
        _ => return Err(format!("unknown menu role {name:?}")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn electron_templates_read_into_menus() {
        let menu = parse_menu(
            r#"[{"label":"&File","submenu":[
                {"id":"open","label":"Open…","accelerator":"CmdOrCtrl+O"},
                {"type":"separator"},
                {"id":"wrap","label":"Word Wrap","type":"checkbox","checked":true,"enabled":false},
                {"role":"selectAll"},
                {"role":"quit","label":"Exit"},
                {"role":"about","about":{"name":"Demo","version":"1.0"}}
            ]}]"#,
        )
        .unwrap()
        .unwrap();
        let MenuItem::Submenu { label, items, .. } = &menu.items[0] else {
            panic!("a submenu");
        };
        assert_eq!(label, "&File");
        assert_eq!(
            items[0],
            MenuItem::command("open", "Open…").with_accelerator("CmdOrCtrl+O")
        );
        assert_eq!(items[1], MenuItem::Separator);
        assert_eq!(
            items[2],
            MenuItem::Command {
                id: "wrap".into(),
                label: "Word Wrap".into(),
                accelerator: None,
                enabled: false,
                checked: Some(true),
            }
        );
        assert_eq!(items[3], MenuItem::role(MenuRole::SelectAll));
        assert_eq!(
            items[4],
            MenuItem::Role {
                role: MenuRole::Quit,
                label: Some("Exit".into()),
                accelerator: None,
            }
        );
        assert_eq!(
            items[5],
            MenuItem::role(MenuRole::About(AboutInfo {
                name: Some("Demo".into()),
                version: Some("1.0".into()),
                ..AboutInfo::default()
            }))
        );
        assert_eq!(parse_menu("null"), Ok(None));
    }

    #[test]
    fn malformed_templates_are_refused() {
        assert!(parse_menu("{}").is_err());
        assert!(
            parse_menu(r#"[{"label":"No id"}]"#)
                .unwrap_err()
                .contains("needs an id")
        );
        assert!(
            parse_menu(r#"[{"role":"fly"}]"#)
                .unwrap_err()
                .contains("unknown menu role")
        );
        assert!(parse_menu(r#"[{"id":"x","enabled":"yes"}]"#).is_err());
        assert!(parse_menu(r#"[{"label":"F","submenu":{}}]"#).is_err());
        let deep = (0..40).fold("[]".to_string(), |inner, _| {
            format!(r#"[{{"label":"x","submenu":{inner}}}]"#)
        });
        assert!(parse_menu(&deep).unwrap_err().contains("deeply"));
    }
}
