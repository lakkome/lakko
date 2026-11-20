// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! UniFFI surface for the camera background's not-per-frame rules
//! ([ADR-0089](../../../docs/adr/0089-camera-background.md), UX
//! `docs/ux/call-backgrounds.md`): the stored choice, the built-in
//! backgrounds, the blur strengths and how a picture fills the frame.
//!
//! The per-frame part (segmentation, health watch, frame gate) is the C ABI
//! `lakko_segment_*` in `lakko-sframe-native`, which the camera pipelines
//! call on their own threads. Mirror of
//! `crates/lakko-ffi-wasm/src/background.rs`.

use lakko_segment::gate::is_photo_id;
use lakko_segment::{
    blur_parameters, builtin_background, cover_source_rect, model_input_rect, BlurStrength, Choice,
    BUILTIN_IDS,
};

/// What a stored choice is.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundKind {
    /// The camera as it is.
    None,
    /// The room blurred.
    Blur,
    /// The room blurred so nothing can be told apart.
    StrongBlur,
    /// A built-in background; the id is in [`BackgroundChoice::id`].
    Builtin,
    /// One of the person's own photos; the id is in
    /// [`BackgroundChoice::id`].
    Photo,
}

/// A stored choice, read.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct BackgroundChoice {
    /// What it is.
    pub kind: BackgroundKind,
    /// The built-in or photo id; empty for the others.
    pub id: String,
}

/// Reads a stored choice. Empty or `none` is `None`; anything else that
/// cannot be read is `Blur`, so an unreadable choice still hides the room.
#[uniffi::export]
#[must_use]
pub fn background_choice_from_stored(stored: String) -> BackgroundChoice {
    match Choice::from_stored(&stored) {
        Choice::None => BackgroundChoice {
            kind: BackgroundKind::None,
            id: String::new(),
        },
        Choice::Blur => BackgroundChoice {
            kind: BackgroundKind::Blur,
            id: String::new(),
        },
        Choice::StrongBlur => BackgroundChoice {
            kind: BackgroundKind::StrongBlur,
            id: String::new(),
        },
        Choice::Builtin(id) => BackgroundChoice {
            kind: BackgroundKind::Builtin,
            id: id.to_owned(),
        },
        Choice::Photo(id) => BackgroundChoice {
            kind: BackgroundKind::Photo,
            id,
        },
    }
}

/// The text to store for `choice`. A built-in or photo id that is not valid
/// stores as `blur` (it would read back as `blur` anyway).
#[uniffi::export]
#[must_use]
pub fn background_choice_to_stored(choice: BackgroundChoice) -> String {
    let c = match choice.kind {
        BackgroundKind::None => Choice::None,
        BackgroundKind::Blur => Choice::Blur,
        BackgroundKind::StrongBlur => Choice::StrongBlur,
        BackgroundKind::Builtin => BUILTIN_IDS
            .iter()
            .find(|b| **b == choice.id)
            .map_or(Choice::Blur, |b| Choice::Builtin(b)),
        BackgroundKind::Photo => {
            if is_photo_id(&choice.id) {
                Choice::Photo(choice.id)
            } else {
                Choice::Blur
            }
        },
    };
    c.to_stored()
}

/// Whether `id` is a valid own-photo id (1 to 64 of `[A-Za-z0-9_-]`).
#[uniffi::export]
#[must_use]
pub fn background_is_photo_id(id: String) -> bool {
    is_photo_id(&id)
}

/// The ids of the six built-in backgrounds, in strip order.
#[uniffi::export]
#[must_use]
pub fn background_builtin_ids() -> Vec<String> {
    BUILTIN_IDS.iter().map(|s| (*s).to_owned()).collect()
}

/// Draws built-in background `id` at `width` x `height` as RGBA (alpha
/// 255). `None` for an unknown id or a side of 0 or above 4096.
#[uniffi::export]
#[must_use]
pub fn background_builtin_render(id: String, width: u32, height: u32) -> Option<Vec<u8>> {
    builtin_background(&id, width, height)
}

/// How the compositor blurs: scale down by `divisor`, Gaussian of `sigma`
/// scaled pixels with `radius` taps on each side.
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq)]
pub struct BackgroundBlur {
    /// Down-scale factor in each direction.
    pub divisor: u32,
    /// Standard deviation, in pixels of the scaled frame.
    pub sigma: f32,
    /// Taps on each side of the centre.
    pub radius: u32,
}

/// The blur for `Blur` (`strong` false) or `Strong blur` (`strong` true) on a
/// frame `frame_height` pixels tall.
#[uniffi::export]
#[must_use]
pub fn background_blur(strong: bool, frame_height: u32) -> BackgroundBlur {
    let p = blur_parameters(
        if strong {
            BlurStrength::Strong
        } else {
            BlurStrength::Light
        },
        frame_height,
    );
    BackgroundBlur {
        divisor: p.divisor,
        sigma: p.sigma,
        radius: p.radius,
    }
}

/// A rectangle of a source picture, in its pixels.
#[derive(uniffi::Record, Debug, Clone, Copy, PartialEq)]
pub struct BackgroundRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// The centred part of a `src_width` x `src_height` picture that fills a
/// `dst_width` x `dst_height` frame edge to edge.
#[uniffi::export]
#[must_use]
pub fn background_cover_rect(
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> BackgroundRect {
    let r = cover_source_rect(src_width, src_height, dst_width, dst_height);
    BackgroundRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

/// Where a whole camera frame of `frame_width` x `frame_height` (upright) is
/// drawn inside the 256 x 144 model input: shape kept, centred, bars where
/// the shapes differ. Read the mask back through the same rectangle. Every
/// client uses this, so a phone held upright is segmented alike everywhere.
#[uniffi::export]
#[must_use]
pub fn background_model_input_rect(frame_width: u32, frame_height: u32) -> BackgroundRect {
    let r = model_input_rect(frame_width, frame_height);
    BackgroundRect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_round_trip_and_fallbacks() {
        for s in ["none", "blur", "strong-blur", "builtin:dusk", "photo:abc"] {
            assert_eq!(
                background_choice_to_stored(background_choice_from_stored(s.into())),
                s
            );
        }
        assert_eq!(
            background_choice_from_stored("garbage".into()).kind,
            BackgroundKind::Blur
        );
        let bad = BackgroundChoice {
            kind: BackgroundKind::Builtin,
            id: "nope".into(),
        };
        assert_eq!(background_choice_to_stored(bad), "blur");
        assert_eq!(background_builtin_ids().len(), 6);
        assert!(background_builtin_render("night".into(), 8, 8).is_some());
        assert_eq!(background_blur(true, 720).radius, 20);
    }
}
