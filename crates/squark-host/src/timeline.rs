// SPDX-License-Identifier: MIT

use std::{
    env,
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use serde::Serialize;
use squark_paint::CommittedFrame;
use squark_types::render::FrameTick;

use crate::{HostError, failure};

const MAX_SAMPLES: usize = 8192;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrameSample {
    sequence: u64,
    epoch: u64,
    vsync_ns: u64,
    deadline_ns: u64,
    present_at_ns: u64,
    latch_ns: u64,
    frame_id: Option<u64>,
    commit_sequence: Option<u64>,
    paint_start_ns: Option<u64>,
    commit_ns: Option<u64>,
    encode_end_ns: Option<u64>,
    submit_ns: Option<u64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Recording {
    adapter: String,
    frames: Vec<FrameSample>,
    wheel_ns: Vec<u64>,
    dropped: usize,
}

pub(crate) struct Timeline {
    start: Instant,
    path: Option<PathBuf>,
    samples: Recording,
    #[cfg(debug_assertions)]
    puffin: Option<(PathBuf, puffin::GlobalFrameView)>,
}

impl Timeline {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            path: env::var_os("SQUARK_FRAME_TIMELINE").map(PathBuf::from),
            samples: Recording::default(),
            #[cfg(debug_assertions)]
            puffin: env::var_os("SQUARK_PUFFIN_TRACE").map(|path| {
                let view = puffin::GlobalFrameView::default();
                {
                    let mut view = view.lock();
                    view.set_max_recent(MAX_SAMPLES);
                    view.set_pack_frames(false);
                }
                puffin::set_scopes_on(true);
                (PathBuf::from(path), view)
            }),
        }
    }

    pub fn set_adapter(&mut self, adapter: &str) {
        self.samples.adapter = adapter.into();
    }

    pub fn cadence_limit(&self) -> Result<Option<Duration>, HostError> {
        let Some(rate) = self
            .path
            .as_ref()
            .and_then(|_| env::var_os("SQUARK_FRAME_TIMELINE_HZ"))
        else {
            return Ok(None);
        };
        let hz = rate
            .to_str()
            .and_then(|rate| rate.parse::<u32>().ok())
            .filter(|hz| (1..=1000).contains(hz))
            .ok_or_else(|| {
                failure(
                    "FrameTimelineError",
                    "SQUARK_FRAME_TIMELINE_HZ must be 1–1000",
                )
            })?;
        Ok(Some(Duration::from_secs_f64(1.0 / f64::from(hz))))
    }

    fn ns(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.start)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }

    pub fn wheel(&mut self, at: Instant) {
        if self.path.is_some() && self.samples.wheel_ns.len() < MAX_SAMPLES {
            self.samples.wheel_ns.push(self.ns(at));
        }
    }

    pub fn latch(&self, tick: FrameTick) -> Option<FrameSample> {
        #[cfg(debug_assertions)]
        if self.puffin.is_some() {
            puffin::GlobalProfiler::lock().new_frame();
        }
        self.path.as_ref().map(|_| FrameSample {
            sequence: tick.sequence,
            epoch: tick.viewport.epoch,
            vsync_ns: self.ns(tick.vsync),
            deadline_ns: self.ns(tick.commit_deadline),
            present_at_ns: self.ns(tick.present_at),
            latch_ns: self.ns(Instant::now()),
            frame_id: None,
            commit_sequence: None,
            paint_start_ns: None,
            commit_ns: None,
            encode_end_ns: None,
            submit_ns: None,
        })
    }

    pub fn frame(&self, sample: &mut Option<FrameSample>, frame: &CommittedFrame) {
        if let Some(sample) = sample {
            sample.frame_id = Some(frame.id.0);
            if let Some(commit) = frame.publication {
                sample.commit_sequence = Some(commit.sequence);
                sample.paint_start_ns = Some(self.ns(commit.paint_started));
                sample.commit_ns = Some(self.ns(commit.committed));
            }
        }
    }

    pub fn encoded(&self, sample: &mut Option<FrameSample>) {
        if let Some(sample) = sample {
            sample.encode_end_ns = Some(self.ns(Instant::now()));
        }
    }

    pub fn finish_frame(&mut self, sample: Option<FrameSample>, submitted: bool) {
        if let Some(mut sample) = sample {
            if self.samples.frames.len() == MAX_SAMPLES {
                self.samples.dropped += 1;
                return;
            }
            sample.submit_ns = submitted.then(|| self.ns(Instant::now()));
            self.samples.frames.push(sample);
        }
    }

    pub fn write(self) -> Result<(), HostError> {
        let error = |error: &dyn std::fmt::Display| failure("FrameTimelineError", error);
        if let Some(path) = self.path {
            let mut file = BufWriter::new(File::create(path).map_err(|err| error(&err))?);
            serde_json::to_writer(&mut file, &self.samples).map_err(|err| error(&err))?;
            file.flush().map_err(|err| error(&err))?;
        }
        #[cfg(debug_assertions)]
        if let Some((path, view)) = self.puffin {
            puffin::GlobalProfiler::lock().new_frame();
            puffin::set_scopes_on(false);
            let mut file = BufWriter::new(File::create(path).map_err(|err| error(&err))?);
            view.lock().write(&mut file).map_err(|err| error(&err))?;
            file.flush().map_err(|err| error(&err))?;
        }
        Ok(())
    }
}
