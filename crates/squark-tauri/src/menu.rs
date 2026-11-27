// SPDX-License-Identifier: MIT
//! Tauri's `tauri::menu` builders over Strange Quark's window menu.
//!
//! The builders keep Tauri's signatures, so menu code written for Tauri
//! compiles unchanged, and produce `squark_native::Menu` data. Items are
//! values here, not live handles: set the menu again to change it.
//! Predefined items that only macOS has (Services, Hide, Show All) and the
//! window items Strange Quark has no action for are left out of the menu.
//! Activated items reach `squark_native::Module::on_menu` with their id.

use std::marker::PhantomData;

use squark_native::{AboutInfo, MenuItem as NativeItem, MenuRole};

use crate::{Manager, Result, Runtime, Wry};

/// `Debug` for the item handles, which hold a `PhantomData<fn() -> R>`.
macro_rules! debug_item {
    ($($name:ident),*) => {$(
        impl<R: Runtime> std::fmt::Debug for $name<R> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name)).field("item", &self.item).finish()
            }
        }
    )*};
}

debug_item!(MenuItem, PredefinedMenuItem, Submenu);

/// A menu item's id, as Tauri's.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MenuId(pub String);

impl MenuId {
    pub fn new(id: impl AsRef<str>) -> Self {
        Self(id.as_ref().to_owned())
    }
}

impl AsRef<str> for MenuId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<&str> for MenuId {
    fn from(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl From<String> for MenuId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl From<&String> for MenuId {
    fn from(id: &String) -> Self {
        Self(id.clone())
    }
}

impl PartialEq<&str> for MenuId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// An activated menu item, as Tauri's `on_menu_event` receives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuEvent {
    pub id: MenuId,
}

impl MenuEvent {
    pub fn id(&self) -> &MenuId {
        &self.id
    }
}

/// What a predefined About item shows, as Tauri's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AboutMetadata {
    pub name: Option<String>,
    pub version: Option<String>,
    pub short_version: Option<String>,
    pub authors: Option<Vec<String>>,
    pub comments: Option<String>,
    pub copyright: Option<String>,
    pub license: Option<String>,
    pub website: Option<String>,
    pub website_label: Option<String>,
    pub credits: Option<String>,
}

impl From<AboutMetadata> for AboutInfo {
    fn from(about: AboutMetadata) -> Self {
        Self {
            name: about.name,
            version: about.version,
            comments: about.comments,
            copyright: about.copyright,
            website: about.website,
        }
    }
}

/// Anything that goes in a menu or submenu.
pub trait IsMenuItem<R: Runtime> {
    /// The item as Strange Quark's menu model has it, or `None` for one
    /// it leaves out.
    #[doc(hidden)]
    fn __native(&self) -> Option<NativeItem>;
}

fn natives<R: Runtime>(items: &[&dyn IsMenuItem<R>]) -> impl Iterator<Item = NativeItem> {
    items.iter().filter_map(|item| item.__native())
}

/// A menu bar.
pub struct Menu<R: Runtime = Wry> {
    items: Vec<NativeItem>,
    runtime: PhantomData<fn() -> R>,
}

impl<R: Runtime> Clone for Menu<R> {
    fn clone(&self) -> Self {
        Self {
            items: self.items.clone(),
            runtime: PhantomData,
        }
    }
}

impl<R: Runtime> std::fmt::Debug for Menu<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Menu").field("items", &self.items).finish()
    }
}

impl<R: Runtime> Menu<R> {
    pub fn new<M: Manager<R>>(_manager: &M) -> Result<Self> {
        Ok(Self {
            items: Vec::new(),
            runtime: PhantomData,
        })
    }

    pub fn with_items<M: Manager<R>>(manager: &M, items: &[&dyn IsMenuItem<R>]) -> Result<Self> {
        let mut menu = Self::new(manager)?;
        menu.items.extend(natives(items));
        Ok(menu)
    }

    pub fn append(&mut self, item: &dyn IsMenuItem<R>) -> Result<()> {
        self.items.extend(item.__native());
        Ok(())
    }

    pub fn append_items(&mut self, items: &[&dyn IsMenuItem<R>]) -> Result<()> {
        self.items.extend(natives(items));
        Ok(())
    }

    /// The menu as Strange Quark's window menu.
    pub fn to_native(&self) -> squark_native::Menu {
        squark_native::Menu {
            items: self.items.clone(),
        }
    }
}

/// A plain item; activating it sends its id.
pub struct MenuItem<R: Runtime = Wry> {
    item: NativeItem,
    runtime: PhantomData<fn() -> R>,
}

impl<R: Runtime> Clone for MenuItem<R> {
    fn clone(&self) -> Self {
        Self {
            item: self.item.clone(),
            runtime: PhantomData,
        }
    }
}

impl<R: Runtime> MenuItem<R> {
    pub fn with_id<M: Manager<R>>(
        _manager: &M,
        id: impl Into<MenuId>,
        text: impl AsRef<str>,
        enabled: bool,
        accelerator: Option<&str>,
    ) -> Result<Self> {
        Ok(Self {
            item: NativeItem::Command {
                id: id.into().0,
                label: text.as_ref().to_owned(),
                accelerator: accelerator.map(str::to_owned),
                enabled,
                checked: None,
            },
            runtime: PhantomData,
        })
    }

    /// An item whose id is its text, as Tauri gives items without an id a
    /// generated one.
    pub fn new<M: Manager<R>>(
        manager: &M,
        text: impl AsRef<str>,
        enabled: bool,
        accelerator: Option<&str>,
    ) -> Result<Self> {
        let id = MenuId::new(&text);
        Self::with_id(manager, id, text, enabled, accelerator)
    }
}

impl<R: Runtime> IsMenuItem<R> for MenuItem<R> {
    fn __native(&self) -> Option<NativeItem> {
        Some(self.item.clone())
    }
}

/// A check item; activating it sends its id.
pub type CheckMenuItem<R = Wry> = MenuItem<R>;

/// Tauri's `MenuItemBuilder`.
#[derive(Clone, Debug)]
pub struct MenuItemBuilder {
    id: Option<MenuId>,
    text: String,
    enabled: bool,
    accelerator: Option<String>,
}

impl MenuItemBuilder {
    pub fn new(text: impl AsRef<str>) -> Self {
        Self {
            id: None,
            text: text.as_ref().to_owned(),
            enabled: true,
            accelerator: None,
        }
    }

    pub fn with_id(id: impl Into<MenuId>, text: impl AsRef<str>) -> Self {
        Self {
            id: Some(id.into()),
            ..Self::new(text)
        }
    }

    pub fn id(mut self, id: impl Into<MenuId>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn accelerator(mut self, accelerator: impl AsRef<str>) -> Self {
        self.accelerator = Some(accelerator.as_ref().to_owned());
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn build<R: Runtime, M: Manager<R>>(self, manager: &M) -> Result<MenuItem<R>> {
        let id = self.id.unwrap_or_else(|| MenuId::new(&self.text));
        MenuItem::with_id(
            manager,
            id,
            self.text,
            self.enabled,
            self.accelerator.as_deref(),
        )
    }
}

/// Tauri's `CheckMenuItemBuilder`.
#[derive(Clone, Debug)]
pub struct CheckMenuItemBuilder {
    item: MenuItemBuilder,
    checked: bool,
}

impl CheckMenuItemBuilder {
    pub fn new(text: impl AsRef<str>) -> Self {
        Self {
            item: MenuItemBuilder::new(text),
            checked: false,
        }
    }

    pub fn with_id(id: impl Into<MenuId>, text: impl AsRef<str>) -> Self {
        Self {
            item: MenuItemBuilder::with_id(id, text),
            checked: false,
        }
    }

    pub fn id(mut self, id: impl Into<MenuId>) -> Self {
        self.item = self.item.id(id);
        self
    }

    pub fn accelerator(mut self, accelerator: impl AsRef<str>) -> Self {
        self.item = self.item.accelerator(accelerator);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.item = self.item.enabled(enabled);
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn build<R: Runtime, M: Manager<R>>(self, manager: &M) -> Result<CheckMenuItem<R>> {
        let mut item = self.item.build(manager)?;
        if let NativeItem::Command { checked, .. } = &mut item.item {
            *checked = Some(self.checked);
        }
        Ok(item)
    }
}

/// Tauri's predefined items.
pub struct PredefinedMenuItem<R: Runtime = Wry> {
    item: Option<NativeItem>,
    runtime: PhantomData<fn() -> R>,
}

impl<R: Runtime> Clone for PredefinedMenuItem<R> {
    fn clone(&self) -> Self {
        Self {
            item: self.item.clone(),
            runtime: PhantomData,
        }
    }
}

impl<R: Runtime> IsMenuItem<R> for PredefinedMenuItem<R> {
    fn __native(&self) -> Option<NativeItem> {
        self.item.clone()
    }
}

impl<R: Runtime> PredefinedMenuItem<R> {
    fn of(item: Option<NativeItem>) -> Result<Self> {
        Ok(Self {
            item,
            runtime: PhantomData,
        })
    }

    fn role(role: MenuRole, text: Option<&str>) -> Result<Self> {
        Self::of(Some(NativeItem::Role {
            role,
            label: text.map(str::to_owned),
            accelerator: None,
        }))
    }

    pub fn separator<M: Manager<R>>(_manager: &M) -> Result<Self> {
        Self::of(Some(NativeItem::Separator))
    }
    pub fn undo<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::Undo, text)
    }
    pub fn redo<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::Redo, text)
    }
    pub fn cut<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::Cut, text)
    }
    pub fn copy<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::Copy, text)
    }
    pub fn paste<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::Paste, text)
    }
    pub fn select_all<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::SelectAll, text)
    }
    pub fn close_window<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::CloseWindow, text)
    }
    pub fn quit<M: Manager<R>>(_manager: &M, text: Option<&str>) -> Result<Self> {
        Self::role(MenuRole::Quit, text)
    }
    pub fn about<M: Manager<R>>(
        _manager: &M,
        text: Option<&str>,
        metadata: Option<AboutMetadata>,
    ) -> Result<Self> {
        Self::role(MenuRole::About(metadata.unwrap_or_default().into()), text)
    }
    /// macOS-only and window items Strange Quark has no action for: left out.
    pub fn minimize<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
    pub fn maximize<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
    pub fn fullscreen<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
    pub fn hide<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
    pub fn hide_others<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
    pub fn show_all<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
    pub fn services<M: Manager<R>>(_manager: &M, _text: Option<&str>) -> Result<Self> {
        Self::of(None)
    }
}

/// A submenu.
pub struct Submenu<R: Runtime = Wry> {
    item: NativeItem,
    runtime: PhantomData<fn() -> R>,
}

impl<R: Runtime> Clone for Submenu<R> {
    fn clone(&self) -> Self {
        Self {
            item: self.item.clone(),
            runtime: PhantomData,
        }
    }
}

impl<R: Runtime> IsMenuItem<R> for Submenu<R> {
    fn __native(&self) -> Option<NativeItem> {
        Some(self.item.clone())
    }
}

impl<R: Runtime> Submenu<R> {
    pub fn with_items<M: Manager<R>>(
        _manager: &M,
        text: impl AsRef<str>,
        enabled: bool,
        items: &[&dyn IsMenuItem<R>],
    ) -> Result<Self> {
        Ok(Self {
            item: NativeItem::Submenu {
                label: text.as_ref().to_owned(),
                enabled,
                items: natives(items).collect(),
            },
            runtime: PhantomData,
        })
    }
}

/// The item list shared by [`MenuBuilder`] and [`SubmenuBuilder`].
struct Items<'m, R: Runtime, M: Manager<R>> {
    manager: &'m M,
    items: Vec<NativeItem>,
    runtime: PhantomData<fn() -> R>,
}

impl<'m, R: Runtime, M: Manager<R>> Items<'m, R, M> {
    fn new(manager: &'m M) -> Self {
        Self {
            manager,
            items: Vec::new(),
            runtime: PhantomData,
        }
    }

    fn push(&mut self, item: Result<impl IsMenuItem<R>>) {
        // Building predefined and plain items cannot fail here.
        if let Ok(item) = item {
            self.items.extend(item.__native());
        }
    }
}

macro_rules! item_methods {
    () => {
        pub fn item(mut self, item: &dyn IsMenuItem<R>) -> Self {
            self.list.items.extend(item.__native());
            self
        }

        pub fn items(mut self, items: &[&dyn IsMenuItem<R>]) -> Self {
            self.list.items.extend(natives(items));
            self
        }

        pub fn text(mut self, id: impl Into<MenuId>, text: impl AsRef<str>) -> Self {
            let item = MenuItemBuilder::with_id(id, text).build(self.list.manager);
            self.list.push(item);
            self
        }

        pub fn check(mut self, id: impl Into<MenuId>, text: impl AsRef<str>) -> Self {
            let item = CheckMenuItemBuilder::with_id(id, text).build(self.list.manager);
            self.list.push(item);
            self
        }

        pub fn separator(mut self) -> Self {
            self.list.items.push(NativeItem::Separator);
            self
        }

        pub fn undo(mut self) -> Self {
            let item = PredefinedMenuItem::undo(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn redo(mut self) -> Self {
            let item = PredefinedMenuItem::redo(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn cut(mut self) -> Self {
            let item = PredefinedMenuItem::cut(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn copy(mut self) -> Self {
            let item = PredefinedMenuItem::copy(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn paste(mut self) -> Self {
            let item = PredefinedMenuItem::paste(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn select_all(mut self) -> Self {
            let item = PredefinedMenuItem::select_all(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn close_window(mut self) -> Self {
            let item = PredefinedMenuItem::close_window(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn quit(mut self) -> Self {
            let item = PredefinedMenuItem::quit(self.list.manager, None);
            self.list.push(item);
            self
        }

        pub fn about(mut self, metadata: Option<AboutMetadata>) -> Self {
            let item = PredefinedMenuItem::about(self.list.manager, None, metadata);
            self.list.push(item);
            self
        }
    };
}

impl<R: Runtime, M: Manager<R>> std::fmt::Debug for Items<'_, R, M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(&self.items).finish()
    }
}

/// Tauri's `MenuBuilder`.
#[derive(Debug)]
pub struct MenuBuilder<'m, R: Runtime, M: Manager<R>> {
    list: Items<'m, R, M>,
}

impl<'m, R: Runtime, M: Manager<R>> MenuBuilder<'m, R, M> {
    pub fn new(manager: &'m M) -> Self {
        Self {
            list: Items::new(manager),
        }
    }

    item_methods!();

    pub fn build(self) -> Result<Menu<R>> {
        Ok(Menu {
            items: self.list.items,
            runtime: PhantomData,
        })
    }
}

/// Tauri's `SubmenuBuilder`.
#[derive(Debug)]
pub struct SubmenuBuilder<'m, R: Runtime, M: Manager<R>> {
    list: Items<'m, R, M>,
    text: String,
    enabled: bool,
}

impl<'m, R: Runtime, M: Manager<R>> SubmenuBuilder<'m, R, M> {
    pub fn new(manager: &'m M, text: impl AsRef<str>) -> Self {
        Self {
            list: Items::new(manager),
            text: text.as_ref().to_owned(),
            enabled: true,
        }
    }

    /// Submenus have no id in Strange Quark's menu; the id is ignored.
    pub fn with_id(manager: &'m M, _id: impl Into<MenuId>, text: impl AsRef<str>) -> Self {
        Self::new(manager, text)
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    item_methods!();

    pub fn build(self) -> Result<Submenu<R>> {
        Ok(Submenu {
            item: NativeItem::Submenu {
                label: self.text,
                enabled: self.enabled,
                items: self.list.items,
            },
            runtime: PhantomData,
        })
    }
}
