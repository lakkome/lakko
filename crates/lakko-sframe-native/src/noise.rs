// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Flat C ABI around `lakko_noise` for the native capture hooks
//! ([ADR-0088](../../../docs/adr/0088-client-noise-suppression-rnnoise.md)).
//!
//! Why it lives in this crate: the Android capture post-processing hook of
//! the pinned libwebrtc (`ExternalAudioProcessingFactory`) hands a direct
//! `ByteBuffer` over libwebrtc's own capture buffer every 10 ms, on the
//! audio processing thread. Cleaning it in place needs the buffer's address
//! (`GetDirectBufferAddress`), which only a hand-written JNI function can
//! take; UniFFI would copy the samples into a fresh buffer on every call.
//! `libsframe_jni.so` is the one native library on Android that already
//! carries hand-written JNI entry points for the real-time media threads,
//! and it links this crate's static archive; a second Rust static archive
//! cannot be linked into the same shared library (both carry the Rust
//! standard library). iOS links the same archive, so the same functions are
//! there for an iOS hook when one exists.
//!
//! Real-time rules: [`lakko_noise_process_s16_in_place`] allocates nothing,
//! takes no lock and never panics on any input; [`lakko_noise_new`] and
//! [`lakko_noise_free`] allocate and free, so a host calls them from a
//! control thread, never from the audio callback.

use lakko_noise::{NoiseError, NoiseSuppressor};

/// One microphone's suppressor, opaque to C (`lakko_noise_suppressor_t`).
pub struct LakkoNoiseSuppressor(NoiseSuppressor);

/// What [`lakko_noise_process_s16_in_place`] did (`lakko_noise_status_t`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LakkoNoiseStatus {
    /// The samples were cleaned in place.
    Ok = 0,
    /// `suppressor` or `samples` was null; nothing was touched.
    NullPointer = 1,
    /// More than [`LAKKO_NOISE_MAX_BLOCK`] samples (at a lower rate, more
    /// than `LAKKO_NOISE_MAX_BLOCK / factor`); nothing was touched.
    BlockTooLong = 2,
    /// The rate is not one the suppressor takes (48, 24, 16, 12 or 8 kHz);
    /// nothing was touched. (3 is left to the Android bridge's own refusal of
    /// a bad buffer.)
    UnsupportedRate = 4,
}

/// The only sample rate the suppressor runs at. A host whose capture runs
/// at any other rate leaves its audio alone.
pub const LAKKO_NOISE_SAMPLE_RATE: u32 = 48_000;

/// The constant delay, in samples, the suppressor adds (one 10 ms frame).
pub const LAKKO_NOISE_LATENCY_SAMPLES: usize = 480;

/// The largest block one [`lakko_noise_process_s16_in_place`] call takes.
pub const LAKKO_NOISE_MAX_BLOCK: usize = 8192;

const _: () = {
    assert!(LAKKO_NOISE_SAMPLE_RATE == lakko_noise::SAMPLE_RATE);
    assert!(LAKKO_NOISE_LATENCY_SAMPLES == lakko_noise::LATENCY_SAMPLES);
    assert!(LAKKO_NOISE_MAX_BLOCK == lakko_noise::MAX_BLOCK);
};

/// A fresh suppressor. Allocates; call it off the audio thread. Never
/// null. Free it with [`lakko_noise_free`].
#[no_mangle]
pub extern "C" fn lakko_noise_new() -> *mut LakkoNoiseSuppressor {
    Box::into_raw(Box::new(LakkoNoiseSuppressor(NoiseSuppressor::new())))
}

/// Frees a suppressor made by [`lakko_noise_new`]. Null is a no-op.
///
/// # Safety
///
/// `suppressor` is null or a pointer returned by [`lakko_noise_new`] that
/// has not been freed, and no other thread is inside
/// [`lakko_noise_process_s16_in_place`] with it.
#[no_mangle]
pub unsafe extern "C" fn lakko_noise_free(suppressor: *mut LakkoNoiseSuppressor) {
    if suppressor.is_null() {
        return;
    }
    // SAFETY: the caller hands back a pointer from `lakko_noise_new`
    // (`Box::into_raw`) exactly once, with no concurrent user.
    drop(unsafe { Box::from_raw(suppressor) });
}

/// Cleans `len` samples at `samples` in place. The samples are `f32` in the
/// range of 16-bit PCM (`[-32768, 32767]`), mono, at
/// [`LAKKO_NOISE_SAMPLE_RATE`]; the output is the input stream delayed by
/// [`LAKKO_NOISE_LATENCY_SAMPLES`] with noise removed. Allocates nothing.
/// `len == 0` is `Ok` and touches nothing.
///
/// # Safety
///
/// `suppressor` is a live pointer from [`lakko_noise_new`] used by one
/// thread at a time; `samples` points to `len` writable, initialised,
/// properly aligned `f32` values not aliased for the call's duration.
#[no_mangle]
pub unsafe extern "C" fn lakko_noise_process_s16_in_place(
    suppressor: *mut LakkoNoiseSuppressor,
    samples: *mut f32,
    len: usize,
) -> LakkoNoiseStatus {
    if suppressor.is_null() || samples.is_null() {
        return LakkoNoiseStatus::NullPointer;
    }
    if len > LAKKO_NOISE_MAX_BLOCK {
        return LakkoNoiseStatus::BlockTooLong;
    }
    // SAFETY: non-null, from `lakko_noise_new`, one thread at a time
    // (caller contract).
    let state = unsafe { &mut (*suppressor).0 };
    // SAFETY: `samples` is non-null and points to `len` initialised,
    // aligned, unaliased `f32`s (caller contract); `len` is bounded above.
    let block = unsafe { std::slice::from_raw_parts_mut(samples, len) };
    match state.process_s16_in_place(block) {
        Ok(()) => LakkoNoiseStatus::Ok,
        Err(NoiseError::BlockTooLong | NoiseError::LengthMismatch) => {
            LakkoNoiseStatus::BlockTooLong
        },
        Err(NoiseError::UnsupportedRate) => LakkoNoiseStatus::UnsupportedRate,
    }
}

/// Whether [`lakko_noise_process_s16_in_place_at_rate`] takes `rate_hz`:
/// 48000, or a rate it divides by a whole factor (24000, 16000, 12000,
/// 8000).
#[no_mangle]
pub extern "C" fn lakko_noise_supports_rate(rate_hz: u32) -> bool {
    NoiseSuppressor::supports_rate(rate_hz)
}

/// [`lakko_noise_process_s16_in_place`] for a mono stream at `rate_hz`
/// (ADR-0088 amendment of 2026-11-10): a rate below 48 kHz is raised to
/// 48 kHz, cleaned and lowered back. The rate may change between calls.
/// Allocates nothing. `len == 0` is `Ok` and touches nothing.
///
/// # Safety
///
/// As [`lakko_noise_process_s16_in_place`].
#[no_mangle]
pub unsafe extern "C" fn lakko_noise_process_s16_in_place_at_rate(
    suppressor: *mut LakkoNoiseSuppressor,
    rate_hz: u32,
    samples: *mut f32,
    len: usize,
) -> LakkoNoiseStatus {
    if suppressor.is_null() || samples.is_null() {
        return LakkoNoiseStatus::NullPointer;
    }
    if !NoiseSuppressor::supports_rate(rate_hz) {
        return LakkoNoiseStatus::UnsupportedRate;
    }
    if len > LAKKO_NOISE_MAX_BLOCK {
        return LakkoNoiseStatus::BlockTooLong;
    }
    // SAFETY: non-null, from `lakko_noise_new`, one thread at a time
    // (caller contract).
    let state = unsafe { &mut (*suppressor).0 };
    // SAFETY: `samples` is non-null and points to `len` initialised,
    // aligned, unaliased `f32`s (caller contract); `len` is bounded above.
    let block = unsafe { std::slice::from_raw_parts_mut(samples, len) };
    match state.process_s16_in_place_at(rate_hz, block) {
        Ok(()) => LakkoNoiseStatus::Ok,
        Err(NoiseError::BlockTooLong | NoiseError::LengthMismatch) => {
            LakkoNoiseStatus::BlockTooLong
        },
        Err(NoiseError::UnsupportedRate) => LakkoNoiseStatus::UnsupportedRate,
    }
}

/// RNNoise's estimate, in `[0, 1]`, that the last whole frame held a voice.
/// Diagnostics only. `0` for null.
///
/// # Safety
///
/// `suppressor` is null or a live pointer from [`lakko_noise_new`] not being
/// processed by another thread at the same moment.
#[no_mangle]
pub unsafe extern "C" fn lakko_noise_voice_probability(
    suppressor: *const LakkoNoiseSuppressor,
) -> f32 {
    if suppressor.is_null() {
        return 0.0;
    }
    // SAFETY: non-null live pointer (caller contract).
    unsafe { (*suppressor).0.voice_probability() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processes_in_place_and_refuses_bad_input() {
        let ns = lakko_noise_new();
        assert!(!ns.is_null());
        // A loud low-passed noise, as the host would hand it in FloatS16.
        let mut seed = 7u32;
        let mut acc = 0.0f32;
        let mut samples: Vec<f32> = (0..48_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                #[allow(clippy::cast_precision_loss)]
                let unit = (seed >> 8) as f32 / (1u32 << 24) as f32;
                acc = acc * 0.95 + (unit * 2.0 - 1.0) * 0.05;
                acc * 4.0 * 3_000.0
            })
            .collect();
        let before: f32 = samples[24_000..].iter().map(|s| s * s).sum();
        for chunk in samples.chunks_mut(480) {
            // SAFETY: `ns` is live; `chunk` is a valid mutable slice.
            let st =
                unsafe { lakko_noise_process_s16_in_place(ns, chunk.as_mut_ptr(), chunk.len()) };
            assert_eq!(st, LakkoNoiseStatus::Ok);
        }
        let after: f32 = samples[24_000..].iter().map(|s| s * s).sum();
        assert!(after < before * 0.01, "energy {before} -> {after}");
        assert!(samples
            .iter()
            .all(|s| s.is_finite() && (-32_768.0..=32_767.0).contains(s)));
        // SAFETY: `ns` is live.
        let p = unsafe { lakko_noise_voice_probability(ns) };
        assert!((0.0..=1.0).contains(&p));

        let mut one = [1.0f32];
        // SAFETY: null pointers are checked before any dereference.
        unsafe {
            assert_eq!(
                lakko_noise_process_s16_in_place(std::ptr::null_mut(), one.as_mut_ptr(), 1),
                LakkoNoiseStatus::NullPointer
            );
            assert_eq!(
                lakko_noise_process_s16_in_place(ns, std::ptr::null_mut(), 1),
                LakkoNoiseStatus::NullPointer
            );
            assert_eq!(
                lakko_noise_process_s16_in_place(ns, one.as_mut_ptr(), LAKKO_NOISE_MAX_BLOCK + 1),
                LakkoNoiseStatus::BlockTooLong
            );
            assert_eq!(
                lakko_noise_process_s16_in_place(ns, one.as_mut_ptr(), 0),
                LakkoNoiseStatus::Ok
            );
        }
        assert!((one[0] - 1.0).abs() < f32::EPSILON);
        // SAFETY: `ns` from `lakko_noise_new`, freed once; null is a no-op.
        unsafe {
            lakko_noise_free(ns);
            lakko_noise_free(std::ptr::null_mut());
            assert!(lakko_noise_voice_probability(std::ptr::null()).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn processes_a_16k_stream_and_refuses_an_unsupported_rate() {
        assert!(lakko_noise_supports_rate(16_000));
        assert!(lakko_noise_supports_rate(48_000));
        assert!(!lakko_noise_supports_rate(44_100));
        let ns = lakko_noise_new();
        let mut seed = 11u32;
        let mut acc = 0.0f32;
        let mut samples: Vec<f32> = (0..32_000)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                #[allow(clippy::cast_precision_loss)]
                let unit = (seed >> 8) as f32 / (1u32 << 24) as f32;
                acc = acc * 0.95 + (unit * 2.0 - 1.0) * 0.05;
                acc * 4.0 * 3_000.0
            })
            .collect();
        let before: f32 = samples[16_000..].iter().map(|s| s * s).sum();
        for chunk in samples.chunks_mut(160) {
            // SAFETY: `ns` is live; `chunk` is a valid mutable slice.
            let st = unsafe {
                lakko_noise_process_s16_in_place_at_rate(
                    ns,
                    16_000,
                    chunk.as_mut_ptr(),
                    chunk.len(),
                )
            };
            assert_eq!(st, LakkoNoiseStatus::Ok);
        }
        let after: f32 = samples[16_000..].iter().map(|s| s * s).sum();
        assert!(after < before * 0.1, "energy {before} -> {after}");
        let mut one = [1.0f32];
        // SAFETY: `ns` is live; `one` is a valid slice.
        unsafe {
            assert_eq!(
                lakko_noise_process_s16_in_place_at_rate(ns, 44_100, one.as_mut_ptr(), 1),
                LakkoNoiseStatus::UnsupportedRate
            );
            assert_eq!(
                lakko_noise_process_s16_in_place_at_rate(
                    std::ptr::null_mut(),
                    16_000,
                    one.as_mut_ptr(),
                    1
                ),
                LakkoNoiseStatus::NullPointer
            );
            assert_eq!(
                lakko_noise_process_s16_in_place_at_rate(
                    ns,
                    16_000,
                    one.as_mut_ptr(),
                    LAKKO_NOISE_MAX_BLOCK + 1
                ),
                LakkoNoiseStatus::BlockTooLong
            );
        }
        assert!((one[0] - 1.0).abs() < f32::EPSILON);
        // SAFETY: `ns` from `lakko_noise_new`, freed once.
        unsafe { lakko_noise_free(ns) };
    }
}
