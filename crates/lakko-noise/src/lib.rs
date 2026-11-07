// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

//! Client-side noise suppression for call audio
//! ([ADR-0088](../../../docs/adr/0088-client-noise-suppression-rnnoise.md)).
//!
//! A [`NoiseSuppressor`] cleans a stream of mono 48 kHz samples with RNNoise
//! (the `nnnoiseless` Rust port of xiph's library). It runs on the sender's
//! own device, on the microphone signal after the platform's echo canceller
//! and before the encoder, so before SFrame: it sees only this device's own
//! plaintext PCM and adds nothing to the wire.
//!
//! RNNoise works on fixed frames of [`FRAME_SIZE`] samples. A host hands
//! audio in whatever block size its audio thread uses (128 samples in a web
//! `AudioWorklet`, other sizes on the native platforms), so the suppressor
//! buffers: every output sample is the cleaned input sample from exactly
//! [`LATENCY_SAMPLES`] samples earlier, whatever the block sizes. That
//! constant delay is the whole latency the suppressor adds (10 ms at 48 kHz),
//! and the output does not depend on how the input was cut into blocks.
//!
//! Samples are `f32` in `[-1, 1]`, the range Web Audio and the native audio
//! units use. A non-finite input sample is treated as silence; every output
//! sample is finite and within `[-1, 1]`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::VecDeque;

use nnnoiseless::DenoiseState;

/// The only sample rate RNNoise's model is trained for. A host whose audio
/// runs at any other rate must not use the suppressor (it would distort the
/// voice); it sends its microphone unprocessed instead.
pub const SAMPLE_RATE: u32 = 48_000;

/// Samples per RNNoise frame (10 ms at [`SAMPLE_RATE`]).
pub const FRAME_SIZE: usize = DenoiseState::FRAME_SIZE;

/// The constant delay, in samples, between an input sample and its cleaned
/// output sample.
pub const LATENCY_SAMPLES: usize = FRAME_SIZE;

/// The largest block one [`NoiseSuppressor::process`] call accepts. Bounds
/// the buffers, which are allocated once in [`NoiseSuppressor::new`] so the
/// audio thread never allocates.
pub const MAX_BLOCK: usize = 8192;

/// RNNoise works on samples in the range of 16-bit PCM.
const PCM_SCALE: f32 = 32_768.0;

/// The largest 16-bit PCM value, as `f32`.
const PCM_MAX_S16: f32 = 32_767.0;

/// Why a block was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseError {
    /// `input` and `output` differ in length.
    LengthMismatch,
    /// The block is longer than [`MAX_BLOCK`].
    BlockTooLong,
}

impl std::fmt::Display for NoiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LengthMismatch => f.write_str("input and output differ in length"),
            Self::BlockTooLong => f.write_str("block longer than MAX_BLOCK"),
        }
    }
}

impl std::error::Error for NoiseError {}

/// One stream's noise suppressor. Holds RNNoise's recurrent state, so one
/// instance serves one microphone and is never shared between streams.
pub struct NoiseSuppressor {
    state: Box<DenoiseState<'static>>,
    /// Scaled input samples not yet making up a whole frame.
    pending: Vec<f32>,
    /// Cleaned samples waiting to be handed out. Starts with
    /// [`LATENCY_SAMPLES`] zeros; `pending.len() + ready.len()` is always
    /// [`LATENCY_SAMPLES`] between calls, which is what keeps the delay
    /// constant and guarantees `ready` never runs dry.
    ready: VecDeque<f32>,
    frame_out: Vec<f32>,
    voice_probability: f32,
}

impl Default for NoiseSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseSuppressor {
    /// A fresh suppressor. Allocates every buffer it will ever use.
    pub fn new() -> Self {
        let mut ready = VecDeque::with_capacity(LATENCY_SAMPLES + MAX_BLOCK + FRAME_SIZE);
        ready.extend(std::iter::repeat(0.0).take(LATENCY_SAMPLES));
        Self {
            state: DenoiseState::new(),
            pending: Vec::with_capacity(FRAME_SIZE),
            ready,
            frame_out: vec![0.0; FRAME_SIZE],
            voice_probability: 0.0,
        }
    }

    /// Cleans `input` into `output` (same length, at most [`MAX_BLOCK`]).
    /// `output[i]` is the cleaned sample of the input stream at position
    /// `stream_position + i - LATENCY_SAMPLES`; the first
    /// [`LATENCY_SAMPLES`] samples a fresh suppressor emits are silence.
    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<(), NoiseError> {
        if input.len() != output.len() {
            return Err(NoiseError::LengthMismatch);
        }
        if input.len() > MAX_BLOCK {
            return Err(NoiseError::BlockTooLong);
        }
        for &sample in input {
            let clean = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
            self.push(clean * PCM_SCALE);
        }
        for slot in output.iter_mut() {
            *slot = self.pop();
        }
        Ok(())
    }

    /// Cleans `samples` in place, for a host whose audio is `f32` in the
    /// range of 16-bit PCM (`[-32768, 32767]`, libwebrtc's "FloatS16", the
    /// format of the Android capture post-processing hook). At most
    /// [`MAX_BLOCK`] samples. The same stream as [`Self::process`]: one
    /// suppressor must be fed through one of the two only. A non-finite
    /// sample is silence; every output sample is finite and within the
    /// 16-bit range. Allocates nothing.
    pub fn process_s16_in_place(&mut self, samples: &mut [f32]) -> Result<(), NoiseError> {
        if samples.len() > MAX_BLOCK {
            return Err(NoiseError::BlockTooLong);
        }
        for &sample in samples.iter() {
            let clean = if sample.is_finite() {
                sample.clamp(-PCM_SCALE, PCM_MAX_S16)
            } else {
                0.0
            };
            self.push(clean);
        }
        for slot in samples.iter_mut() {
            *slot = (self.pop() * PCM_SCALE).clamp(-PCM_SCALE, PCM_MAX_S16);
        }
        Ok(())
    }

    /// Feeds one sample already in the 16-bit range; runs RNNoise on every
    /// whole frame.
    fn push(&mut self, pcm: f32) {
        self.pending.push(pcm);
        if self.pending.len() == FRAME_SIZE {
            self.voice_probability = self.state.process_frame(&mut self.frame_out, &self.pending);
            self.pending.clear();
            for &out in &self.frame_out {
                let scaled = out / PCM_SCALE;
                let bounded = if scaled.is_finite() {
                    scaled.clamp(-1.0, 1.0)
                } else {
                    0.0
                };
                self.ready.push_back(bounded);
            }
        }
    }

    /// The next cleaned sample, in `[-1, 1]`.
    fn pop(&mut self) -> f32 {
        // Never `None` by the invariant on `ready` (see the field doc);
        // silence rather than a panic if it ever were.
        self.ready.pop_front().unwrap_or(0.0)
    }

    /// RNNoise's estimate, in `[0, 1]`, that the last whole frame held a
    /// voice. Diagnostics only; nothing decides on it.
    pub fn voice_probability(&self) -> f32 {
        self.voice_probability
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random sequence in `[-amp, amp]`, so the tests
    /// need no RNG crate and give the same numbers on every run.
    fn noise(len: usize, amp: f32, mut seed: u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                #[allow(clippy::cast_precision_loss)]
                let unit = (seed >> 8) as f32 / (1u32 << 24) as f32;
                (unit * 2.0 - 1.0) * amp
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let n = samples.len() as f32;
        (samples.iter().map(|s| s * s).sum::<f32>() / n).sqrt()
    }

    fn run(input: &[f32], block: usize) -> Vec<f32> {
        let mut ns = NoiseSuppressor::new();
        let mut out = vec![0.0; input.len()];
        for (i, o) in input.chunks(block).zip(out.chunks_mut(block)) {
            ns.process(i, o).unwrap();
        }
        out
    }

    #[test]
    fn output_does_not_depend_on_block_size() {
        let input = noise(FRAME_SIZE * 7 + 13, 0.2, 7);
        let reference = run(&input, FRAME_SIZE);
        for block in [1, 7, 128, 479, 481, 1000, 4096] {
            assert_eq!(run(&input, block), reference, "block {block}");
        }
    }

    #[test]
    fn first_latency_samples_are_silence() {
        let input = noise(LATENCY_SAMPLES * 3, 0.5, 1);
        let out = run(&input, 128);
        assert!(out[..LATENCY_SAMPLES].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn stationary_noise_is_attenuated() {
        // Two seconds of low-passed noise (a one-pole filter over the
        // pseudo-random sequence), the kind of stationary hum RNNoise is
        // trained to remove. Observed while writing this test: this input
        // drops by far more than the bound below, while flat full-band
        // white noise from the same generator came out only about 10 percent
        // quieter and with a voice probability near 0.5, so the model does
        // not treat every noise alike. The bound is loose on purpose: it
        // checks the model is wired up, not its quality, which needs
        // recordings.
        let white = noise(SAMPLE_RATE as usize * 2, 0.1, 42);
        let mut acc = 0.0f32;
        let input: Vec<f32> = white
            .iter()
            .map(|x| {
                acc = acc * 0.95 + x * 0.05;
                acc * 4.0
            })
            .collect();
        let out = run(&input, 128);
        let skip = SAMPLE_RATE as usize / 2; // let the recurrent state settle
        let rin = rms(&input[skip..]);
        let rout = rms(&out[skip..]);
        assert!(rout < rin * 0.1, "rms in {rin} out {rout}");
    }

    #[test]
    fn hostile_samples_stay_bounded() {
        let mut input = noise(FRAME_SIZE * 4, 4.0, 3);
        input[5] = f32::NAN;
        input[600] = f32::INFINITY;
        input[700] = f32::NEG_INFINITY;
        let out = run(&input, 333);
        assert!(out
            .iter()
            .all(|s| s.is_finite() && (-1.0..=1.0).contains(s)));
    }

    #[test]
    fn refuses_bad_blocks() {
        let mut ns = NoiseSuppressor::new();
        let mut out = vec![0.0; 3];
        assert_eq!(
            ns.process(&[0.0; 4], &mut out),
            Err(NoiseError::LengthMismatch)
        );
        let big = vec![0.0; MAX_BLOCK + 1];
        let mut big_out = vec![0.0; MAX_BLOCK + 1];
        assert_eq!(
            ns.process(&big, &mut big_out),
            Err(NoiseError::BlockTooLong)
        );
    }

    #[test]
    fn s16_in_place_matches_the_unit_range_stream() {
        // Inputs that are exact in both scalings, so the two entry points
        // see the same PCM: the in-place 16-bit path must give the unit
        // path's output scaled by 32768, whatever the block size.
        let input: Vec<f32> = noise(FRAME_SIZE * 6 + 7, 0.25, 11)
            .iter()
            .map(|s| (s * 32_768.0).round() / 32_768.0)
            .collect();
        let reference = run(&input, 128);
        for block in [1, 160, 480, 481, 4096] {
            let mut ns = NoiseSuppressor::new();
            let mut samples: Vec<f32> = input.iter().map(|s| s * 32_768.0).collect();
            for chunk in samples.chunks_mut(block) {
                ns.process_s16_in_place(chunk).unwrap();
            }
            for (i, (got, want)) in samples.iter().zip(&reference).enumerate() {
                let want = (want * 32_768.0).clamp(-32_768.0, 32_767.0);
                assert!(
                    (got - want).abs() < 1e-3,
                    "block {block} sample {i}: {got} vs {want}"
                );
            }
        }
    }

    #[test]
    fn s16_in_place_stays_in_range_and_refuses_long_blocks() {
        let mut samples = noise(FRAME_SIZE * 4, 200_000.0, 5);
        samples[3] = f32::NAN;
        samples[900] = f32::NEG_INFINITY;
        let mut ns = NoiseSuppressor::new();
        for chunk in samples.chunks_mut(480) {
            ns.process_s16_in_place(chunk).unwrap();
        }
        assert!(samples
            .iter()
            .all(|s| s.is_finite() && (-32_768.0..=32_767.0).contains(s)));
        let mut big = vec![0.0; MAX_BLOCK + 1];
        assert_eq!(
            ns.process_s16_in_place(&mut big),
            Err(NoiseError::BlockTooLong)
        );
        assert!(big.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn voice_probability_is_in_range() {
        let mut ns = NoiseSuppressor::new();
        let input = noise(FRAME_SIZE * 3, 0.3, 9);
        let mut out = vec![0.0; input.len()];
        ns.process(&input, &mut out).unwrap();
        assert!((0.0..=1.0).contains(&ns.voice_probability()));
    }
}
