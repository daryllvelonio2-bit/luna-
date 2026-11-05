use serde::{Deserialize, Serialize};

/// A screen rectangle, in desktop coordinates.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether the rectangle has an area at all.
    pub fn has_area(&self) -> bool {
        self.width > 0 && self.height > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_without_area_is_not_one() {
        assert!(!Rect::new(0, 0, 0, 10).has_area());
        assert!(!Rect::new(0, 0, 10, 0).has_area());
        assert!(Rect::new(0, 0, 10, 10).has_area());
    }
}
