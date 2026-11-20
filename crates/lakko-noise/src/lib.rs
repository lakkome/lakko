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

mod resample;

use resample::RateAdapter;
pub use resample::{factor_of, MAX_FACTOR, SUPPORTED_RATES};

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
    /// The block is longer than [`MAX_BLOCK`] (at [`SAMPLE_RATE`]: a block
    /// at a lower rate may hold at most `MAX_BLOCK / factor` samples).
    BlockTooLong,
    /// The rate is not one of [`SUPPORTED_RATES`].
    UnsupportedRate,
}

impl std::fmt::Display for NoiseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LengthMismatch => f.write_str("input and output differ in length"),
            Self::BlockTooLong => f.write_str("block longer than MAX_BLOCK"),
            Self::UnsupportedRate => f.write_str("sample rate not supported"),
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
    /// The filters for every factor above 1, made once so a change of rate
    /// on the audio thread allocates nothing.
    adapters: Vec<RateAdapter>,
    /// The factor of the rate the last block came at (1 for 48 kHz).
    current_factor: usize,
    /// The raised stream of one block, at 48 kHz.
    high: Vec<f32>,
}

impl Default for NoiseSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

impl NoiseSuppressor {
    /// A fresh suppressor. Allocates every buffer it will ever use.
    pub fn new() -> Self {
        // nnnoiseless builds its shared tables (a process-wide `OnceCell`) on
        // the first frame any state processes: 54 allocations, about 32 KB,
        // counted 2026-11-10. One frame of silence through a throwaway state
        // builds them here, so the audio thread never does (in the web's
        // wasm it would grow the memory under the page's buffer views).
        DenoiseState::new().process_frame(&mut [0.0; FRAME_SIZE], &[0.0; FRAME_SIZE]);
        let mut ready = VecDeque::with_capacity(LATENCY_SAMPLES + MAX_BLOCK + FRAME_SIZE);
        ready.extend(std::iter::repeat(0.0).take(LATENCY_SAMPLES));
        Self {
            state: DenoiseState::new(),
            pending: Vec::with_capacity(FRAME_SIZE),
            ready,
            frame_out: vec![0.0; FRAME_SIZE],
            voice_probability: 0.0,
            adapters: SUPPORTED_RATES
                .iter()
                .filter(|(_, k)| *k > 1)
                .map(|(_, k)| RateAdapter::new(*k))
                .collect(),
            current_factor: 1,
            high: Vec::with_capacity(MAX_BLOCK),
        }
    }

    /// Whether [`Self::process_s16_in_place_at`] takes `rate`.
    #[must_use]
    pub fn supports_rate(rate: u32) -> bool {
        factor_of(rate).is_some()
    }

    /// Cleans `samples` in place like [`Self::process_s16_in_place`], for a
    /// stream at `rate`, one of [`SUPPORTED_RATES`]: a rate below 48 kHz is
    /// raised to 48 kHz, cleaned and lowered back (module `resample`). At
    /// most `MAX_BLOCK / factor` samples. The rate may change between calls
    /// (a capture stage that re-opens at another rate); the filters then
    /// start from silence. Adds the filters' delay to [`LATENCY_SAMPLES`].
    /// Allocates nothing.
    pub fn process_s16_in_place_at(
        &mut self,
        rate: u32,
        samples: &mut [f32],
    ) -> Result<(), NoiseError> {
        let factor = factor_of(rate).ok_or(NoiseError::UnsupportedRate)?;
        if samples.len() * factor > MAX_BLOCK {
            return Err(NoiseError::BlockTooLong);
        }
        if factor != self.current_factor {
            self.current_factor = factor;
            for adapter in &mut self.adapters {
                adapter.clear();
            }
        }
        if factor == 1 {
            return self.process_s16_in_place(samples);
        }
        let mut high = std::mem::take(&mut self.high);
        let result = match self.adapters.iter_mut().position(|a| a.factor() == factor) {
            Some(index) => {
                self.adapters[index].raise(samples, &mut high);
                let cleaned = self.process_s16_in_place(&mut high);
                if cleaned.is_ok() {
                    self.adapters[index].lower(&high, samples);
                    for sample in samples.iter_mut() {
                        *sample = if sample.is_finite() {
                            sample.clamp(-PCM_SCALE, PCM_MAX_S16)
                        } else {
                            0.0
                        };
                    }
                }
                cleaned
            },
            // Not reachable: every factor above 1 has an adapter.
            None => Err(NoiseError::UnsupportedRate),
        };
        self.high = high;
        result
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

    /// Low-passed noise in the 16-bit range, `secs` seconds at `rate`.
    fn hum_s16(rate: u32, secs: usize, seed: u32) -> Vec<f32> {
        let white = noise(rate as usize * secs, 0.1, seed);
        let mut acc = 0.0f32;
        white
            .iter()
            .map(|x| {
                acc = acc * 0.95 + x * 0.05;
                acc * 4.0 * PCM_SCALE
            })
            .collect()
    }

    #[test]
    fn stationary_noise_is_attenuated_at_a_lower_rate() {
        for rate in [16_000_u32, 8_000, 24_000, 12_000] {
            let input = hum_s16(rate, 2, 42);
            let mut ns = NoiseSuppressor::new();
            let mut out = input.clone();
            for block in out.chunks_mut(rate as usize / 100) {
                ns.process_s16_in_place_at(rate, block).unwrap();
            }
            let skip = rate as usize / 2;
            let rin = rms(&input[skip..]);
            let rout = rms(&out[skip..]);
            assert!(rout < rin * 0.3, "rate {rate}: rms in {rin} out {rout}");
            assert!(out
                .iter()
                .all(|x| x.is_finite() && (-PCM_SCALE..=PCM_MAX_S16).contains(x)));
        }
    }

    #[test]
    fn at_48k_the_rate_aware_call_is_the_plain_one() {
        let input = hum_s16(SAMPLE_RATE, 1, 7);
        let mut a = NoiseSuppressor::new();
        let mut b = NoiseSuppressor::new();
        let mut out_a = input.clone();
        let mut out_b = input.clone();
        for (x, y) in out_a.chunks_mut(480).zip(out_b.chunks_mut(480)) {
            a.process_s16_in_place(x).unwrap();
            b.process_s16_in_place_at(SAMPLE_RATE, y).unwrap();
        }
        assert_eq!(out_a, out_b);
    }

    #[test]
    fn refuses_an_unsupported_rate_and_a_long_block_at_a_lower_rate() {
        let mut ns = NoiseSuppressor::new();
        let mut block = vec![1.0; 441];
        let before = block.clone();
        assert_eq!(
            ns.process_s16_in_place_at(44_100, &mut block),
            Err(NoiseError::UnsupportedRate)
        );
        assert_eq!(block, before);
        assert!(!NoiseSuppressor::supports_rate(44_100));
        assert!(NoiseSuppressor::supports_rate(16_000));
        let mut long = vec![0.0; MAX_BLOCK / 3 + 1];
        assert_eq!(
            ns.process_s16_in_place_at(16_000, &mut long),
            Err(NoiseError::BlockTooLong)
        );
        let mut fits = vec![0.0; MAX_BLOCK / 3];
        assert_eq!(ns.process_s16_in_place_at(16_000, &mut fits), Ok(()));
    }

    #[test]
    fn a_change_of_rate_mid_stream_keeps_working() {
        // The Pixel 7a's capture stage opened at 16 kHz and moved to 48 kHz.
        let mut ns = NoiseSuppressor::new();
        let mut low = hum_s16(16_000, 1, 3);
        for block in low.chunks_mut(160) {
            ns.process_s16_in_place_at(16_000, block).unwrap();
        }
        let input = hum_s16(SAMPLE_RATE, 2, 5);
        let mut high = input.clone();
        for block in high.chunks_mut(480) {
            ns.process_s16_in_place_at(SAMPLE_RATE, block).unwrap();
        }
        let skip = SAMPLE_RATE as usize / 2;
        assert!(rms(&high[skip..]) < rms(&input[skip..]) * 0.1);
        for x in low.iter().chain(high.iter()) {
            assert!(x.is_finite() && (-PCM_SCALE..=PCM_MAX_S16).contains(x));
        }
    }
}
