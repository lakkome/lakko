// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Flat C ABI around `lakko_segment` for the iOS and Android camera
//! pipelines ([ADR-0089](../../../docs/adr/0089-camera-background.md)).
//!
//! Why here and not in UniFFI: the camera pipeline runs per frame on the
//! capture and processing threads and hands over pixel buffers it already
//! holds; UniFFI would copy each picture into a fresh buffer and route the
//! call through generated bindings. Both clients already link this crate's
//! library for SFrame and the noise suppressor (see `noise.rs`), so no new
//! native library is added. The not-per-frame parts (the stored choice, the
//! built-in backgrounds, blur strengths, picture fit) are on UniFFI.
//!
//! Threads: one segmenter per camera. [`lakko_segment_run`] (inference, tens
//! of milliseconds on a phone) may run on a worker thread while
//! [`lakko_segment_on_frame`] is called from the capture thread: the model
//! and the health watch sit behind separate locks, so a frame is judged
//! without waiting for an inference to end.
//!
//! The rule the hosts apply per frame is [`lakko_segment_frame_action`]:
//! while a background is selected, a frame it was not applied to is never
//! sent.

use std::sync::Mutex;
use std::time::Instant;

use lakko_segment::{
    frame_action, FrameAction, HealthWatch, PixelLayout, ProcessorState, Segmenter, INPUT_HEIGHT,
    INPUT_PIXELS, INPUT_WIDTH,
};

/// One camera's segmenter and health watch, opaque to C
/// (`lakko_segmenter_t`).
pub struct LakkoSegmenter {
    model: Mutex<Option<Segmenter>>,
    health: Mutex<HealthWatch>,
}

/// The processing state (`lakko_segment_state_t`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LakkoSegmentState {
    /// The model is loading or has produced no mask yet: drop frames.
    Starting = 0,
    /// Masks arrive in time: send processed frames.
    Working = 1,
    /// Failed: drop frames, turn the camera off, show the failure line.
    Failed = 2,
}

/// The layout of the picture handed to [`lakko_segment_run`]
/// (`lakko_segment_layout_t`). 8 bits per channel, no row padding.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LakkoSegmentLayout {
    /// Red, green, blue, alpha.
    Rgba = 0,
    /// Blue, green, red, alpha (`kCVPixelFormatType_32BGRA`).
    Bgra = 1,
    /// Red, green, blue.
    Rgb = 2,
}

/// What to do with one camera frame (`lakko_frame_action_t`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LakkoFrameAction {
    /// No background selected: send the camera frame as it is.
    SendCamera = 0,
    /// Send the frame with the background applied.
    SendProcessed = 1,
    /// Send nothing for this frame.
    Drop = 2,
    /// Send nothing, turn the camera off and show the failure line.
    DropAndStopCamera = 3,
}

/// Width of the picture [`lakko_segment_run`] takes and of the mask.
pub const LAKKO_SEGMENT_INPUT_WIDTH: usize = 256;

/// Height of the picture and of the mask.
pub const LAKKO_SEGMENT_INPUT_HEIGHT: usize = 144;

const _: () = {
    assert!(LAKKO_SEGMENT_INPUT_WIDTH == INPUT_WIDTH);
    assert!(LAKKO_SEGMENT_INPUT_HEIGHT == INPUT_HEIGHT);
};

const fn to_c(state: ProcessorState) -> LakkoSegmentState {
    match state {
        ProcessorState::Starting => LakkoSegmentState::Starting,
        ProcessorState::Working => LakkoSegmentState::Working,
        ProcessorState::Failed => LakkoSegmentState::Failed,
    }
}

/// Runs `f` on the health watch; a poisoned lock reads as failed.
fn with_health(
    seg: &LakkoSegmenter,
    f: impl FnOnce(&mut HealthWatch) -> ProcessorState,
) -> LakkoSegmentState {
    match seg.health.lock() {
        Ok(mut watch) => to_c(f(&mut watch)),
        Err(_) => LakkoSegmentState::Failed,
    }
}

/// Loads the model and starts the health watch at `now_ms` (the host's
/// monotonic clock, milliseconds). Allocates and takes tens of milliseconds:
/// call it off the capture thread. Never null; a model that cannot load
/// leaves the segmenter `Failed`. Free it with [`lakko_segment_free`].
#[no_mangle]
pub extern "C" fn lakko_segment_new(now_ms: u64) -> *mut LakkoSegmenter {
    let model = Segmenter::new().ok();
    let mut health = HealthWatch::new(now_ms);
    if model.is_none() {
        health.on_error();
    }
    Box::into_raw(Box::new(LakkoSegmenter {
        model: Mutex::new(model),
        health: Mutex::new(health),
    }))
}

/// Frees a segmenter made by [`lakko_segment_new`]. Null is a no-op.
///
/// # Safety
///
/// `seg` is null or a pointer from [`lakko_segment_new`] not yet freed, and
/// no other thread is using it.
#[no_mangle]
pub unsafe extern "C" fn lakko_segment_free(seg: *mut LakkoSegmenter) {
    if seg.is_null() {
        return;
    }
    // SAFETY: the caller hands back a pointer from `lakko_segment_new`
    // (`Box::into_raw`) exactly once, with no concurrent user.
    drop(unsafe { Box::from_raw(seg) });
}

/// Segments one picture of [`LAKKO_SEGMENT_INPUT_WIDTH`] x
/// [`LAKKO_SEGMENT_INPUT_HEIGHT`] pixels (`len` bytes in `layout`) and writes
/// the smoothed alpha mask (one byte per pixel, 255 where the person is)
/// into `mask_out` (`mask_len` must be width x height). Tells the health
/// watch at `now_ms` how long it took. Returns the state after it; a buffer
/// of the wrong size or a failed run fails the watch (fail closed: a host
/// bug turns the camera off rather than sending the room). On failure
/// `mask_out` is left as it was.
///
/// # Safety
///
/// `seg` is a live pointer from [`lakko_segment_new`]; `pixels` points to
/// `len` readable bytes and `mask_out` to `mask_len` writable bytes, neither
/// aliased with the other for the call.
#[no_mangle]
pub unsafe extern "C" fn lakko_segment_run(
    seg: *const LakkoSegmenter,
    pixels: *const u8,
    len: usize,
    layout: LakkoSegmentLayout,
    now_ms: u64,
    mask_out: *mut u8,
    mask_len: usize,
) -> LakkoSegmentState {
    if seg.is_null() {
        return LakkoSegmentState::Failed;
    }
    // SAFETY: non-null live pointer from `lakko_segment_new` (caller
    // contract); only shared access, the locks inside serialise mutation.
    let seg = unsafe { &*seg };
    if pixels.is_null() || mask_out.is_null() || mask_len != INPUT_PIXELS {
        return with_health(seg, HealthWatch::on_error);
    }
    let layout = match layout {
        LakkoSegmentLayout::Rgba => PixelLayout::Rgba,
        LakkoSegmentLayout::Bgra => PixelLayout::Bgra,
        LakkoSegmentLayout::Rgb => PixelLayout::Rgb,
    };
    // SAFETY: `pixels` points to `len` readable bytes (caller contract).
    let input = unsafe { std::slice::from_raw_parts(pixels, len) };
    let started = Instant::now();
    let produced = match seg.model.lock() {
        Ok(mut model) => match model.as_mut() {
            Some(m) => match m.segment(input, layout) {
                Ok(mask) => {
                    // SAFETY: `mask_out` points to `mask_len == INPUT_PIXELS`
                    // writable bytes not aliased with `pixels` (caller
                    // contract).
                    let out = unsafe { std::slice::from_raw_parts_mut(mask_out, mask_len) };
                    out.copy_from_slice(mask);
                    true
                },
                Err(_) => false,
            },
            None => false,
        },
        Err(_) => false,
    };
    if !produced {
        return with_health(seg, HealthWatch::on_error);
    }
    let took = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
    with_health(seg, |w| w.on_mask(now_ms, took))
}

/// A camera frame arrived at `now_ms`: returns the state to judge it by. A
/// start that took too long or masks that stopped fail here. Takes only the
/// health lock, so it never waits for an inference.
///
/// # Safety
///
/// `seg` is null (answered `Failed`) or a live pointer from
/// [`lakko_segment_new`].
#[no_mangle]
pub unsafe extern "C" fn lakko_segment_on_frame(
    seg: *const LakkoSegmenter,
    now_ms: u64,
) -> LakkoSegmentState {
    if seg.is_null() {
        return LakkoSegmentState::Failed;
    }
    // SAFETY: non-null live pointer (caller contract).
    let seg = unsafe { &*seg };
    with_health(seg, |w| w.on_frame(now_ms))
}

/// Forgets the mask history and restarts the health watch at `now_ms` (a
/// background chosen again, the camera switched or turned back on). A
/// segmenter whose model never loaded stays `Failed`.
///
/// # Safety
///
/// As for [`lakko_segment_on_frame`].
#[no_mangle]
pub unsafe extern "C" fn lakko_segment_restart(
    seg: *const LakkoSegmenter,
    now_ms: u64,
) -> LakkoSegmentState {
    if seg.is_null() {
        return LakkoSegmentState::Failed;
    }
    // SAFETY: non-null live pointer (caller contract).
    let seg = unsafe { &*seg };
    let loaded = match seg.model.lock() {
        Ok(mut model) => model.as_mut().is_some_and(|m| {
            m.reset();
            true
        }),
        Err(_) => false,
    };
    with_health(seg, |w| {
        w.restart(now_ms);
        if loaded {
            w.state()
        } else {
            w.on_error()
        }
    })
}

/// The current state, without judging a frame.
///
/// # Safety
///
/// As for [`lakko_segment_on_frame`].
#[no_mangle]
pub unsafe extern "C" fn lakko_segment_state(seg: *const LakkoSegmenter) -> LakkoSegmentState {
    if seg.is_null() {
        return LakkoSegmentState::Failed;
    }
    // SAFETY: non-null live pointer (caller contract).
    let seg = unsafe { &*seg };
    with_health(seg, |w| w.state())
}

/// The frame gate: what to do with a camera frame when a background is
/// `selected` (or not) and the processing is in `state`.
#[no_mangle]
pub extern "C" fn lakko_segment_frame_action(
    selected: bool,
    state: LakkoSegmentState,
) -> LakkoFrameAction {
    let state = match state {
        LakkoSegmentState::Starting => ProcessorState::Starting,
        LakkoSegmentState::Working => ProcessorState::Working,
        LakkoSegmentState::Failed => ProcessorState::Failed,
    };
    match frame_action(selected, state) {
        FrameAction::SendCamera => LakkoFrameAction::SendCamera,
        FrameAction::SendProcessed => LakkoFrameAction::SendProcessed,
        FrameAction::Drop => LakkoFrameAction::Drop,
        FrameAction::DropAndStopCamera => LakkoFrameAction::DropAndStopCamera,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_judges_and_refuses_bad_buffers() {
        let seg = lakko_segment_new(0);
        assert!(!seg.is_null());
        // SAFETY: live pointer, test-local buffers of the stated sizes.
        unsafe {
            assert_eq!(lakko_segment_on_frame(seg, 1), LakkoSegmentState::Starting);
            let px = vec![100u8; INPUT_PIXELS * 4];
            let mut mask = vec![7u8; INPUT_PIXELS];
            let s = lakko_segment_run(
                seg,
                px.as_ptr(),
                px.len(),
                LakkoSegmentLayout::Rgba,
                2,
                mask.as_mut_ptr(),
                mask.len(),
            );
            assert_eq!(s, LakkoSegmentState::Working);
            assert_eq!(
                lakko_segment_frame_action(true, lakko_segment_on_frame(seg, 3)),
                LakkoFrameAction::SendProcessed
            );
            // A short buffer is a host bug: fail closed.
            let s = lakko_segment_run(
                seg,
                px.as_ptr(),
                10,
                LakkoSegmentLayout::Rgba,
                4,
                mask.as_mut_ptr(),
                mask.len(),
            );
            assert_eq!(s, LakkoSegmentState::Failed);
            assert_eq!(
                lakko_segment_frame_action(true, lakko_segment_state(seg)),
                LakkoFrameAction::DropAndStopCamera
            );
            assert_eq!(lakko_segment_restart(seg, 5), LakkoSegmentState::Starting);
            lakko_segment_free(seg);
        }
    }

    #[test]
    fn null_is_failed_and_free_null_is_a_no_op() {
        // SAFETY: null is allowed by every function here.
        unsafe {
            assert_eq!(
                lakko_segment_on_frame(std::ptr::null(), 0),
                LakkoSegmentState::Failed
            );
            assert_eq!(
                lakko_segment_state(std::ptr::null()),
                LakkoSegmentState::Failed
            );
            lakko_segment_free(std::ptr::null_mut());
        }
        assert_eq!(
            lakko_segment_frame_action(false, LakkoSegmentState::Failed),
            LakkoFrameAction::SendCamera
        );
    }
}
