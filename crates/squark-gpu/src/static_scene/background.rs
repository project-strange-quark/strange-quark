// SPDX-License-Identifier: MIT

use squark_paint::{resolve_length, resolve_position};
use squark_types::{
    Rect, Size,
    fragment::{BackgroundLayer, ImageSize, Repeat},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Tile {
    pub rect: Rect,
    pub period: [f32; 2],
}

pub(super) fn tile(layer: &BackgroundLayer, origin: Rect, intrinsic: Option<Size>) -> Option<Tile> {
    let available = [origin.size.width, origin.size.height];
    let intrinsic_size = intrinsic.map(|size| [size.width, size.height]);
    let intrinsic_ratio = intrinsic_size.map(|[w, h]| w / h);
    let auto = match layer.size {
        ImageSize::Explicit(lengths) => lengths.map(|length| length.is_none()),
        _ => [false; 2],
    };
    let mut size = match layer.size {
        ImageSize::Contain | ImageSize::Cover => intrinsic_size.map_or(available, |[w, h]| {
            let [x, y] = [available[0] / w, available[1] / h];
            let scale = if layer.size == ImageSize::Contain {
                x.min(y)
            } else {
                x.max(y)
            };
            [w * scale, h * scale]
        }),
        ImageSize::Explicit(lengths) => {
            let hinted: [Option<f32>; 2] = std::array::from_fn(|i| {
                lengths[i].map(|length| resolve_length(length, available[i]))
            });
            match (hinted, intrinsic_ratio) {
                ([Some(w), None], Some(ratio)) => [w, w / ratio],
                ([None, Some(h)], Some(ratio)) => [h * ratio, h],
                _ => {
                    let fallback = intrinsic_size.unwrap_or(available);
                    std::array::from_fn(|i| hinted[i].unwrap_or(fallback[i]))
                }
            }
        }
    };
    if size
        .iter()
        .any(|&dimension| !dimension.is_finite() || dimension <= 0.0)
    {
        return None;
    }
    for i in 0..2 {
        if layer.repeat[i] == Repeat::Round {
            let count = (available[i] / size[i]).round().max(1.0);
            let dimension = available[i] / count;
            let other = 1 - i;
            if auto[other] && layer.repeat[other] != Repeat::Round && intrinsic_ratio.is_some() {
                size[other] *= dimension / size[i];
            }
            size[i] = dimension;
        }
    }
    if size.iter().any(|&dimension| dimension <= 0.0) {
        return None;
    }
    let position = resolve_position(
        layer.position,
        Size {
            width: available[0] - size[0],
            height: available[1] - size[1],
        },
    );
    let mut offset = [position.x, position.y];
    let mut period = [0.0; 2];
    for i in 0..2 {
        period[i] = match layer.repeat[i] {
            Repeat::Repeat | Repeat::Round => size[i],
            Repeat::Space if available[i] >= 2.0 * size[i] => {
                let count = (available[i] / size[i]).floor();
                offset[i] = 0.0;
                (available[i] - size[i]) / (count - 1.0)
            }
            _ => 0.0,
        };
    }
    Some(Tile {
        rect: Rect::new(
            origin.origin.x + offset[0],
            origin.origin.y + offset[1],
            size[0],
            size[1],
        ),
        period,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use squark_types::{
        fragment::{BackgroundClip, BackgroundImage, GeometryBox, LengthPercentage},
        render::ImageKey,
    };

    #[test]
    fn space_repeat_positions_end_tiles_at_both_edges() {
        let layer = BackgroundLayer {
            image: BackgroundImage::Image(ImageKey(1)),
            origin: GeometryBox::Border,
            clip: BackgroundClip::Border,
            size: ImageSize::Explicit([
                Some(LengthPercentage {
                    px: 10.0,
                    fraction: 0.0,
                }),
                None,
            ]),
            position: Default::default(),
            repeat: [Repeat::Space, Repeat::NoRepeat],
        };
        let tile = tile(
            &layer,
            Rect::new(2.0, 3.0, 34.0, 40.0),
            Some(Size {
                width: 20.0,
                height: 10.0,
            }),
        )
        .unwrap();
        assert_eq!(tile.rect, Rect::new(2.0, 3.0, 10.0, 5.0));
        assert_eq!(tile.period, [12.0, 0.0]);
    }
}
