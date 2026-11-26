// SPDX-License-Identifier: MIT
//! Owned host messages: input, host service requests and their replies.

use std::{fmt, sync::Arc, time::Instant};

use crate::{
    NodeId, Point,
    render::{FrameId, Viewport},
};

pub use crate::journal::ListenerFlags;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down,
    Up,
    Move,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerType {
    Mouse,
    Pen,
    Touch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerSample {
    pub time: Instant,
    pub css: Point,
    pub pressure: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeltaMode {
    Pixel = 0,
    Line = 1,
    Page = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyKind {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImeKind {
    Start,
    Update,
    Commit,
    End,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InputTask {
    pub request: u32,
    pub target: NodeId,
    pub related: NodeId,
    pub time: Instant,
    pub modifiers: Modifiers,
    pub input: Input,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Pointer {
        kind: PointerKind,
        /// The last pointer position hit a different target after presentation.
        /// The pointer position and buttons themselves did not change.
        boundary_update: bool,
        /// Explicit click count supplied by test control, when present.
        click_count: Option<u32>,
        id: i32,
        device: PointerType,
        primary: bool,
        css: Point,
        pressure: f32,
        button: i16,
        buttons: u16,
        samples: Box<[PointerSample]>,
    },
    Wheel {
        css: Point,
        delta: [f64; 3],
        mode: DeltaMode,
    },
    Key {
        kind: KeyKind,
        key: String,
        code: String,
        location: u8,
        repeat: bool,
        composing: bool,
    },
    Ime {
        kind: ImeKind,
        text: String,
        selection_utf16: Option<[u32; 2]>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum HostTask {
    Input(InputTask),
    ScrollSync {
        frame: FrameId,
        tick: u64,
        offsets: Box<[crate::ScrollOffset]>,
    },
    Resize(Viewport),
    HostReply {
        request: u32,
        result: Result<HostValue, HostError>,
    },
    /// The window's fullscreen state changed without a document request, as
    /// when the user leaves fullscreen through the window system.
    FullscreenChanged {
        fullscreen: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardItem {
    pub mime: String,
    pub bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostRequest {
    ClipboardRead {
        id: u32,
    },
    ClipboardWrite {
        id: u32,
        items: Box<[ClipboardItem]>,
    },
    FileOpen {
        id: u32,
        accept: Box<[String]>,
        multiple: bool,
    },
    FileSave {
        id: u32,
        suggested_name: String,
        mime: String,
        bytes: Arc<[u8]>,
    },
    OpenExternal {
        id: u32,
        url: String,
    },
    /// Enter or leave borderless fullscreen for the whole window. The reply
    /// is `HostValue::Done`.
    Fullscreen {
        id: u32,
        enter: bool,
    },
}

/// Owned contents and metadata selected by a file dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHandle {
    pub name: String,
    pub mime: String,
    pub last_modified_ms: u64,
    pub bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostValue {
    Done,
    Clipboard(Box<[ClipboardItem]>),
    Files(Box<[FileHandle]>),
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    pub name: String,
    pub message: String,
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.name, self.message)
    }
}

impl std::error::Error for HostError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_flags_are_the_journal_type() {
        let flags: ListenerFlags = crate::journal::ListenerFlags::NON_PASSIVE_TOUCHMOVE;
        assert_eq!(flags.bits(), 2);
    }
}
