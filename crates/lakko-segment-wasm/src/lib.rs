// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Raw WebAssembly exports of [`lakko_segment`] for the web client's camera
//! background worker
//! ([ADR-0089](../../../docs/adr/0089-camera-background.md)).
//!
//! Like `lakko-noise-wasm`, this module imports nothing, so a worker
//! instantiates it with an empty import object and no wasm-bindgen glue. One
//! instance is one camera's processing: the segmenter, its health watch and
//! the exchange buffers live in a per-instance global.
//!
//! Exchange: the worker writes an RGBA picture of
//! [`lakko_segment::INPUT_WIDTH`] x [`lakko_segment::INPUT_HEIGHT`] at
//! [`lakko_seg_input_ptr`], calls [`lakko_seg_run`], and reads the alpha mask
//! (one byte per pixel) at [`lakko_seg_mask_ptr`]. Times are the worker's
//! monotonic clock in milliseconds, as `f64` (JavaScript numbers).
//!
//! The only `unsafe` is the `#[no_mangle]` attribute the exports need and
//! the custom `getrandom` backend's symbol; no function dereferences a raw
//! pointer. Every export is total: a refused call returns 0, never traps.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::cell::RefCell;
use std::mem::ManuallyDrop;

use lakko_segment::{
    blur_parameters, builtin_background, cover_source_rect, frame_action, model_input_rect,
    BlurStrength, FrameAction, HealthWatch, PixelLayout, ProcessorState, Segmenter, BUILTIN_IDS,
    INPUT_HEIGHT, INPUT_PIXELS, INPUT_WIDTH,
};

struct Instance {
    segmenter: Option<Segmenter>,
    health: HealthWatch,
    input: Vec<u8>,
    mask: Vec<u8>,
    picture: Vec<u8>,
    rect: [f32; 4],
}

// `ManuallyDrop`: the instance is never dropped. In a browser the module's
// one thread never ends, so nothing changes there; on a host thread (the
// tests) dropping the segmenter at thread exit would reach `tract`'s own
// thread-local state after it was destroyed and abort the process.
thread_local! {
    static INSTANCE: ManuallyDrop<RefCell<Instance>> = ManuallyDrop::new(RefCell::new(Instance {
        segmenter: None,
        health: HealthWatch::new(0),
        input: vec![0; INPUT_PIXELS * 4],
        mask: vec![0; INPUT_PIXELS],
        picture: Vec::new(),
        rect: [0.0; 4],
    }));
}

fn address_of<T>(buffer: &[T]) -> u32 {
    // On wasm32 an address is 32 bits; on a 64-bit host (the tests) an
    // address that does not fit reads as 0, which callers treat as "none".
    u32::try_from(buffer.as_ptr() as usize).unwrap_or(0)
}

fn millis(now_ms: f64) -> u64 {
    if now_ms.is_finite() && now_ms > 0.0 {
        // A browser's monotonic clock is far below 2^53 ms.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let ms = now_ms as u64;
        ms
    } else {
        0
    }
}

const fn state_code(state: ProcessorState) -> u32 {
    match state {
        ProcessorState::Starting => 0,
        ProcessorState::Working => 1,
        ProcessorState::Failed => 2,
    }
}

const fn strength(code: u32) -> BlurStrength {
    if code == 1 {
        BlurStrength::Strong
    } else {
        BlurStrength::Light
    }
}

/// Loads the model and starts the health watch at `now_ms`. Returns 1, or 0
/// when the model could not load (the watch is then `Failed`, so the gate
/// keeps dropping frames).
// SAFETY: `#[no_mangle]` only fixes the exported symbol name so the worker
// can find it; the function dereferences no raw pointer.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_init(now_ms: f64) -> u32 {
    INSTANCE.with(|cell| {
        let mut inst = cell.borrow_mut();
        inst.health.restart(millis(now_ms));
        if let Ok(s) = Segmenter::new() {
            inst.segmenter = Some(s);
            1
        } else {
            inst.segmenter = None;
            inst.health.on_error();
            0
        }
    })
}

/// Address of the RGBA input buffer (`INPUT_WIDTH * INPUT_HEIGHT * 4` bytes).
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_input_ptr() -> u32 {
    INSTANCE.with(|cell| address_of(&cell.borrow().input))
}

/// Address of the alpha mask (`INPUT_WIDTH * INPUT_HEIGHT` bytes), written by
/// [`lakko_seg_run`].
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_mask_ptr() -> u32 {
    INSTANCE.with(|cell| address_of(&cell.borrow().mask))
}

/// Width of the model's input picture and of the mask.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_input_width() -> u32 {
    u32::try_from(INPUT_WIDTH).unwrap_or(0)
}

/// Height of the model's input picture and of the mask.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_input_height() -> u32 {
    u32::try_from(INPUT_HEIGHT).unwrap_or(0)
}

/// Segments the input buffer into the mask and tells the health watch,
/// counting `took_ms` (measured by the caller around this call) at
/// `now_ms`. Returns the processor state code (0 starting, 1 working,
/// 2 failed). A failed run fails the watch.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_run(now_ms: f64, took_ms: f64) -> u32 {
    INSTANCE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Instance {
            segmenter,
            health,
            input,
            mask,
            ..
        } = &mut *guard;
        let Some(seg) = segmenter.as_mut() else {
            return state_code(health.on_error());
        };
        match seg.segment(input, PixelLayout::Rgba) {
            Ok(out) => {
                mask.copy_from_slice(out);
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let took = if took_ms.is_finite() && took_ms > 0.0 {
                    took_ms.min(f64::from(u32::MAX)) as u32
                } else {
                    0
                };
                state_code(health.on_mask(millis(now_ms), took))
            },
            Err(_) => state_code(health.on_error()),
        }
    })
}

/// Forgets the mask history and restarts the health watch at `now_ms` (the
/// person chose a background again, switched or turned on the camera).
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_restart(now_ms: f64) -> u32 {
    INSTANCE.with(|cell| {
        let mut inst = cell.borrow_mut();
        if let Some(seg) = inst.segmenter.as_mut() {
            seg.reset();
        }
        inst.health.restart(millis(now_ms));
        if inst.segmenter.is_none() {
            inst.health.on_error();
        }
        state_code(inst.health.state())
    })
}

/// A camera frame arrived at `now_ms`; returns the state to judge it by.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_on_frame(now_ms: f64) -> u32 {
    INSTANCE.with(|cell| state_code(cell.borrow_mut().health.on_frame(millis(now_ms))))
}

/// The frame gate: 0 send the camera, 1 send processed, 2 drop, 3 drop and
/// turn the camera off. `selected` is 1 when a background is selected;
/// `state` a state code. An unknown state code reads as failed.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_frame_action(selected: u32, state: u32) -> u32 {
    let state = match state {
        0 => ProcessorState::Starting,
        1 => ProcessorState::Working,
        _ => ProcessorState::Failed,
    };
    match frame_action(selected != 0, state) {
        FrameAction::SendCamera => 0,
        FrameAction::SendProcessed => 1,
        FrameAction::Drop => 2,
        FrameAction::DropAndStopCamera => 3,
    }
}

/// Blur down-scale divisor for `strength` (0 light, 1 strong) on a frame
/// `frame_height` tall.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_blur_divisor(strength_code: u32, frame_height: u32) -> u32 {
    blur_parameters(strength(strength_code), frame_height).divisor
}

/// Blur sigma, in pixels of the down-scaled frame.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_blur_sigma(strength_code: u32, frame_height: u32) -> f32 {
    blur_parameters(strength(strength_code), frame_height).sigma
}

/// Blur taps on each side of the centre.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_blur_radius(strength_code: u32, frame_height: u32) -> u32 {
    blur_parameters(strength(strength_code), frame_height).radius
}

/// Computes the part of a picture that fills a frame (`x, y, width, height`
/// as four `f32` at [`lakko_seg_rect_ptr`]). Returns the address.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_cover_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> u32 {
    INSTANCE.with(|cell| {
        let mut inst = cell.borrow_mut();
        let r = cover_source_rect(src_w, src_h, dst_w, dst_h);
        inst.rect = [r.x, r.y, r.width, r.height];
        address_of(&inst.rect)
    })
}

/// Computes where a whole upright `frame_w` x `frame_h` camera frame is drawn
/// inside the model input (shape kept, centred, bars where the shapes
/// differ) into the same four `f32` as [`lakko_seg_cover_rect`]. Returns the
/// address.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_model_input_rect(frame_w: u32, frame_h: u32) -> u32 {
    INSTANCE.with(|cell| {
        let mut inst = cell.borrow_mut();
        let r = model_input_rect(frame_w, frame_h);
        inst.rect = [r.x, r.y, r.width, r.height];
        address_of(&inst.rect)
    })
}

/// Address of the four `f32` written by [`lakko_seg_cover_rect`] or
/// [`lakko_seg_model_input_rect`].
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_rect_ptr() -> u32 {
    INSTANCE.with(|cell| address_of(&cell.borrow().rect))
}

/// How many built-in backgrounds there are.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_builtin_count() -> u32 {
    u32::try_from(BUILTIN_IDS.len()).unwrap_or(0)
}

/// Draws built-in background number `index` at `width` x `height` (RGBA,
/// `width * height * 4` bytes) into the instance's picture buffer and returns
/// its address, or 0 for a bad index or size.
// SAFETY: as for `lakko_seg_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_seg_builtin_render(index: u32, width: u32, height: u32) -> u32 {
    let Some(id) = usize::try_from(index).ok().and_then(|i| BUILTIN_IDS.get(i)) else {
        return 0;
    };
    let Some(pixels) = builtin_background(id, width, height) else {
        return 0;
    };
    INSTANCE.with(|cell| {
        let mut inst = cell.borrow_mut();
        inst.picture = pixels;
        address_of(&inst.picture)
    })
}

/// The segmentation model draws no random numbers; refuse any request, so
/// the module needs no host import for randomness.
// SAFETY: the symbol name and signature are the custom-backend contract of
// `getrandom` 0.4; the function writes nothing through `dest`.
#[cfg(target_family = "wasm")]
#[allow(unsafe_code)]
#[no_mangle]
unsafe extern "Rust" fn __getrandom_v03_custom(
    _dest: *mut u8,
    _len: usize,
) -> Result<(), getrandom::Error> {
    Err(getrandom::Error::UNSUPPORTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_codes() {
        assert_eq!(lakko_seg_frame_action(0, 2), 0);
        assert_eq!(lakko_seg_frame_action(1, 0), 2);
        assert_eq!(lakko_seg_frame_action(1, 1), 1);
        assert_eq!(lakko_seg_frame_action(1, 2), 3);
        assert_eq!(lakko_seg_frame_action(1, 99), 3);
    }

    #[test]
    fn runs_after_init_and_reports_working() {
        assert_eq!(lakko_seg_init(1.0), 1);
        assert_eq!(lakko_seg_on_frame(2.0), 0);
        assert_eq!(lakko_seg_run(3.0, 4.0), 1);
        assert_eq!(lakko_seg_on_frame(5.0), 1);
    }

    #[test]
    fn a_run_without_init_fails_the_watch() {
        // Each test runs on its own thread, so this instance is fresh.
        assert_eq!(lakko_seg_run(1.0, 1.0), 2);
        assert_eq!(lakko_seg_frame_action(1, lakko_seg_on_frame(2.0)), 3);
    }

    #[test]
    fn builtin_render_bounds() {
        assert_eq!(lakko_seg_builtin_count(), 6);
        assert_eq!(lakko_seg_builtin_render(6, 10, 10), 0);
        assert_eq!(lakko_seg_builtin_render(0, 0, 10), 0);
        assert_eq!(lakko_seg_input_width(), 256);
        assert_eq!(lakko_seg_input_height(), 144);
    }
}
