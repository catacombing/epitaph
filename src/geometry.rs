//! Shared geometry types.

use std::ops::{AddAssign, Mul, Sub};

/// 2D object position.
#[derive(PartialEq, Eq, Copy, Clone, Default, Debug)]
pub struct Position<T = i32> {
    pub x: T,
    pub y: T,
}

impl<T> From<(T, T)> for Position<T> {
    fn from((x, y): (T, T)) -> Self {
        Self { x, y }
    }
}

impl From<Position<f64>> for Position<f32> {
    fn from(position: Position<f64>) -> Self {
        Self { x: position.x as f32, y: position.y as f32 }
    }
}

impl From<Position> for Position<f64> {
    fn from(position: Position) -> Self {
        Self { x: position.x as f64, y: position.y as f64 }
    }
}

impl From<Position<u32>> for Position<f32> {
    fn from(position: Position<u32>) -> Self {
        Self { x: position.x as f32, y: position.y as f32 }
    }
}

impl From<Position<u32>> for Position<f64> {
    fn from(position: Position<u32>) -> Self {
        Self { x: position.x as f64, y: position.y as f64 }
    }
}

impl<T: Sub<T, Output = T>> Sub<Position<T>> for Position<T> {
    type Output = Self;

    fn sub(mut self, rhs: Position<T>) -> Self {
        self.x = self.x - rhs.x;
        self.y = self.y - rhs.y;
        self
    }
}

impl AddAssign<Position<f64>> for Position<i32> {
    fn add_assign(&mut self, rhs: Position<f64>) {
        self.x += rhs.x.round() as i32;
        self.y += rhs.y.round() as i32;
    }
}

impl Mul<f64> for Position<f64> {
    type Output = Self;

    fn mul(mut self, scale: f64) -> Self {
        self.x *= scale;
        self.y *= scale;
        self
    }
}

/// 2D object size.
#[derive(PartialEq, Eq, Copy, Clone, Default, Debug)]
pub struct Size<T = u32> {
    pub width: T,
    pub height: T,
}

impl<T> Size<T> {
    pub fn new(width: T, height: T) -> Self {
        Self { width, height }
    }
}

impl<T> From<(T, T)> for Size<T> {
    fn from((width, height): (T, T)) -> Self {
        Self { width, height }
    }
}

impl From<Size> for Size<i32> {
    fn from(size: Size) -> Self {
        Self { width: size.width as i32, height: size.height as i32 }
    }
}

impl From<Size> for Size<f32> {
    fn from(size: Size) -> Self {
        Self { width: size.width as f32, height: size.height as f32 }
    }
}

impl From<Size> for Size<f64> {
    fn from(size: Size) -> Self {
        Self { width: size.width as f64, height: size.height as f64 }
    }
}

impl Mul<f64> for Size {
    type Output = Self;

    fn mul(mut self, scale: f64) -> Self {
        self.width = (self.width as f64 * scale).round() as u32;
        self.height = (self.height as f64 * scale).round() as u32;
        self
    }
}
