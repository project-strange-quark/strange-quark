// SPDX-License-Identifier: MIT
//! Tracks the document's fullscreen request against what the window reports,
//! so squark-host can tell the document when the window system leaves
//! fullscreen on its own.

/// The document's fullscreen request, as the window confirms it.
///
/// On Wayland, winit reports fullscreen from the last configure, so a resize
/// that arrives between a request and the compositor's fullscreen configure
/// still reads as windowed. Only a window that was seen fullscreen and is
/// then seen windowed, with no new request between, has left on its own.
/// Entering fullscreen through the window system (a compositor key, say) is
/// not a document fullscreen, as in Chromium, so it is never reported.
#[derive(Debug, Default)]
pub(crate) struct FullscreenTracker {
    requested: bool,
    confirmed: bool,
}

impl FullscreenTracker {
    /// The document asked to enter or leave fullscreen.
    pub(crate) fn request(&mut self, enter: bool) {
        self.requested = enter;
        self.confirmed = false;
    }

    /// The window reported its state after a resize. Returns the change to
    /// send to the document, if the window system left fullscreen.
    pub(crate) fn observe(&mut self, fullscreen: bool) -> Option<bool> {
        if !self.requested {
            return None;
        }
        if fullscreen {
            self.confirmed = true;
            return None;
        }
        if !self.confirmed {
            return None;
        }
        self.requested = false;
        self.confirmed = false;
        Some(false)
    }
}

#[cfg(test)]
mod tests {
    use super::FullscreenTracker;

    #[test]
    fn stale_windowed_configures_before_the_fullscreen_one_are_not_an_exit() {
        let mut tracker = FullscreenTracker::default();
        tracker.request(true);
        assert_eq!(tracker.observe(false), None);
        assert_eq!(tracker.observe(true), None);
        assert_eq!(tracker.observe(true), None);
        assert_eq!(tracker.observe(false), Some(false));
        assert_eq!(tracker.observe(false), None);
    }

    #[test]
    fn requested_exits_and_window_system_fullscreen_are_not_reported() {
        let mut tracker = FullscreenTracker::default();
        assert_eq!(tracker.observe(true), None);
        assert_eq!(tracker.observe(false), None);
        tracker.request(true);
        assert_eq!(tracker.observe(true), None);
        tracker.request(false);
        assert_eq!(tracker.observe(false), None);
        assert_eq!(tracker.observe(true), None);
    }
}
