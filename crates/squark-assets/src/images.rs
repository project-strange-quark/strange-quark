// SPDX-License-Identifier: MIT
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine as _;
use percent_encoding::percent_decode_str;
use squark_types::render::{Bitmap, BitmapFormat, ImageKey};
use url::Url;

use crate::Pack;

pub type BlobResolver = dyn Fn(&str) -> Option<(Arc<[u8]>, String)> + Send + Sync;

#[derive(Clone, Copy)]
pub struct Resources<'a> {
    pub pack: Option<&'a Pack>,
    pub document_url: &'a Url,
    pub blob_resolver: Option<&'a BlobResolver>,
}

impl std::fmt::Debug for Resources<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resources")
            .field("document_url", &self.document_url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct Resource<'a> {
    pub mime: Cow<'a, str>,
    pub bytes: Cow<'a, [u8]>,
}

impl<'a> Resources<'a> {
    pub fn resolve(self, reference: &str) -> Option<Url> {
        self.document_url.join(reference.trim()).ok()
    }

    pub fn resolve_blob(self, url: &Url) -> Option<(Arc<[u8]>, String)> {
        self.blob_resolver?(url.as_str())
    }

    pub fn get(self, url: &Url) -> Option<Resource<'a>> {
        if url.scheme() == "blob" {
            let (bytes, mime) = self.resolve_blob(url)?;
            return Some(Resource::from_blob(&bytes, mime));
        }
        if url.scheme() == "data" {
            let data = url.as_str().strip_prefix("data:")?.split('#').next()?;
            let (header, body) = data.split_once(',')?;
            let encoded = header.rsplit(';').next()?.eq_ignore_ascii_case("base64");
            let mime = header.split(';').next()?;
            let unescaped: Vec<_> = percent_decode_str(body).collect();
            let bytes = if encoded {
                let compact: Vec<_> = unescaped
                    .into_iter()
                    .filter(|&b| !matches!(b, b' ' | b'\r' | b'\n' | b'\t' | 0x0c))
                    .collect();
                let engine = base64::engine::GeneralPurpose::new(
                    &base64::alphabet::STANDARD,
                    base64::engine::GeneralPurposeConfig::new()
                        .with_decode_allow_trailing_bits(true)
                        .with_decode_padding_mode(base64::engine::DecodePaddingMode::Indifferent),
                );
                engine.decode(compact).ok()?
            } else {
                unescaped
            };
            return Some(Resource {
                mime: Cow::Owned(mime.to_ascii_lowercase()),
                bytes: Cow::Owned(bytes),
            });
        }
        if url.origin() != self.document_url.origin() || url.scheme() != self.document_url.scheme()
        {
            return None;
        }
        let path = percent_decode_str(url.path()).decode_utf8().ok()?;
        let asset = self.pack?.entry(&path)?;
        Some(Resource {
            mime: Cow::Borrowed(asset.mime),
            bytes: Cow::Borrowed(asset.bytes),
        })
    }
}

impl Resource<'_> {
    pub fn from_blob(bytes: &[u8], mime: String) -> Self {
        Self {
            mime: Cow::Owned(mime),
            bytes: Cow::Owned(bytes.to_vec()),
        }
    }

    /// Decode one static frame. The renderer receives top-down premultiplied sRGB.
    pub fn decode_image(&self) -> Option<Bitmap> {
        let ([width, height], pixels) = if is_svg(&self.mime) {
            let tree = resvg::usvg::Tree::from_data(&self.bytes, &svg_options()).ok()?;
            let size = tree.size().to_int_size();
            // Match the default image decoder's allocation limit.
            if u64::from(size.width()) * u64::from(size.height()) * 4 > 512 * 1024 * 1024 {
                return None;
            }
            let mut image = resvg::tiny_skia::Pixmap::new(size.width(), size.height())?;
            resvg::render(
                &tree,
                resvg::tiny_skia::Transform::identity(),
                &mut image.as_mut(),
            );
            ([size.width(), size.height()], image.take())
        } else {
            let image = image::ImageReader::new(Cursor::new(self.bytes.as_ref()))
                .with_guessed_format()
                .ok()?
                .decode()
                .ok()?
                .into_rgba8();
            let size = [image.width(), image.height()];
            let mut pixels = image.into_raw();
            premultiply_rgba(&mut pixels);
            (size, pixels)
        };
        Some(Bitmap {
            size: [width, height],
            stride: width.checked_mul(4)?,
            format: BitmapFormat::Rgba8SrgbPremultiplied,
            pixels: pixels.into(),
        })
    }
}

fn is_svg(mime: &str) -> bool {
    mime.split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("image/svg+xml"))
}

/// Normalize straight-alpha, eight-bit sRGB pixels for the render queue.
pub fn premultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        let alpha = u16::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = ((u16::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
}

/// Resolve SVG sources inside the pack or as data, without a filesystem fallback.
pub fn svg_options() -> resvg::usvg::Options<'static> {
    resvg::usvg::Options {
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: resvg::usvg::ImageHrefResolver::default_data_resolver(),
            resolve_string: Box::new(|_, _| None),
        },
        ..Default::default()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IntrinsicSize {
    pub width: Option<f32>,
    pub height: Option<f32>,
    pub ratio: Option<f32>,
}

impl IntrinsicSize {
    pub fn dimensions(width: f32, height: f32) -> Self {
        Self {
            width: Some(width),
            height: Some(height),
            ratio: (width > 0.0 && height > 0.0).then(|| width / height),
        }
    }

    /// Resolve missing natural dimensions against the default object size.
    pub fn default_object_size(self) -> [f32; 2] {
        let ratio = self.ratio.filter(|ratio| ratio.is_finite() && *ratio > 0.0);
        match (self.width, self.height, ratio) {
            (Some(width), Some(height), _) => [width, height],
            (Some(width), None, Some(ratio)) => [width, width / ratio],
            (None, Some(height), Some(ratio)) => [height * ratio, height],
            (None, None, Some(ratio)) if ratio > 2.0 => [300.0, 300.0 / ratio],
            (None, None, Some(ratio)) => [150.0 * ratio, 150.0],
            (width, height, _) => [width.unwrap_or(300.0), height.unwrap_or(150.0)],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Image {
    pub key: ImageKey,
    pub intrinsic: IntrinsicSize,
}

#[derive(Clone, Debug)]
pub struct DecodedImage {
    intrinsic: IntrinsicSize,
    natural_size: [u32; 2],
    pub bitmap: Bitmap,
}

impl DecodedImage {
    pub fn decode(resource: Resource<'_>) -> Option<Self> {
        let bitmap = resource.decode_image()?;
        let (intrinsic, natural_size) = if is_svg(&resource.mime) {
            let intrinsic = svg_metadata(&resource.bytes)?;
            let natural_size = intrinsic
                .default_object_size()
                .map(|size| size.round() as u32);
            (intrinsic, natural_size)
        } else {
            (
                IntrinsicSize::dimensions(bitmap.size[0] as f32, bitmap.size[1] as f32),
                bitmap.size,
            )
        };
        Some(Self {
            intrinsic,
            natural_size,
            bitmap,
        })
    }
}

fn svg_metadata(bytes: &[u8]) -> Option<IntrinsicSize> {
    let xml = roxmltree::Document::parse(std::str::from_utf8(bytes).ok()?).ok()?;
    let root = xml.root_element();
    if root.tag_name().name() != "svg"
        || root.tag_name().namespace() != Some("http://www.w3.org/2000/svg")
    {
        return None;
    }
    Some(svg_intrinsic(
        root.attribute("width"),
        root.attribute("height"),
        root.attribute("viewBox"),
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementImage {
    Pending,
    Finished(Option<[u32; 2]>),
}

/// Metadata is cached by resolved URL for the lifetime of a packed document.
#[derive(Debug, Default)]
pub struct ImageCatalog {
    images: HashMap<Url, Option<Image>>,
    urls: Vec<Url>,
    elements: HashMap<Url, ElementImage>,
    bitmaps: HashMap<ImageKey, Bitmap>,
}

impl ImageCatalog {
    pub fn find(&mut self, resources: Resources<'_>, url: &Url) -> Option<Image> {
        if let Some(image) = self.images.get(url) {
            return *image;
        }
        let image = resources.get(url).and_then(|resource| {
            let intrinsic = if is_svg(&resource.mime) {
                svg_metadata(&resource.bytes)?
            } else {
                let reader = image::ImageReader::new(Cursor::new(resource.bytes))
                    .with_guessed_format()
                    .ok()?;
                let (width, height) = reader.into_dimensions().ok()?;
                IntrinsicSize::dimensions(width as f32, height as f32)
            };
            self.urls.push(url.clone());
            Some(Image {
                key: ImageKey(self.urls.len() as u64),
                intrinsic,
            })
        });
        self.images.insert(url.clone(), image);
        image
    }

    pub fn url(&self, key: ImageKey) -> Option<&Url> {
        self.urls.get(usize::try_from(key.0.checked_sub(1)?).ok()?)
    }

    pub fn element_state(&self, url: &Url) -> Option<ElementImage> {
        self.elements.get(url).copied()
    }

    pub fn decoded_element_key(&self, url: &Url) -> Option<ImageKey> {
        if !matches!(
            self.element_state(url),
            Some(ElementImage::Finished(Some(_)))
        ) {
            return None;
        }
        self.images.get(url)?.map(|image| image.key)
    }

    pub fn begin_element(&mut self, url: Url) {
        self.elements.insert(url, ElementImage::Pending);
    }

    pub fn finish_element(&mut self, url: Url, image: Option<DecodedImage>) -> Option<[u32; 2]> {
        let size = image.map(|decoded| {
            let key = if let Some(Some(image)) = self.images.get(&url) {
                image.key
            } else {
                self.urls.push(url.clone());
                ImageKey(self.urls.len() as u64)
            };
            let size = decoded.natural_size;
            self.bitmaps.insert(key, decoded.bitmap);
            self.images.insert(
                url.clone(),
                Some(Image {
                    key,
                    intrinsic: decoded.intrinsic,
                }),
            );
            size
        });
        if size.is_none() {
            self.images.insert(url.clone(), None);
        }
        self.elements.insert(url, ElementImage::Finished(size));
        size
    }

    pub fn bitmap(&self, key: ImageKey) -> Option<&Bitmap> {
        self.bitmaps.get(&key)
    }

    pub fn did_upload(&mut self, key: ImageKey) {
        self.bitmaps.remove(&key);
    }
}

pub fn svg_intrinsic(
    width: Option<&str>,
    height: Option<&str>,
    view_box: Option<&str>,
) -> IntrinsicSize {
    fn length(value: &str) -> Option<f32> {
        let value = value.trim();
        let split = value
            .find(|c: char| !matches!(c, '0'..='9' | '+' | '-' | '.' | 'e' | 'E'))
            .unwrap_or(value.len());
        let (number, unit) = value.split_at(split);
        let pixels = match unit {
            "" | "px" => 1.0,
            "in" => 96.0,
            "cm" => 96.0 / 2.54,
            "mm" => 96.0 / 25.4,
            "q" | "Q" => 96.0 / 101.6,
            "pt" => 96.0 / 72.0,
            "pc" => 16.0,
            _ => return None,
        };
        let size = number.parse::<f32>().ok()? * pixels;
        (size.is_finite() && size >= 0.0).then_some(size)
    }
    let (width, height) = (width.and_then(length), height.and_then(length));
    let ratio = match (width, height) {
        (Some(w), Some(h)) if w > 0.0 && h > 0.0 => Some(w / h),
        _ => view_box.and_then(|view_box| {
            let values: Vec<_> = view_box
                .split(|c: char| c == ',' || c.is_ascii_whitespace())
                .filter(|part| !part.is_empty())
                .collect();
            let [x, y, width, height] = values.as_slice() else {
                return None;
            };
            let [x, y, w, h] = [
                x.parse::<f32>().ok()?,
                y.parse().ok()?,
                width.parse().ok()?,
                height.parse().ok()?,
            ];
            (x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0)
                .then(|| w / h)
        }),
    };
    IntrinsicSize {
        width,
        height,
        ratio,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_data_png_and_viewbox_only_svg() {
        let url = Url::parse("https://app.test/index.html").unwrap();
        let resources = Resources {
            pack: None,
            blob_resolver: None,
            document_url: &url,
        };
        let png = resources.resolve("data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aPIcAAAAASUVORK5CYII=").unwrap();
        let svg = resources.resolve("data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A//www.w3.org/2000/svg%22%20viewBox%3D%220%200%20120%2040%22/%3E").unwrap();
        let mut images = ImageCatalog::default();
        assert_eq!(
            images.find(resources, &png).unwrap().intrinsic,
            IntrinsicSize::dimensions(1.0, 1.0)
        );
        let svg_image = images.find(resources, &svg).unwrap();
        assert_eq!(
            svg_image.intrinsic,
            IntrinsicSize {
                width: None,
                height: None,
                ratio: Some(3.0)
            }
        );
        assert_eq!(images.find(resources, &svg), Some(svg_image));
        assert_eq!(images.url(svg_image.key), Some(&svg));
    }
}

#[cfg(test)]
mod bitmap_tests {
    use super::*;
    use image::ImageEncoder;

    #[test]
    fn decodes_first_png_frame_to_premultiplied_srgb() {
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&[128, 64, 0, 128], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        let image = Resource {
            mime: "image/png".into(),
            bytes: bytes.into(),
        }
        .decode_image()
        .unwrap();
        assert_eq!(image.size, [1, 1]);
        assert_eq!(image.stride, 4);
        assert_eq!(image.format, BitmapFormat::Rgba8SrgbPremultiplied);
        assert_eq!(image.pixels.as_ref(), &[64, 32, 0, 128]);
    }
}

#[cfg(test)]
#[test]
fn reports_the_natural_size_of_an_svg_with_only_a_view_box() {
    let mut catalog = ImageCatalog::default();
    let url = Url::parse("https://example.test/ratio.svg").unwrap();
    let image = DecodedImage::decode(Resource {
        mime: "image/svg+xml".into(),
        bytes: br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 120 40"/>"#
            .as_slice()
            .into(),
    });
    assert_eq!(catalog.finish_element(url, image), Some([300, 100]));
}
