// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Raw WebAssembly exports of [`lakko_noise`] for the web client's
//! `AudioWorklet` ([ADR-0088](../../../docs/adr/0088-client-noise-suppression-rnnoise.md)).
//!
//! Why not `lakko-ffi-wasm`: an `AudioWorkletGlobalScope` has no `fetch`
//! and, in some engines, no `TextDecoder`, and the main wasm module's
//! wasm-bindgen glue imports dozens of host functions. This module imports
//! nothing: the page compiles it, hands the bytes to the worklet, and the
//! worklet instantiates it with an empty import object.
//!
//! One wasm instance is one suppressor (the worklet makes one instance per
//! microphone), so the state lives in a per-instance global rather than
//! behind a pointer the JavaScript side would have to carry. The exchange
//! buffers live in the instance's linear memory: JavaScript writes up to
//! [`lakko_noise::MAX_BLOCK`] samples at [`lakko_ns_input_ptr`], calls
//! [`lakko_ns_process`], and reads the same count at [`lakko_ns_output_ptr`].
//!
//! The only `unsafe` here is the `#[no_mangle]` attribute the exports need
//! (the `unsafe_code` lint flags it); no function dereferences a raw
//! pointer. Every export is total: a refused call returns 0, never traps.

#![deny(unsafe_code)]
#![warn(missing_docs)]

use std::cell::RefCell;

use lakko_noise::{NoiseSuppressor, LATENCY_SAMPLES, MAX_BLOCK, SAMPLE_RATE};

struct Instance {
    suppressor: NoiseSuppressor,
    input: Vec<f32>,
    output: Vec<f32>,
}

thread_local! {
    static INSTANCE: RefCell<Option<Instance>> = const { RefCell::new(None) };
}

fn address_of(buffer: &[f32]) -> u32 {
    // On wasm32 an address is 32 bits; on a 64-bit host (the tests) an
    // address that does not fit reads as 0, which callers treat as "no
    // buffer".
    u32::try_from(buffer.as_ptr() as usize).unwrap_or(0)
}

/// Creates (or re-creates) this instance's suppressor and buffers. Returns 1.
// SAFETY: `#[no_mangle]` only fixes the exported symbol name so the
// worklet can find it; the function dereferences no raw pointer.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_init() -> u32 {
    INSTANCE.with(|cell| {
        *cell.borrow_mut() = Some(Instance {
            suppressor: NoiseSuppressor::new(),
            input: vec![0.0; MAX_BLOCK],
            output: vec![0.0; MAX_BLOCK],
        });
    });
    1
}

/// Address of the input buffer ([`lakko_noise::MAX_BLOCK`] `f32`s), or 0
/// before [`lakko_ns_init`].
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_input_ptr() -> u32 {
    INSTANCE.with(|cell| cell.borrow().as_ref().map_or(0, |i| address_of(&i.input)))
}

/// Address of the output buffer, or 0 before [`lakko_ns_init`].
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_output_ptr() -> u32 {
    INSTANCE.with(|cell| cell.borrow().as_ref().map_or(0, |i| address_of(&i.output)))
}

/// Cleans the first `n` samples of the input buffer into the output
/// buffer. Returns 1, or 0 when not initialised or `n` exceeds
/// [`lakko_noise::MAX_BLOCK`].
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
// The buffers are read and written through ordinary slices of the `Vec`s
// the instance owns; the JavaScript side writes them between calls through
// the instance's linear memory, as any wasm buffer exchange does.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_process(n: u32) -> u32 {
    let Ok(n) = usize::try_from(n) else { return 0 };
    if n > MAX_BLOCK {
        return 0;
    }
    INSTANCE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(inst) = guard.as_mut() else { return 0 };
        let Instance {
            suppressor,
            input,
            output,
        } = inst;
        match suppressor.process(&input[..n], &mut output[..n]) {
            Ok(()) => 1,
            Err(_) => 0,
        }
    })
}

/// RNNoise's voice probability for the last whole frame, or 0.
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_voice_probability() -> f32 {
    INSTANCE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map_or(0.0, |i| i.suppressor.voice_probability())
    })
}

/// The largest `n` [`lakko_ns_process`] accepts.
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_max_block() -> u32 {
    u32::try_from(MAX_BLOCK).unwrap_or(0)
}

/// The only sample rate the suppressor may run at.
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_sample_rate() -> u32 {
    SAMPLE_RATE
}

/// The delay, in samples, the suppressor adds.
// SAFETY: as for `lakko_ns_init`: a name for the export, no dereference.
#[allow(unsafe_code)]
#[no_mangle]
pub extern "C" fn lakko_ns_latency_samples() -> u32 {
    u32::try_from(LATENCY_SAMPLES).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_before_init_and_oversized_blocks() {
        // Each test runs on its own thread, so this instance is fresh.
        assert_eq!(lakko_ns_process(1), 0);
        assert_eq!(lakko_ns_input_ptr(), 0);
        assert_eq!(lakko_ns_init(), 1);
        assert_eq!(lakko_ns_process(lakko_ns_max_block() + 1), 0);
        assert_eq!(lakko_ns_process(128), 1);
    }

    #[test]
    fn constants_match_the_core() {
        assert_eq!(lakko_ns_sample_rate(), 48_000);
        assert_eq!(lakko_ns_latency_samples() as usize, LATENCY_SAMPLES);
        assert_eq!(lakko_ns_max_block() as usize, MAX_BLOCK);
    }
}
