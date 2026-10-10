//! Auto-reframe (layer L2): find the main subject of a shot, follow it, and plan a smooth crop.
//!
//! * [`detect`] — YOLOX (people, Apache-2.0) and YuNet (faces, MIT) on [`filmcraft_nn`]: letterbox
//!   to the network size, run, decode, non-maximum suppression.
//! * [`track`] — picks the subject among the detections of each sampled frame (the largest, most
//!   confident person or face at first, then the one closest to where the subject was) and gives
//!   the point the framing should centre on (the face when there is one, else the upper body).
//! * [`path`] — turns those sparse samples into the crop window's centre at every frame: a
//!   dead zone so the camera stays still while the subject only fidgets, then a smoothing spline
//!   (acceleration penalty, solved exactly) so the pan has no jerks, then the limits of the frame.
//!
//! The engine (`sequence.autoReframe`) decodes the frames, calls these, and writes the result as
//! `Motion` keyframes.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod detect;
pub mod path;
pub mod track;

/// A decoded picture, RGBA8 (alpha ignored), row-major.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    pub width: usize,
    pub height: usize,
    pub rgba: &'a [u8],
}

/// A detected box in picture pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Detection {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub score: f32,
}

impl Detection {
    pub fn width(&self) -> f32 {
        (self.x1 - self.x0).max(0.0)
    }
    pub fn height(&self) -> f32 {
        (self.y1 - self.y0).max(0.0)
    }
    pub fn area(&self) -> f32 {
        self.width() * self.height()
    }
    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) * 0.5, (self.y0 + self.y1) * 0.5)
    }
    pub fn iou(&self, o: &Detection) -> f32 {
        let iw = (self.x1.min(o.x1) - self.x0.max(o.x0)).max(0.0);
        let ih = (self.y1.min(o.y1) - self.y0.max(o.y0)).max(0.0);
        let inter = iw * ih;
        let union = self.area() + o.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReframeError(pub String);

impl std::fmt::Display for ReframeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReframeError {}

impl From<filmcraft_nn::NnError> for ReframeError {
    fn from(e: filmcraft_nn::NnError) -> Self {
        ReframeError(e.0)
    }
}

pub type Result<T> = std::result::Result<T, ReframeError>;
