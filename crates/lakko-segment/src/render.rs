// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! What the compositors draw, decided once for every client: the two blur
//! strengths, how a picture fills the frame, and the six built-in backgrounds
//! (UX `call-backgrounds.md` section 5), drawn by code here so the clients
//! ship no image files and show the same pictures.

use crate::gate::BUILTIN_IDS;

/// The two blur choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlurStrength {
    /// `Blur`: keeps the room's colours, hides its details.
    Light,
    /// `Strong blur`: nothing in the room can be told apart.
    Strong,
}

/// How a compositor blurs the frame: scale it down by `divisor` in each
/// direction, run a separable Gaussian of `sigma` (in pixels of the scaled
/// frame) with `radius` taps on each side, and scale back up when blending.
/// Blurring at a quarter size keeps the strong blur affordable on a phone's
/// GPU.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlurParameters {
    /// Down-scale factor, in each direction.
    pub divisor: u32,
    /// Gaussian standard deviation, in pixels of the scaled frame.
    pub sigma: f32,
    /// Taps on each side of the centre (`ceil(3 * sigma)`, at least 1).
    pub radius: u32,
}

/// Blur standard deviation as a share of the frame height, before
/// down-scaling. Chosen, not measured: about 9 px and 26 px on a 720p frame.
const LIGHT_SIGMA_PER_HEIGHT: f32 = 0.012;
const STRONG_SIGMA_PER_HEIGHT: f32 = 0.036;
const BLUR_DIVISOR: u32 = 4;
const MAX_FRAME_SIDE: u32 = 4096;

/// The blur for `strength` on a frame `frame_height` pixels tall.
#[must_use]
pub fn blur_parameters(strength: BlurStrength, frame_height: u32) -> BlurParameters {
    let per_height = match strength {
        BlurStrength::Light => LIGHT_SIGMA_PER_HEIGHT,
        BlurStrength::Strong => STRONG_SIGMA_PER_HEIGHT,
    };
    #[allow(clippy::cast_precision_loss)]
    let height = frame_height.clamp(1, MAX_FRAME_SIDE) as f32;
    #[allow(clippy::cast_precision_loss)]
    let sigma = (height * per_height / BLUR_DIVISOR as f32).max(0.5);
    // sigma <= 4096 * 0.036 / 4 < 37, so 3 * sigma fits a u32 easily.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let radius = ((3.0 * sigma).ceil() as u32).max(1);
    BlurParameters {
        divisor: BLUR_DIVISOR,
        sigma,
        radius,
    }
}

/// A rectangle of a source picture, in its pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceRect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// The part of a `src_width` x `src_height` picture that fills a
/// `dst_width` x `dst_height` frame edge to edge: the largest centred
/// rectangle of the frame's shape (UX section 2.3: cropped from the centre).
/// Zero sizes read as 1.
#[must_use]
pub fn cover_source_rect(
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> SourceRect {
    #[allow(clippy::cast_precision_loss)]
    let [sw, sh, dw, dh] = [src_width, src_height, dst_width, dst_height].map(|v| v.max(1) as f32);
    let (width, height) = if sw * dh > dw * sh {
        // The source is wider than the frame: keep its height.
        (sh * dw / dh, sh)
    } else {
        (sw, sw * dh / dw)
    };
    SourceRect {
        x: (sw - width) / 2.0,
        y: (sh - height) / 2.0,
        width,
        height,
    }
}

/// Where a whole `frame_width` x `frame_height` camera frame (upright, as the
/// person sees it) is drawn inside the model's input picture: as large as fits,
/// shape kept, centred, the rest left black. A frame of another shape is never
/// stretched (the model was trained on undistorted pictures), so a phone held
/// upright gets bars at the sides. The mask is read back through the same
/// rectangle. In pixels of the model input ([`crate::INPUT_WIDTH`] x
/// [`crate::INPUT_HEIGHT`]). Zero sizes read as 1.
#[must_use]
pub fn model_input_rect(frame_width: u32, frame_height: u32) -> SourceRect {
    #[allow(clippy::cast_precision_loss)]
    let [fw, fh] = [frame_width, frame_height].map(|v| v.max(1) as f32);
    #[allow(clippy::cast_precision_loss)]
    let [mw, mh] = [crate::INPUT_WIDTH as f32, crate::INPUT_HEIGHT as f32];
    let scale = (mw / fw).min(mh / fh);
    let (width, height) = (fw * scale, fh * scale);
    SourceRect {
        x: (mw - width) / 2.0,
        y: (mh - height) / 2.0,
        width,
        height,
    }
}

type Rgb = [f32; 3];

struct Blob {
    x: f32,
    y: f32,
    radius: f32,
    colour: Rgb,
    strength: f32,
}

struct Design {
    top: Rgb,
    bottom: Rgb,
    blobs: [Blob; 3],
}

const fn hex(v: u32) -> Rgb {
    #[allow(clippy::cast_precision_loss)]
    {
        [
            ((v >> 16) & 0xff) as f32,
            ((v >> 8) & 0xff) as f32,
            (v & 0xff) as f32,
        ]
    }
}

const fn blob(x: f32, y: f32, radius: f32, colour: u32, strength: f32) -> Blob {
    Blob {
        x,
        y,
        radius,
        colour: hex(colour),
        strength,
    }
}

/// The six designs, in [`BUILTIN_IDS`] order, from the design system's
/// palette (indigo, teal, slate, sand; no violet): two light, two mid, two
/// dark.
fn design(id: &str) -> Option<Design> {
    Some(match id {
        "dawn" => Design {
            top: hex(0x00f6_efe3),
            bottom: hex(0x00e0_e7ff),
            blobs: [
                blob(0.18, 0.30, 0.55, 0x00fd_e2c8, 0.8),
                blob(0.82, 0.72, 0.60, 0x00c7_d2fe, 0.7),
                blob(0.55, 0.12, 0.35, 0x00ff_ffff, 0.6),
            ],
        },
        "morning" => Design {
            top: hex(0x00ee_f2ff),
            bottom: hex(0x00e0_f2f6),
            blobs: [
                blob(0.78, 0.25, 0.55, 0x00c7_d2fe, 0.7),
                blob(0.20, 0.78, 0.60, 0x00bf_e7ef, 0.8),
                blob(0.42, 0.40, 0.30, 0x00ff_ffff, 0.5),
            ],
        },
        "meadow" => Design {
            top: hex(0x005e_a8b5),
            bottom: hex(0x002f_7f8e),
            blobs: [
                blob(0.25, 0.30, 0.55, 0x008f_d3dd, 0.6),
                blob(0.82, 0.82, 0.65, 0x001d_5f6d, 0.7),
                blob(0.70, 0.18, 0.35, 0x006f_d0e0, 0.4),
            ],
        },
        "harbor" => Design {
            top: hex(0x0081_8cf8),
            bottom: hex(0x0043_38ca),
            blobs: [
                blob(0.20, 0.20, 0.50, 0x00a5_b4fc, 0.6),
                blob(0.85, 0.85, 0.65, 0x0031_2e81, 0.7),
                blob(0.75, 0.30, 0.40, 0x006f_d0e0, 0.3),
            ],
        },
        "dusk" => Design {
            top: hex(0x0026_295a),
            bottom: hex(0x001e_1b4b),
            blobs: [
                blob(0.20, 0.30, 0.55, 0x0037_30a3, 0.45),
                blob(0.80, 0.78, 0.60, 0x008a_5a17, 0.5),
                blob(0.88, 0.82, 0.30, 0x00e5_b96b, 0.25),
            ],
        },
        "night" => Design {
            top: hex(0x0017_1b2e),
            bottom: hex(0x000f_1220),
            blobs: [
                blob(0.30, 0.35, 0.55, 0x0026_295a, 0.8),
                blob(0.75, 0.70, 0.60, 0x0012_313a, 0.8),
                blob(0.60, 0.20, 0.35, 0x0037_30a3, 0.3),
            ],
        },
        _ => return None,
    })
}

/// A small integer hash for the dither, so the picture is the same on every
/// run and every client.
const fn hash(x: u32, y: u32) -> u32 {
    let mut h = x.wrapping_mul(0x9e37_79b1) ^ y.wrapping_mul(0x85eb_ca77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h
}

/// Draws the built-in background `id` at `width` x `height` as RGBA, rows top
/// to bottom, alpha 255. `None` for an unknown id or a side of 0 or above
/// 4096. Soft shapes over a vertical gradient, with a fixed dither so the
/// gradients do not band.
#[must_use]
pub fn builtin_background(id: &str, width: u32, height: u32) -> Option<Vec<u8>> {
    if !BUILTIN_IDS.contains(&id) || width == 0 || height == 0 {
        return None;
    }
    if width > MAX_FRAME_SIDE || height > MAX_FRAME_SIDE {
        return None;
    }
    let look = design(id)?;
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    #[allow(clippy::cast_precision_loss)]
    let (frame_w, frame_h) = (width as f32, height as f32);
    for py in 0..height {
        #[allow(clippy::cast_precision_loss)]
        let row = (py as f32 + 0.5) / frame_h;
        for px in 0..width {
            #[allow(clippy::cast_precision_loss)]
            let col = (px as f32 + 0.5) / frame_w;
            let mut colour: Rgb = [0.0; 3];
            for (k, ck) in colour.iter_mut().enumerate() {
                *ck = (look.bottom[k] - look.top[k]).mul_add(row, look.top[k]);
            }
            for shape in &look.blobs {
                // Distances in units of the height, so a blob stays round on
                // any frame shape.
                let dx = (col - shape.x) * frame_w / frame_h;
                let dy = row - shape.y;
                let spread = shape.radius * shape.radius / 2.0;
                let weight = shape.strength * (-(dx * dx + dy * dy) / spread).exp();
                for (k, ck) in colour.iter_mut().enumerate() {
                    *ck = (shape.colour[k] - *ck).mul_add(weight, *ck);
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let dither = (hash(px, py) & 0xff) as f32 / 255.0 - 0.5;
            for ck in colour {
                // Clamped to [0, 255] before the cast.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                out.push((ck + dither).round().clamp(0.0, 255.0) as u8);
            }
            out.push(255);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn blur_at_720p() {
        let light = blur_parameters(BlurStrength::Light, 720);
        let strong = blur_parameters(BlurStrength::Strong, 720);
        assert_eq!(light.divisor, 4);
        assert!((light.sigma - 2.16).abs() < 1e-3);
        assert_eq!(light.radius, 7);
        assert!((strong.sigma - 6.48).abs() < 1e-3);
        assert_eq!(strong.radius, 20);
        assert!(blur_parameters(BlurStrength::Light, 0).radius >= 1);
    }

    #[test]
    fn cover_crops_the_centre() {
        // 4:3 photo into a 16:9 frame: full width, centre band.
        let r = cover_source_rect(1600, 1200, 1280, 720);
        assert_eq!((r.x, r.width), (0.0, 1600.0));
        assert!((r.height - 900.0).abs() < 1e-3);
        assert!((r.y - 150.0).abs() < 1e-3);
        // Panorama into 16:9: full height, centre slice.
        let r = cover_source_rect(4000, 1000, 1280, 720);
        assert_eq!((r.y, r.height), (0.0, 1000.0));
        assert!((r.width - 1777.778).abs() < 1e-2);
    }

    #[test]
    fn every_builtin_draws_and_unknown_does_not() {
        for id in BUILTIN_IDS {
            let px = builtin_background(id, 64, 36).expect(id);
            assert_eq!(px.len(), 64 * 36 * 4);
            assert!(px.chunks_exact(4).all(|p| p[3] == 255));
        }
        assert!(builtin_background("sunset", 64, 36).is_none());
        assert!(builtin_background("dawn", 0, 36).is_none());
        assert!(builtin_background("dawn", 4097, 36).is_none());
    }

    #[test]
    fn light_ones_are_light_and_dark_ones_dark() {
        let mean = |id: &str| {
            let px = builtin_background(id, 64, 36).expect(id);
            let s: u64 = px
                .chunks_exact(4)
                .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
                .sum();
            s / (64 * 36 * 3)
        };
        for id in ["dawn", "morning"] {
            assert!(mean(id) > 180, "{id} {}", mean(id));
        }
        for id in ["meadow", "harbor"] {
            let m = mean(id);
            assert!((60..=180).contains(&m), "{id} {m}");
        }
        for id in ["dusk", "night"] {
            assert!(mean(id) < 60, "{id} {}", mean(id));
        }
    }

    #[test]
    fn the_same_picture_every_time() {
        assert_eq!(
            builtin_background("harbor", 40, 30),
            builtin_background("harbor", 40, 30)
        );
    }

    #[test]
    fn model_input_keeps_the_frame_shape() {
        let wide = model_input_rect(1280, 720);
        assert_eq!(
            (wide.x, wide.y, wide.width, wide.height),
            (0.0, 0.0, 256.0, 144.0)
        );
        let upright = model_input_rect(720, 1280);
        assert!((upright.height - 144.0).abs() < 1e-3);
        assert!((upright.width - 81.0).abs() < 1e-3);
        assert!((upright.x - 87.5).abs() < 1e-3);
        let square = model_input_rect(640, 640);
        assert!((square.width - 144.0).abs() < 1e-3 && (square.x - 56.0).abs() < 1e-3);
    }

    proptest! {
        #[test]
        fn cover_stays_inside_and_keeps_the_frame_shape(
            sw in 1u32..5000, sh in 1u32..5000, dw in 1u32..5000, dh in 1u32..5000
        ) {
            let r = cover_source_rect(sw, sh, dw, dh);
            #[allow(clippy::cast_precision_loss)]
            let (sw, sh, dw, dh) = (sw as f32, sh as f32, dw as f32, dh as f32);
            prop_assert!(r.x >= -1e-3 && r.y >= -1e-3);
            prop_assert!(r.x + r.width <= sw * (1.0 + 1e-5) + 1e-3);
            prop_assert!(r.y + r.height <= sh * (1.0 + 1e-5) + 1e-3);
            prop_assert!((r.width / r.height - dw / dh).abs() <= 1e-3 * (dw / dh).max(1.0));
        }
    }
}
