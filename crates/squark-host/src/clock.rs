// SPDX-License-Identifier: MIT

use std::time::{Duration, Instant};

use squark_js::document::DocumentTask;
use squark_types::render::{FrameDemand, FrameTick, Viewport};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::Sender,
};

pub(crate) const DEFAULT_REFRESH: Duration = Duration::from_nanos(16_666_667);

pub(crate) struct FrameClock {
    /// Redraws wait for the clock's next present time instead of the
    /// compositor's frame callback (X11, macOS and Windows).
    self_paced: bool,
    nominal: Duration,
    interval: Duration,
    encode: Duration,
    last_present: Option<Instant>,
    last_vsync: Option<Instant>,
    last_capped_target: Option<Instant>,
    minimum_interval: Option<Duration>,
    sequence: u64,
}

impl FrameClock {
    pub fn new(self_paced: bool) -> Self {
        Self {
            self_paced,
            nominal: DEFAULT_REFRESH,
            interval: DEFAULT_REFRESH,
            encode: Duration::from_millis(2),
            last_present: None,
            last_vsync: None,
            last_capped_target: None,
            minimum_interval: None,
            sequence: 0,
        }
    }

    pub fn refresh(&mut self, nominal: Duration) {
        let nominal = nominal.max(self.minimum_interval.unwrap_or_default());
        if nominal != self.nominal {
            self.nominal = nominal;
            self.reset();
        }
    }

    pub fn limit_refresh(&mut self, interval: Option<Duration>) {
        self.minimum_interval = interval;
    }

    /// Whether the compositor's frame callbacks pace redraws. A capped
    /// cadence paces itself with a timer. Waiting there for the callback of
    /// the commit just presented only delays each tick until the compositor's
    /// next repaint, up to a display refresh, and a tick that slips past half
    /// an interval restarts the cadence and loses a period.
    pub fn compositor_paced(&self) -> bool {
        self.minimum_interval.is_none()
    }

    pub fn reset(&mut self) {
        self.interval = self.nominal;
        self.last_present = None;
        self.last_capped_target = None;
    }

    pub fn tick(&mut self, vsync: Instant, viewport: Viewport) -> FrameTick {
        self.sequence += 1;
        let vsync = self.last_vsync.map_or(vsync, |last| last.max(vsync));
        self.last_vsync = Some(vsync);
        let mut present_at = vsync + self.interval;
        if self.minimum_interval.is_some() {
            // Keep the cadence anchored across ordinary event-loop jitter,
            // while starting a fresh interval after a stall or an idle gap.
            if let Some(previous) = self.last_capped_target
                && vsync >= previous
                && vsync - previous < self.interval / 2
            {
                present_at = previous + self.interval;
            }
            self.last_capped_target = Some(present_at);
        }
        let lead = self.encode.clamp(self.interval / 8, self.interval / 2);
        FrameTick {
            sequence: self.sequence,
            vsync,
            present_at,
            commit_deadline: present_at - lead,
            viewport,
        }
    }

    pub fn presented(&mut self, at: Instant, encode: Duration) {
        self.encode = (self.encode * 3 + encode) / 4;
        let previous = self.last_present.replace(at);
        if self.self_paced
            && self.minimum_interval.is_none()
            && let Some(previous) = previous
        {
            let observed = at.saturating_duration_since(previous);
            if (self.nominal / 2..=self.nominal * 3 / 2).contains(&observed) {
                self.interval = (self.interval * 7 + observed) / 8;
            }
        }
    }

    pub fn next_redraw(&self, tick: FrameTick, now: Instant) -> Instant {
        if self.self_paced || self.minimum_interval.is_some() {
            tick.present_at.max(now)
        } else {
            now
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct FrameWake(Arc<AtomicBool>);

impl FrameWake {
    pub fn pair(notify: impl Fn() + Send + Sync + 'static) -> (Self, FrameDemand) {
        let wake = Self::default();
        let pending = Arc::clone(&wake.0);
        let demand = FrameDemand::new(move || {
            pending.store(true, Ordering::Release);
            notify();
        });
        (wake, demand)
    }

    pub fn requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub fn require_tick(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn begin_tick(&self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Keep at most one tick in the runner's channel while it is executing JS.
/// A resize or redraw replaces any follow-up tick still on the render thread.
pub(crate) struct TickChannel {
    sender: Sender<DocumentTask>,
    pub wake: FrameWake,
    busy: bool,
    latest: Option<FrameTick>,
}

impl TickChannel {
    pub fn new(sender: Sender<DocumentTask>, wake: FrameWake) -> Self {
        Self {
            sender,
            wake,
            busy: false,
            latest: None,
        }
    }

    pub fn sender(&self) -> &Sender<DocumentTask> {
        &self.sender
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn offer(&mut self, tick: FrameTick) -> bool {
        self.latest = Some(tick);
        self.send_ready()
    }

    pub fn committed(&mut self) -> bool {
        self.busy = false;
        self.send_ready()
    }

    pub fn discard_pending(&mut self) {
        self.latest = None;
    }

    fn send_ready(&mut self) -> bool {
        if !self.busy
            && let Some(tick) = self.latest.take()
        {
            self.wake.begin_tick();
            self.busy = self.sender.send(tick.into()).is_ok();
            return self.busy;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use squark_types::Size;
    use std::sync::mpsc;

    fn viewport(epoch: u64) -> Viewport {
        Viewport {
            css: Size {
                width: 320.0,
                height: 240.0,
            },
            device: [320, 240],
            scale: 1.0,
            epoch,
        }
    }

    #[test]
    fn x11_uses_refresh_and_fifo_samples_with_a_deadline_before_present() {
        let now = Instant::now();
        let mut clock = FrameClock::new(true);
        clock.refresh(Duration::from_millis(10));
        let first = clock.tick(now, viewport(1));
        assert_eq!(first.vsync, now);
        assert_eq!(first.present_at, now + Duration::from_millis(10));
        assert!(first.vsync < first.commit_deadline && first.commit_deadline < first.present_at);
        assert_eq!(
            clock.next_redraw(first, first.commit_deadline),
            first.present_at
        );
        clock.presented(first.present_at, Duration::from_millis(1));
        let present = first.present_at + Duration::from_millis(12);
        clock.presented(present, Duration::from_millis(1));
        let next = clock.tick(present, viewport(2));
        assert_eq!(next.sequence, 2);
        assert_eq!(next.present_at - present, Duration::from_micros(10_250));
        clock.reset();
        assert_eq!(
            clock.tick(present, viewport(2)).present_at - present,
            Duration::from_millis(10)
        );
        assert_eq!(FrameClock::new(false).next_redraw(first, now), now);
    }

    #[test]
    fn only_an_uncapped_cadence_waits_for_compositor_frame_callbacks() {
        let mut clock = FrameClock::new(false);
        assert!(clock.compositor_paced());
        clock.limit_refresh(Some(DEFAULT_REFRESH));
        assert!(!clock.compositor_paced());
        clock.limit_refresh(None);
        assert!(clock.compositor_paced());
    }

    #[test]
    fn capped_presents_keep_their_phase_across_redraw_jitter() {
        let start = Instant::now();
        let mut clock = FrameClock::new(true);
        clock.limit_refresh(Some(DEFAULT_REFRESH));
        let first = clock.tick(start, viewport(1));
        let actual = first.present_at + Duration::from_millis(2);
        clock.presented(start, Duration::from_millis(1));
        clock.presented(actual, Duration::from_millis(1));
        let next = clock.tick(actual, viewport(1));
        assert_eq!(next.vsync, actual);
        assert_eq!(next.present_at, first.present_at + DEFAULT_REFRESH);
        let after_idle = start + Duration::from_secs(1);
        assert_eq!(
            clock.tick(after_idle, viewport(1)).present_at,
            after_idle + DEFAULT_REFRESH
        );
    }

    #[test]
    fn a_slow_document_gets_only_the_newest_queued_tick() {
        let now = Instant::now();
        let mut clock = FrameClock::new(false);
        let (send, receive) = mpsc::channel();
        let mut channel = TickChannel::new(send, FrameWake::default());
        for epoch in 1..=3 {
            assert!(channel.offer(clock.tick(now, viewport(epoch))));
        }
        let event = receive.try_recv().unwrap();
        assert!(matches!(
            event,
            DocumentTask::Frame(FrameTick { sequence: 1, .. })
        ));
        assert!(receive.try_recv().is_err());
        assert!(channel.committed());
        let event = receive.try_recv().unwrap();
        assert!(matches!(
            event,
            DocumentTask::Frame(FrameTick {
                sequence: 3,
                viewport: Viewport { epoch: 3, .. },
                ..
            })
        ));
    }
}
