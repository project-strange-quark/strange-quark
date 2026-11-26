// SPDX-License-Identifier: MIT

use std::time::Instant;

use squark_types::{Rect, host::ImeKind};
use winit::{
    dpi::{PhysicalPosition, PhysicalSize},
    event::Ime,
    window::Window,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CursorArea {
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
}

impl CursorArea {
    pub fn new(rect: Rect, scale: f64) -> Option<Self> {
        let [left, top, right, bottom] = [rect.origin.x, rect.origin.y, rect.max_x(), rect.max_y()]
            .map(|edge| f64::from(edge) * scale);
        if ![left, top, right, bottom].into_iter().all(f64::is_finite) {
            return None;
        }
        let x = left.floor();
        let y = top.floor();
        Some(Self {
            position: PhysicalPosition::new(x as i32, y as i32),
            size: PhysicalSize::new(
                (right.ceil() - x).max(1.0) as u32,
                (bottom.ceil() - y).max(1.0) as u32,
            ),
        })
    }

    pub fn apply(self, window: &Window) {
        window.set_ime_cursor_area(self.position, self.size);
    }
}

#[derive(Debug)]
pub(crate) struct Edit {
    pub kind: ImeKind,
    pub text: String,
    pub selection_utf16: Option<[u32; 2]>,
    pub time: Instant,
}

#[derive(Default)]
pub(crate) struct ImeState {
    pub area: Option<CursorArea>,
    enabled: bool,
    composing: bool,
    empty: Option<Edit>,
}

impl ImeState {
    pub fn composing(&self) -> bool {
        self.composing
    }

    pub fn set_area(&mut self, area: Option<CursorArea>, time: Instant) -> Option<Edit> {
        let end = if area.is_none() {
            self.empty = None;
            self.disable(time)
        } else {
            None
        };
        self.area = area;
        end
    }

    pub fn flush(&mut self) -> Option<Edit> {
        self.empty.take()
    }

    pub fn pending_empty(&self) -> bool {
        self.empty.is_some()
    }

    pub fn control_event(&mut self, event: Ime, time: Instant) -> [Option<Edit>; 2] {
        if !matches!(&event, Ime::Disabled) {
            self.enabled = true;
        }
        self.event(event, time)
    }

    pub fn event(&mut self, event: Ime, time: Instant) -> [Option<Edit>; 2] {
        let before = self.flush().filter(|_| !matches!(event, Ime::Commit(_)));
        let edit = match event {
            Ime::Enabled => {
                self.enabled = true;
                None
            }
            Ime::Disabled => self.disable(time),
            _ if !self.enabled => None,
            Ime::Preedit(text, cursor) => {
                let selection_utf16 = cursor.and_then(|(anchor, focus)| {
                    Some([utf16_offset(&text, anchor)?, utf16_offset(&text, focus)?])
                });
                if text.is_empty() && !self.composing {
                    return [before, None];
                }
                let empty = text.is_empty();
                let edit = self.edit(ImeKind::Update, text, selection_utf16, time);
                if empty {
                    // Winit sends a synthetic clear immediately before a Commit.
                    // A standalone clear must still reach a live composition.
                    self.empty = edit;
                    None
                } else {
                    self.composing = edit.is_some();
                    edit
                }
            }
            Ime::Commit(text) => {
                self.composing = false;
                self.edit(ImeKind::Commit, text, None, time)
            }
        };
        [before, edit]
    }

    pub fn disable(&mut self, time: Instant) -> Option<Edit> {
        self.enabled = false;
        if std::mem::take(&mut self.composing) {
            self.edit(ImeKind::End, String::new(), None, time)
        } else {
            None
        }
    }

    fn edit(
        &self,
        kind: ImeKind,
        text: String,
        selection_utf16: Option<[u32; 2]>,
        time: Instant,
    ) -> Option<Edit> {
        self.area?;
        Some(Edit {
            kind,
            text,
            selection_utf16,
            time,
        })
    }
}

fn utf16_offset(text: &str, utf8: usize) -> Option<u32> {
    text.get(..utf8)?.encode_utf16().count().try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ImeState {
        ImeState {
            area: CursorArea::new(Rect::default(), 1.0),
            enabled: true,
            ..ImeState::default()
        }
    }

    #[test]
    fn winit_preedit_converts_byte_offsets_and_omits_the_clear_before_commit() {
        let mut state = state();
        let now = Instant::now();
        let [None, Some(preedit)] = state.event(Ime::Preedit("a😀日".into(), Some((5, 1))), now)
        else {
            panic!("expected a preedit");
        };
        assert_eq!(preedit.kind, ImeKind::Update);
        assert_eq!(preedit.selection_utf16, Some([3, 1]));
        assert!(state.composing());
        assert!(matches!(
            state.event(Ime::Preedit(String::new(), None), now),
            [None, None]
        ));
        let [None, Some(commit)] = state.event(Ime::Commit("日本".into()), now) else {
            panic!("expected a commit without an intermediate clear");
        };
        assert_eq!(commit.kind, ImeKind::Commit);
        assert_eq!(commit.text, "日本");
        assert!(!state.composing());
    }

    #[test]
    fn test_control_uses_the_winit_mapping_and_drops_the_empty_preedit_before_commit() {
        let mut state = ImeState {
            area: CursorArea::new(Rect::default(), 1.0),
            ..ImeState::default()
        };
        let now = Instant::now();
        let [None, Some(preedit)] =
            state.control_event(Ime::Preedit("日本".into(), Some((6, 6))), now)
        else {
            panic!("expected a control preedit");
        };
        assert_eq!(preedit.kind, ImeKind::Update);
        assert_eq!(preedit.selection_utf16, Some([2, 2]));
        assert!(matches!(
            state.control_event(Ime::Preedit(String::new(), None), now),
            [None, None]
        ));
        let [None, Some(commit)] = state.control_event(Ime::Commit("日本".into()), now) else {
            panic!("expected a commit without an intermediate clear");
        };
        assert_eq!(commit.kind, ImeKind::Commit);
        assert_eq!(commit.text, "日本");
    }

    #[test]
    fn standalone_empty_preedit_keeps_the_session_alive_until_disable() {
        let mut state = state();
        let now = Instant::now();
        let [None, Some(preedit)] = state.event(Ime::Preedit("😀".into(), Some((1, 4))), now)
        else {
            panic!("expected a preedit");
        };
        assert_eq!(preedit.selection_utf16, None);
        state.event(Ime::Preedit(String::new(), None), now);
        let empty = state.flush().unwrap();
        assert_eq!(empty.kind, ImeKind::Update);
        assert_eq!(empty.text, "");
        assert!(state.composing());
        let [None, Some(end)] = state.event(Ime::Disabled, now) else {
            panic!("expected an end event");
        };
        assert_eq!(end.kind, ImeKind::End);
        assert!(!state.composing());
    }

    #[test]
    fn cursor_area_encloses_the_caret_in_physical_pixels() {
        assert_eq!(
            CursorArea::new(Rect::new(15.25, 21.5, 0.0, 10.75), 1.5),
            Some(CursorArea {
                position: PhysicalPosition::new(22, 32),
                size: PhysicalSize::new(1, 17),
            })
        );
    }

    #[test]
    fn removing_the_area_ends_composition_and_ignores_late_commits() {
        let mut state = state();
        let now = Instant::now();
        state.event(Ime::Preedit("a".into(), None), now);
        let end = state.set_area(None, now).unwrap();
        assert_eq!(end.kind, ImeKind::End);
        assert!(matches!(
            state.event(Ime::Commit("stale".into()), now),
            [None, None]
        ));
    }
}
