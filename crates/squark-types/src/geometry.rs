// SPDX-License-Identifier: MIT
//! Geometry in CSS pixels. Device pixels appear only inside squark-gpu and
//! squark-host, which multiply by the window's scale factor.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

/// An axis-aligned rectangle: origin at the top-left, y grows downward.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub origin: Point,
    pub size: Size,
}

impl Rect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            origin: Point { x, y },
            size: Size { width, height },
        }
    }

    pub fn max_x(&self) -> f32 {
        self.origin.x + self.size.width
    }

    pub fn max_y(&self) -> f32 {
        self.origin.y + self.size.height
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.origin.x && p.x < self.max_x() && p.y >= self.origin.y && p.y < self.max_y()
    }

    /// The overlap of two rectangles, or `None` when they do not overlap.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x0 = self.origin.x.max(other.origin.x);
        let y0 = self.origin.y.max(other.origin.y);
        let x1 = self.max_x().min(other.max_x());
        let y1 = self.max_y().min(other.max_y());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intersection_and_containment() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, 5.0, 10.0, 10.0);
        assert_eq!(a.intersection(&b), Some(Rect::new(5.0, 5.0, 5.0, 5.0)));
        assert_eq!(a.intersection(&Rect::new(10.0, 0.0, 1.0, 1.0)), None);
        assert!(a.contains(Point { x: 0.0, y: 9.9 }));
        assert!(!a.contains(Point { x: 10.0, y: 0.0 }));
    }
}
