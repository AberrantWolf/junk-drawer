//! Minimal geometry primitive for desk/card positions.
//! No egui dependency — jd-app adds From/Into conversions.

/// A 2-D point or offset in desk-space coordinates.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}
