// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Camera background for calls
//! ([ADR-0089](../../../docs/adr/0089-camera-background.md), UX
//! [`docs/ux/call-backgrounds.md`](../../../docs/ux/call-backgrounds.md)).
//!
//! Everything about the feature that is not pixels on a GPU, written once for
//! every client:
//!
//! - [`Segmenter`]: runs the bundled MediaPipe selfie segmentation model
//!   (landscape, [`INPUT_WIDTH`] x [`INPUT_HEIGHT`]) on a small picture of the
//!   camera frame and returns a smoothed person mask of the same size;
//! - [`mask`]: the smoothing across frames and the ramp that turns a
//!   probability into an alpha value (usable without the model);
//! - [`gate`]: the stored choice, the processor's health and **the frame
//!   gate**, the one rule for what may leave the device: while a background
//!   is selected, a frame it was not applied to is never sent;
//! - [`render`]: the blur strengths, how a picture fills the frame, and the six
//!   built-in backgrounds, drawn by code so every client shows the same
//!   pictures without shipping image files.
//!
//! The compositing itself (the blur and the blend of a 1280x720 frame) stays
//! on each platform's GPU; this crate hands it its inputs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod gate;
pub mod mask;
#[cfg(feature = "model")]
mod model;
pub mod render;

pub use gate::{frame_action, Choice, FrameAction, HealthWatch, ProcessorState, BUILTIN_IDS};
pub use mask::{MaskSmoother, ALPHA_HIGH, ALPHA_LOW, SMOOTHING};
#[cfg(feature = "model")]
pub use model::{PixelLayout, SegmentError, Segmenter, MODEL_SHA256};
pub use render::{
    blur_parameters, builtin_background, cover_source_rect, model_input_rect, BlurParameters,
    BlurStrength, SourceRect,
};

/// Width, in pixels, of the picture [`Segmenter::segment`] takes and of the
/// mask it returns (the landscape model's input).
pub const INPUT_WIDTH: usize = 256;

/// Height, in pixels, of the model's input picture and of the mask.
pub const INPUT_HEIGHT: usize = 144;

/// Pixels in one model input picture and one mask.
pub const INPUT_PIXELS: usize = INPUT_WIDTH * INPUT_HEIGHT;
