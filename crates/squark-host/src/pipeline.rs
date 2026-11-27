// SPDX-License-Identifier: MIT

use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

use squark_compositor::{Compositor, FrameMailbox, HitSnapshots, Painter, ScrollOffsets};
use squark_gpu::StaticRenderer;
use squark_js::document::{DocumentError, DocumentTask, InputResult};
use squark_paint::{CommittedFrame, HitSnapshot};
use squark_types::{
    host::InputTask,
    render::{FrameTick, ResourceMessage, ResourceSender, resource_channel},
};
use winit::dpi::PhysicalPosition;

use crate::canvas::{Backend, PendingCanvasRequests, ReadCanvas};
use crate::{HostError, failure, input::PresentedMouse, wheel::InputQueue};

pub(crate) fn connect() -> (DocumentPipeline, RenderPipeline, Arc<HitSnapshots>) {
    let mailbox = Arc::new(FrameMailbox::default());
    let (sender, receiver) = resource_channel();
    let hits = Arc::new(HitSnapshots::default());
    let scroll = Arc::new(ScrollOffsets::default());
    let document = DocumentPipeline {
        painter: Painter::new(mailbox.clone(), sender.clone())
            .with_scroll_offsets(Arc::clone(&scroll))
            .with_hit_snapshots(Arc::clone(&hits)),
        resources: sender,
        canvas: None,
        pending_canvas: Arc::new(PendingCanvasRequests::default()),
    };
    let render = RenderPipeline {
        compositor: Compositor::new(mailbox, Arc::clone(&hits))
            .with_scroll_offsets(Arc::clone(&scroll)),
        hits: Arc::clone(&hits),
        mouse: PresentedMouse::default(),
        receiver,
        input: InputQueue::new(scroll),
    };
    (document, render, hits)
}

pub(crate) struct DocumentPipeline {
    pub painter: Painter,
    pub resources: ResourceSender,
    pub canvas: Option<Backend>,
    pending_canvas: Arc<PendingCanvasRequests>,
}

impl DocumentPipeline {
    pub fn pending_canvas(&self) -> Arc<PendingCanvasRequests> {
        Arc::clone(&self.pending_canvas)
    }

    pub fn with_canvas(mut self, send: impl Fn(ReadCanvas) -> bool + Send + 'static) -> Self {
        self.canvas = Some(Backend::new(
            self.resources.clone(),
            Arc::clone(&self.pending_canvas),
            send,
        ));
        self
    }
}

pub(crate) struct RenderPipeline {
    compositor: Compositor,
    hits: Arc<HitSnapshots>,
    mouse: PresentedMouse,
    receiver: mpsc::Receiver<ResourceMessage>,
    pub input: InputQueue,
}

pub(crate) struct LatchedFrame<'a> {
    pub frame: &'a CommittedFrame,
    pub changed: bool,
    pub animating: bool,
}

#[derive(Default)]
pub(crate) struct Presentation {
    pub published_hits: bool,
    pub scrolled: bool,
    pub boundary_updated: bool,
}

impl RenderPipeline {
    pub fn read_canvas(&self, renderer: &mut StaticRenderer, request: ReadCanvas) {
        let pending = request
            .through
            .0
            .saturating_sub(renderer.applied_resources().0);
        // ResourceSender publishes its watermark after enqueuing. The
        // captured prefix is already readable, even while the document is
        // blocked inside its readback call and has no committed frame.
        let pixels = renderer
            .apply_resources(self.receiver.try_iter().take(pending as usize))
            .and_then(|_| renderer.read_canvas(request.id, request.through, request.region))
            .map_err(|error| error.to_string());
        let _ = request.reply.send(pixels);
    }

    pub fn current(&self) -> Option<&CommittedFrame> {
        self.compositor.current()
    }

    pub fn mark_presented(
        &mut self,
        tick: u64,
        time: Instant,
        tasks: &mpsc::Sender<DocumentTask>,
    ) -> Result<Presentation, HostError> {
        if !self.compositor.mark_presented() {
            return Ok(Presentation::default());
        }
        let offsets = self.compositor.take_scroll_echo();
        let scrolled = !offsets.is_empty();
        if scrolled {
            tasks
                .send(DocumentTask::ScrollSync {
                    frame: self.compositor.current().unwrap().id,
                    tick,
                    offsets: offsets.into_boxed_slice(),
                })
                .map_err(|_| failure("DocumentError", "document thread closed"))?;
        }
        let snapshot = self
            .hits
            .latest()
            .expect("a presentation published its hits");
        let boundary = self.mouse.boundary_update(&snapshot, time);
        let boundary_updated = boundary.is_some();
        if let Some(task) = boundary {
            self.input.dispatch(tasks, task, None, None)?;
        }
        Ok(Presentation {
            published_hits: true,
            scrolled,
            boundary_updated,
        })
    }

    pub fn dispatch_input(
        &mut self,
        tasks: &mpsc::Sender<DocumentTask>,
        mut task: InputTask,
        mouse: Option<PhysicalPosition<f64>>,
        snapshot: Option<Arc<HitSnapshot>>,
        decision: Option<mpsc::Sender<Result<InputResult, DocumentError>>>,
    ) -> Result<(), HostError> {
        self.mouse.record(&mut task, mouse);
        self.input.dispatch(tasks, task, snapshot, decision)
    }

    pub fn adopt(
        &mut self,
        renderer: &mut StaticRenderer,
        tick: FrameTick,
        paced: bool,
    ) -> Result<Option<LatchedFrame<'_>>, HostError> {
        let viewport = tick.viewport;
        let applied = renderer
            .drain_resources(&self.receiver)
            .map_err(|error| failure("GpuError", error))?;
        let deadline = (paced
            && self
                .current()
                .is_some_and(|frame| frame.viewport == viewport))
        .then_some(tick.commit_deadline);
        if let Some(adoption) = self.compositor.adopt_before(viewport, applied, deadline) {
            renderer
                .retire_resources(adoption.frame.frame.resource_updates)
                .map_err(|error| failure("GpuError", error))?;
        }
        if self
            .current()
            .is_none_or(|frame| frame.viewport != viewport)
        {
            return Ok(None);
        }
        let scrolled = self.compositor.latch_scroll();
        let animation = self.compositor.latch_animations(tick.vsync);
        Ok(self.current().map(|frame| LatchedFrame {
            frame,
            changed: scrolled || animation.changed,
            animating: animation.running,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use squark_layout::StaticDocument;
    use squark_types::{
        Point, Size,
        engine::{DocumentEngine, FrameInput},
        host::{Input, PointerKind},
        render::FramePainter,
    };
    use winit::event::{DeviceId, WindowEvent};

    use super::*;
    use crate::{control, input::InputRouter};

    #[test]
    fn scroll_presentation_reports_one_boundary_change_at_the_stationary_mouse() {
        let (mut document, mut render, hits) = connect();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/runtime-control.sqpak");
        let pack = crate::document::open_pack(&fixture).unwrap();
        let url = url::Url::parse("http://squark.invalid/").unwrap();
        let mut engine = StaticDocument::from_pack(&pack, url, document.resources.clone()).unwrap();
        let viewport = squark_types::render::Viewport {
            css: Size {
                width: 80.0,
                height: 60.0,
            },
            device: [160, 120],
            scale: 2.0,
            epoch: 1,
        };
        let tick = crate::static_tick(viewport);
        engine
            .update_layout(&FrameInput {
                tick,
                media: Default::default(),
                scroll: Default::default(),
            })
            .unwrap();
        let (tree, mirror) = engine.snapshot();
        document.painter.commit(&tick, tree, mirror).unwrap();
        render
            .compositor
            .adopt_latest(viewport, document.resources.watermark())
            .unwrap();
        let (tasks, received) = mpsc::channel();
        let now = Instant::now();
        assert!(
            !render
                .mark_presented(1, now, &tasks)
                .unwrap()
                .boundary_updated
        );
        let first_hit = hits.latest().unwrap();
        let mut input = InputRouter::default();
        let movement = input
            .event(
                &WindowEvent::CursorMoved {
                    device_id: DeviceId::dummy(),
                    position: PhysicalPosition::new(16.0, 20.0),
                },
                now,
                Some(viewport),
                Some(&first_hit),
            )
            .unwrap();
        let before = movement.target;
        render
            .dispatch_input(
                &tasks,
                movement,
                input.mouse_position(),
                Some(first_hit.clone()),
                None,
            )
            .unwrap();
        assert!(matches!(
            received.try_recv().unwrap(),
            DocumentTask::Input { .. }
        ));

        let wheel = input.synthetic(
            control::Request::Wheel(control::Wheel {
                x: 8.0,
                y: 10.0,
                dx: 0.0,
                dy: 20.0,
                modifiers: 0,
            }),
            now,
            viewport,
            Some(&first_hit),
        );
        render
            .dispatch_input(
                &tasks,
                wheel,
                input.mouse_position(),
                Some(first_hit.clone()),
                None,
            )
            .unwrap();
        assert!(matches!(
            received.try_recv().unwrap(),
            DocumentTask::Input { .. }
        ));
        assert!(render.compositor.latch_scroll());
        assert!(Arc::ptr_eq(&first_hit, &hits.latest().unwrap()));
        assert!(received.try_recv().is_err());

        let presented = render.mark_presented(2, now, &tasks).unwrap();
        assert!(presented.scrolled && presented.boundary_updated);
        let scrolled_hit = hits.latest().unwrap();
        assert_eq!(first_hit.frame, scrolled_hit.frame);
        assert!(matches!(
            received.try_recv().unwrap(),
            DocumentTask::ScrollSync { .. }
        ));
        let DocumentTask::Input { task, reply: None } = received.try_recv().unwrap() else {
            panic!("expected a one-way boundary update");
        };
        let point = Point { x: 8.0, y: 10.0 };
        assert_eq!(
            task.target,
            squark_paint::hit_test(&scrolled_hit, point).unwrap().target
        );
        assert_ne!(task.target, before);
        assert_eq!(task.related, before);
        assert_eq!(task.time, now);
        assert!(matches!(task.input, Input::Pointer {
            kind: PointerKind::Move, boundary_update: true, css, button: -1, buttons: 0, samples, ..
        } if css == point && samples.is_empty()));
        assert!(
            !render
                .mark_presented(2, now, &tasks)
                .unwrap()
                .boundary_updated
        );
        assert!(received.try_recv().is_err());
    }
}
