// SPDX-License-Identifier: MIT

use std::borrow::Cow;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{
    ExtendedColorType, ImageEncoder, RgbaImage, codecs::png::PngEncoder, imageops,
    imageops::FilterType,
};
use serde::Deserialize;
use v8::{Function, FunctionCallbackArguments, Local, Object, PinScope, ReturnValue};

use crate::{
    bindings::{set, string},
    network::bytes,
};

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Quality {
    Pixelated,
    Low,
    Medium,
    High,
}

impl From<Quality> for FilterType {
    fn from(value: Quality) -> Self {
        match value {
            Quality::Pixelated => Self::Nearest,
            Quality::Low => Self::Triangle,
            Quality::Medium => Self::CatmullRom,
            Quality::High => Self::Lanczos3,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    mime: String,
    size: Option<[u32; 2]>,
    data_url: Option<String>,
    crop: Option<[i32; 4]>,
    resize: [Option<u32>; 2],
    quality: Quality,
    flip_y: bool,
}

pub(crate) fn install(scope: &mut PinScope<'_, '_>, host: Local<'_, Object>) -> Option<()> {
    let function = Function::new(scope, prepare)?;
    set(scope, host, "prepareImageBitmap", function.into())
}

fn prepare(
    scope: &mut PinScope<'_, '_>,
    args: FunctionCallbackArguments<'_>,
    mut result: ReturnValue<'_>,
) {
    let url = (|| {
        let request: Request = serde_json::from_str(&string(scope, args.get(1))?).ok()?;
        let pixels = bytes(args.get(0))?;
        let png = encode(request, pixels)?;
        v8::String::new(
            scope,
            &format!("data:image/png;base64,{}", STANDARD.encode(&png)),
        )
    })();
    result.set(
        url.map(v8::Local::<v8::Value>::from)
            .unwrap_or_else(|| v8::null(scope).into()),
    );
}

fn pixel_len(width: u32, height: u32) -> Option<usize> {
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(4)?;
    (bytes != 0 && bytes <= 512 * 1024 * 1024).then_some(bytes as usize)
}

fn encode(request: Request, mut pixels: Vec<u8>) -> Option<Vec<u8>> {
    // Work with premultiplied colors so resampling transparent edges does
    // not pull color from fully transparent pixels.
    let mut source = if let Some([width, height]) = request.size {
        if pixels.len() != pixel_len(width, height)? {
            return None;
        }
        squark_assets::premultiply_rgba(&mut pixels);
        RgbaImage::from_raw(width, height, pixels)?
    } else {
        let bitmap = if let Some(address) = request.data_url {
            let url = url::Url::parse(&address).ok()?;
            squark_assets::Resources {
                pack: None,
                document_url: &url,
                blob_resolver: None,
            }
            .get(&url)?
            .decode_image()?
        } else {
            squark_assets::Resource {
                mime: Cow::Owned(request.mime),
                bytes: Cow::Owned(pixels),
            }
            .decode_image()?
        };
        RgbaImage::from_raw(bitmap.size[0], bitmap.size[1], bitmap.pixels.to_vec())?
    };
    if let Some(rect) = request.crop {
        source = crop(source, rect)?;
    }
    if request.flip_y {
        imageops::flip_vertical_in_place(&mut source);
    }
    let (width, height) = source.dimensions();
    let from_ratio = |target: u32, numerator: u32, denominator: u32| {
        u32::try_from((u64::from(target) * u64::from(numerator)).div_ceil(u64::from(denominator)))
            .ok()
    };
    let [width, height] = match request.resize {
        [None, None] => [width, height],
        [Some(w), Some(h)] => [w, h],
        [Some(w), None] => [w, from_ratio(w, height, width)?],
        [None, Some(h)] => [from_ratio(h, width, height)?, h],
    };
    pixel_len(width, height)?;
    if source.dimensions() != (width, height) {
        source = imageops::resize(&source, width, height, request.quality.into());
    }
    let mut rgba = source.into_raw();
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            let straight = (u16::from(*channel) * 255 + alpha / 2)
                .checked_div(alpha)
                .unwrap_or(0);
            *channel = straight.min(255) as u8;
        }
    }
    let mut png = Vec::new();
    PngEncoder::new(&mut png)
        .write_image(&rgba, width, height, ExtendedColorType::Rgba8)
        .ok()?;
    Some(png)
}

fn crop(source: RgbaImage, [x, y, width, height]: [i32; 4]) -> Option<RgbaImage> {
    let left = i64::from(x) + i64::from(width.min(0));
    let top = i64::from(y) + i64::from(height.min(0));
    let (width, height) = (width.unsigned_abs(), height.unsigned_abs());
    if left == 0 && top == 0 && source.dimensions() == (width, height) {
        return Some(source);
    }
    let mut pixels = vec![0; pixel_len(width, height)?];
    let start_x = left.max(0);
    let start_y = top.max(0);
    let end_x = (left + i64::from(width)).min(i64::from(source.width()));
    let end_y = (top + i64::from(height)).min(i64::from(source.height()));
    if start_x < end_x {
        let span = (end_x - start_x) as usize * 4;
        let stride = source.width() as usize * 4;
        for row in start_y..end_y {
            let from = row as usize * stride + start_x as usize * 4;
            let to = (((row - top) as usize * width as usize) + (start_x - left) as usize) * 4;
            pixels[to..to + span].copy_from_slice(&source.as_raw()[from..from + span]);
        }
    }
    RgbaImage::from_raw(width, height, pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshots_crop_with_transparent_padding_and_flip_before_resizing() {
        let request = Request {
            mime: String::new(),
            size: Some([1, 2]),
            data_url: None,
            crop: Some([1, 2, -2, -2]),
            resize: [Some(4), None],
            quality: Quality::Pixelated,
            flip_y: true,
        };
        let png = encode(request, vec![255, 0, 0, 255, 0, 127, 0, 128]).unwrap();
        let snapshot = image::load_from_memory(&png).unwrap().into_rgba8();
        assert_eq!(snapshot.dimensions(), (4, 4));
        assert_eq!(snapshot.get_pixel(0, 0).0, [0, 0, 0, 0]);
        assert_eq!(snapshot.get_pixel(3, 0).0, [0, 128, 0, 128]);
        assert_eq!(snapshot.get_pixel(3, 3).0, [255, 0, 0, 255]);
    }
}
