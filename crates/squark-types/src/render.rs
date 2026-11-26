// SPDX-License-Identifier: MIT
//! Document-to-render types: frames, ticks, viewports and resources.

use std::{
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Instant,
};

use crate::{
    NodeId, Point, Size,
    fragment::{FragmentTree, MirrorRead},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub css: Size,
    pub device: [u32; 2],
    pub scale: f64,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTick {
    pub sequence: u64,
    pub vsync: Instant,
    pub present_at: Instant,
    pub commit_deadline: Instant,
    pub viewport: Viewport,
}

/// Coalesces document redraw requests until the next tick begins.
#[derive(Clone)]
pub struct FrameDemand(Arc<Demand>);

struct Demand {
    requested: AtomicBool,
    redraw: Box<dyn Fn() + Send + Sync>,
}

impl FrameDemand {
    pub fn new(redraw: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(Demand {
            requested: AtomicBool::new(false),
            redraw: Box::new(redraw),
        }))
    }

    pub fn request_frame(&self) {
        if !self.0.requested.swap(true, Ordering::AcqRel) {
            (self.0.redraw)();
        }
    }

    pub fn begin_tick(&self) {
        self.0.requested.store(false, Ordering::Release);
    }
}

impl Default for FrameDemand {
    fn default() -> Self {
        Self::new(|| {})
    }
}

impl fmt::Debug for FrameDemand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameDemand")
            .field("requested", &self.0.requested.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FrameId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaintError {
    MissingFragment(u64),
    InvalidFragment(u64),
    FrameIdExhausted,
}

impl fmt::Display for PaintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingFragment(id) => write!(f, "fragment {} is missing", id),
            Self::InvalidFragment(id) => write!(f, "fragment {} has invalid tree links", id),
            Self::FrameIdExhausted => f.write_str("the frame ID sequence is exhausted"),
        }
    }
}

impl std::error::Error for PaintError {}

/// Document-thread paint hooks. Only the render-side crates see the resulting frame.
pub trait FramePainter {
    fn commit(
        &mut self,
        tick: &FrameTick,
        fragments: &FragmentTree,
        mirror: &dyn MirrorRead,
    ) -> Result<FrameId, PaintError>;

    fn hit_test(
        &self,
        fragments: &FragmentTree,
        mirror: &dyn MirrorRead,
        point: Point,
    ) -> Option<NodeId>;
}

/// A position in the ordered resource stream. Zero denotes an empty prefix.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceSeq(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontInstanceId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font: FontInstanceId,
    pub glyph_id: u32,
    /// X and Y offsets in quarter raster pixels within the anchor pixel.
    pub phase: [u8; 2],
    pub scale: u32,
}

impl GlyphKey {
    /// Raster DPR in units of 1/1024. The phase is one of four bins per axis
    /// within the containing device pixel, including at negative positions.
    pub fn for_position(font: FontInstanceId, glyph_id: u32, point: Point, dpr: f64) -> Self {
        let scale = ((dpr * 1024.0).round() as u32).max(1);
        let phase = [point.x, point.y].map(|coordinate| {
            let device = f64::from(coordinate) * f64::from(scale) / 1024.0;
            ((device - device.floor()) * 4.0) as u8
        });
        Self {
            font,
            glyph_id,
            phase,
            scale,
        }
    }

    pub fn raster_scale(self) -> f32 {
        self.scale as f32 / 1024.0
    }

    /// Unshifted device-pixel anchor. The bitmap raster incorporates the phase.
    pub fn device_origin(self, point: Point) -> [f32; 2] {
        let dpr = f64::from(self.scale) / 1024.0;
        [point.x, point.y].map(|coordinate| (f64::from(coordinate) * dpr).floor() as f32)
    }
}

/// An immutable, generation-qualified decoded image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GradientKey(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResourceKey {
    Glyph(GlyphKey),
    Image(ImageKey),
    Gradient(GradientKey),
    Canvas(NodeId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitmapFormat {
    R8Coverage,
    Rgba8SrgbPremultiplied,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub size: [u32; 2],
    /// Bytes per row. Rows are top down and may include trailing padding.
    pub stride: u32,
    pub format: BitmapFormat,
    pub pixels: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResourceUpdate {
    /// An immutable font instance used by shaped Canvas 2D runs. Publish
    /// it before the first canvas submission that references its ID.
    PutFont {
        id: FontInstanceId,
        bytes: Arc<[u8]>,
        index: u32,
        size_bits: u32,
        coords: Arc<[i16]>,
    },
    PutGlyph {
        key: GlyphKey,
        bitmap: Bitmap,
        /// Bitmap origin measured rightward and upward from the device-pixel anchor.
        bearing: [i32; 2],
    },
    PutImage {
        key: ImageKey,
        bitmap: Bitmap,
    },
    PutGradient {
        key: GradientKey,
        ramp: Bitmap,
    },
    Free {
        key: ResourceKey,
    },
    ResizeCanvas {
        id: NodeId,
        size: [u32; 2],
    },
    SubmitCanvas {
        id: NodeId,
        stream: Arc<[u8]>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceMessage {
    pub seq: ResourceSeq,
    pub update: ResourceUpdate,
}

/// Clones share one ordered stream. The document thread sends worker
/// results through this stream before committing a dependent frame.
#[derive(Clone, Debug)]
pub struct ResourceSender {
    sender: mpsc::Sender<ResourceMessage>,
    watermark: Arc<Mutex<ResourceSeq>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceSendError {
    Disconnected,
    SequenceExhausted,
}

impl fmt::Display for ResourceSendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Disconnected => "the render resource receiver has disconnected",
            Self::SequenceExhausted => "the render resource sequence is exhausted",
        })
    }
}

impl std::error::Error for ResourceSendError {}

pub fn resource_channel() -> (ResourceSender, mpsc::Receiver<ResourceMessage>) {
    let (sender, receiver) = mpsc::channel();
    (
        ResourceSender {
            sender,
            watermark: Arc::new(Mutex::new(ResourceSeq(0))),
        },
        receiver,
    )
}

impl ResourceSender {
    pub fn send(&self, update: ResourceUpdate) -> Result<ResourceSeq, ResourceSendError> {
        let mut watermark = self
            .watermark
            .lock()
            .expect("resource sequence lock poisoned");
        let next = watermark
            .0
            .checked_add(1)
            .ok_or(ResourceSendError::SequenceExhausted)?;
        let seq = ResourceSeq(next);
        self.sender
            .send(ResourceMessage { seq, update })
            .map_err(|_| ResourceSendError::Disconnected)?;
        *watermark = seq;
        Ok(seq)
    }

    pub fn watermark(&self) -> ResourceSeq {
        *self
            .watermark
            .lock()
            .expect("resource sequence lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_keys_share_device_phase_and_an_integer_raster_anchor() {
        let point = Point { x: -1.0, y: 4.5 };
        let key = GlyphKey::for_position(FontInstanceId(17), 9, point, 1.5);
        assert_eq!(key.raster_scale(), 1.5);
        assert_eq!(key.phase, [2, 3]);
        assert_eq!(key.device_origin(point), [-2.0, 6.0]);
        assert_eq!(
            key,
            GlyphKey::for_position(FontInstanceId(17), 9, Point { x: 1.0, y: 6.5 }, 1.5),
        );
    }

    #[test]
    fn resource_sequence_follows_queue_order_and_publishes_only_success() {
        let (sender, receiver) = resource_channel();
        assert_eq!(sender.watermark(), ResourceSeq(0));
        std::thread::scope(|scope| {
            for generation in [0, 1] {
                let sender = sender.clone();
                scope.spawn(move || {
                    let id = NodeId::new(1, generation).unwrap();
                    for _ in 0..10 {
                        sender
                            .send(ResourceUpdate::ResizeCanvas { id, size: [10, 20] })
                            .unwrap();
                    }
                });
            }
        });
        for i in 1..=20 {
            assert_eq!(receiver.try_recv().unwrap().seq, ResourceSeq(i));
        }
        assert_eq!(sender.watermark(), ResourceSeq(20));

        drop(receiver);
        assert_eq!(
            sender.send(ResourceUpdate::Free {
                key: ResourceKey::Image(ImageKey(1))
            }),
            Err(ResourceSendError::Disconnected),
        );
        assert_eq!(sender.watermark(), ResourceSeq(20));
    }
}
