// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! wasm-bindgen surface for the camera background's not-per-frame rules
//! ([ADR-0089](../../../docs/adr/0089-camera-background.md)), for the web
//! client's main thread (the settings strip, thumbnails, the stored choice).
//!
//! The per-frame part runs in the background worker on its own import-free
//! module, `lakko-segment-wasm`. Mirror of
//! `crates/lakko-ffi-uniffi/src/background.rs`. A choice crosses as its
//! stored text (`none`, `blur`, `strong-blur`, `builtin:<id>`,
//! `photo:<id>`).

use lakko_segment::gate::is_photo_id;
use lakko_segment::{
    blur_parameters, builtin_background, cover_source_rect, model_input_rect, BlurStrength, Choice,
    BUILTIN_IDS,
};
use wasm_bindgen::prelude::*;

/// The stored text a stored value reads as: empty or `none` is `"none"`;
/// anything unreadable is `"blur"`, so it still hides the room.
#[wasm_bindgen(js_name = backgroundChoiceNormalize)]
#[must_use]
pub fn background_choice_normalize(stored: &str) -> String {
    Choice::from_stored(stored).to_stored()
}

/// Whether the stored value selects a background (anything but none).
#[wasm_bindgen(js_name = backgroundChoiceIsBackground)]
#[must_use]
pub fn background_choice_is_background(stored: &str) -> bool {
    Choice::from_stored(stored).is_background()
}

/// Whether `id` is a valid own-photo id (1 to 64 of `[A-Za-z0-9_-]`).
#[wasm_bindgen(js_name = backgroundIsPhotoId)]
#[must_use]
pub fn background_is_photo_id(id: &str) -> bool {
    is_photo_id(id)
}

/// The ids of the six built-in backgrounds, in strip order.
#[wasm_bindgen(js_name = backgroundBuiltinIds)]
#[must_use]
pub fn background_builtin_ids() -> Vec<String> {
    BUILTIN_IDS.iter().map(|s| (*s).to_owned()).collect()
}

/// Draws built-in background `id` at `width` x `height` as RGBA, or
/// `undefined` for an unknown id or a side of 0 or above 4096.
#[wasm_bindgen(js_name = backgroundBuiltinRender)]
#[must_use]
pub fn background_builtin_render(id: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    builtin_background(id, width, height)
}

/// `[divisor, sigma, radius]` of the blur for `Blur` (`strong` false) or
/// `Strong blur` (`strong` true) on a frame `frame_height` tall.
#[wasm_bindgen(js_name = backgroundBlur)]
#[must_use]
pub fn background_blur(strong: bool, frame_height: u32) -> Vec<f32> {
    let p = blur_parameters(
        if strong {
            BlurStrength::Strong
        } else {
            BlurStrength::Light
        },
        frame_height,
    );
    #[allow(clippy::cast_precision_loss)]
    let v = vec![p.divisor as f32, p.sigma, p.radius as f32];
    v
}

/// `[x, y, width, height]` of the centred part of a picture that fills a
/// frame edge to edge.
#[wasm_bindgen(js_name = backgroundCoverRect)]
#[must_use]
pub fn background_cover_rect(
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Vec<f32> {
    let r = cover_source_rect(src_width, src_height, dst_width, dst_height);
    vec![r.x, r.y, r.width, r.height]
}

/// `[x, y, width, height]` in the 256 x 144 model input where a whole upright
/// camera frame is drawn: shape kept, centred, bars where the shapes differ.
#[wasm_bindgen(js_name = backgroundModelInputRect)]
#[must_use]
pub fn background_model_input_rect(frame_width: u32, frame_height: u32) -> Vec<f32> {
    let r = model_input_rect(frame_width, frame_height);
    vec![r.x, r.y, r.width, r.height]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_and_render() {
        assert_eq!(background_choice_normalize(""), "none");
        assert_eq!(background_choice_normalize("photo:../etc"), "blur");
        assert!(!background_choice_is_background("none"));
        assert!(background_choice_is_background("builtin:dawn"));
        assert_eq!(background_builtin_ids().len(), 6);
        assert_eq!(
            background_builtin_render("dawn", 4, 2).map(|v| v.len()),
            Some(32)
        );
        assert!((background_blur(false, 720)[2] - 7.0).abs() < f32::EPSILON);
    }
}
