// SPDX-License-Identifier: MIT

use std::{
    collections::HashMap,
    mem,
    sync::{Mutex, mpsc},
};

use image::{
    ExtendedColorType, ImageEncoder,
    codecs::{jpeg::JpegEncoder, png::PngEncoder, webp::WebPEncoder},
};
use squark_gpu::CanvasPixels;
use squark_types::{
    NodeId,
    render::{ResourceSender, ResourceSeq},
};

use crate::control::REQUEST_TIMEOUT;

#[derive(Debug)]
pub(crate) struct ReadCanvas {
    pub id: NodeId,
    pub through: ResourceSeq,
    pub region: Option<([i32; 2], [u32; 2])>,
    pub reply: mpsc::Sender<Result<CanvasPixels, String>>,
}

#[derive(Default)]
pub(crate) struct PendingCanvasRequests {
    state: Mutex<PendingCanvasState>,
}

#[derive(Default)]
struct PendingCanvasState {
    next_id: u64,
    replies: HashMap<u64, mpsc::Sender<Result<CanvasPixels, String>>>,
}

impl PendingCanvasRequests {
    fn register(&self, reply: mpsc::Sender<Result<CanvasPixels, String>>) -> u64 {
        let mut state = self.state.lock().unwrap();
        state.next_id = state
            .next_id
            .checked_add(1)
            .expect("canvas request ID exhausted");
        let id = state.next_id;
        state.replies.insert(id, reply);
        id
    }

    fn complete(&self, id: u64) {
        self.state.lock().unwrap().replies.remove(&id);
    }

    pub(crate) fn fail_all(&self) {
        let replies = mem::take(&mut self.state.lock().unwrap().replies);
        for reply in replies.into_values() {
            let _ = reply.send(Err("document is shutting down".into()));
        }
    }
}

pub(crate) struct Backend {
    resources: ResourceSender,
    pending: std::sync::Arc<PendingCanvasRequests>,
    send: Box<dyn Fn(ReadCanvas) -> bool + Send>,
}

impl Backend {
    pub fn new(
        resources: ResourceSender,
        pending: std::sync::Arc<PendingCanvasRequests>,
        send: impl Fn(ReadCanvas) -> bool + Send + 'static,
    ) -> Self {
        Self {
            resources,
            pending,
            send: Box::new(send),
        }
    }

    fn capture(
        &self,
        id: NodeId,
        region: Option<([i32; 2], [u32; 2])>,
    ) -> Result<CanvasPixels, String> {
        let through = self.resources.watermark();
        let (reply, response) = mpsc::channel();
        let request_id = self.pending.register(reply.clone());
        if !(self.send)(ReadCanvas {
            id,
            through,
            region,
            reply,
        }) {
            self.pending.complete(request_id);
            return Err("canvas render thread closed".into());
        }
        let result = response
            .recv_timeout(REQUEST_TIMEOUT)
            .map_err(|error| format!("canvas readback: {error}"));
        self.pending.complete(request_id);
        result?
    }
}

impl squark_js::CanvasBackend for Backend {
    fn readback(
        &mut self,
        canvas: NodeId,
        origin: [i32; 2],
        size: [u32; 2],
    ) -> Result<Vec<u8>, String> {
        Ok(self.capture(canvas, Some((origin, size)))?.rgba)
    }

    fn encode(
        &mut self,
        canvas: NodeId,
        mime: &str,
        quality: Option<f64>,
    ) -> Result<Vec<u8>, String> {
        let pixels = self.capture(canvas, None)?;
        encode(pixels, mime, quality)
    }
}

fn encode(
    CanvasPixels {
        size: [width, height],
        rgba,
    }: CanvasPixels,
    mime: &str,
    quality: Option<f64>,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("cannot encode an empty canvas".into());
    }
    let mut bytes = Vec::new();
    match mime {
        "image/jpeg" => {
            let quality = quality
                .filter(|value| (0.0..=1.0).contains(value))
                .unwrap_or(0.92);
            let rgb: Vec<_> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[red, green, blue, alpha]| {
                    [red, green, blue]
                        .map(|channel| ((u16::from(channel) * u16::from(alpha) + 127) / 255) as u8)
                })
                .collect();
            JpegEncoder::new_with_quality(&mut bytes, (quality * 100.0).round() as u8).encode(
                &rgb,
                width,
                height,
                ExtendedColorType::Rgb8,
            )
        }
        "image/webp" => WebPEncoder::new_lossless(&mut bytes).write_image(
            &rgba,
            width,
            height,
            ExtendedColorType::Rgba8,
        ),
        _ => {
            PngEncoder::new(&mut bytes).write_image(&rgba, width, height, ExtendedColorType::Rgba8)
        }
    }
    .map_err(|error| format!("canvas encode: {error}"))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageFormat;

    #[test]
    fn shutdown_fails_every_pending_canvas_reply() {
        let pending = PendingCanvasRequests::default();
        let (reply, response) = mpsc::channel();
        let request = pending.register(reply);

        pending.fail_all();

        assert!(matches!(
            response.recv().unwrap(),
            Err(error) if error == "document is shutting down"
        ));
        pending.complete(request);
        assert!(pending.state.lock().unwrap().replies.is_empty());
    }

    #[test]
    fn canvas_encodings_round_trip_pixels_and_fall_back_to_png() {
        let pixels = || CanvasPixels {
            size: [16, 16],
            rgba: [202, 102, 48, 128].repeat(256),
        };
        for (mime, format, expected) in [
            ("image/png", ImageFormat::Png, [202, 102, 48, 128]),
            ("image/webp", ImageFormat::WebP, [202, 102, 48, 128]),
            ("image/unsupported", ImageFormat::Png, [202, 102, 48, 128]),
            ("image/jpeg", ImageFormat::Jpeg, [101, 51, 24, 255]),
        ] {
            let bytes = encode(pixels(), mime, Some(1.0)).unwrap();
            assert_eq!(image::guess_format(&bytes).unwrap(), format);
            let decoded = image::load_from_memory(&bytes).unwrap().into_rgba8();
            assert_eq!(decoded.dimensions(), (16, 16));
            let tolerance = if format == ImageFormat::Jpeg { 2 } else { 0 };
            for (actual, expected) in decoded.get_pixel(8, 8).0.into_iter().zip(expected) {
                assert!(
                    actual.abs_diff(expected) <= tolerance,
                    "{mime}: {actual} != {expected}"
                );
            }
        }
    }
}
