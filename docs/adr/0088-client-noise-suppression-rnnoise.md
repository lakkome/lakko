<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0088: Client-side noise suppression with RNNoise

- **Status:** Proposed (2026-11-07). The maintainer decided the feature and
  the switch (on by default) the same day; this record proposes how. Built
  the same day on web and desktop only (`crates/lakko-noise`,
  `crates/lakko-noise-wasm`, `apps/web/src/calls/noise-*.ts`), host-tested
  (Rust tests; web unit tests that drive the real `.wasm` under Node). The
  production build's worklet and `.wasm` were also loaded once in the
  Chromium of the Claude desktop browser pane with a synthetic noise source
  (no microphone, no call): the worklet reported ready and the noise RMS fell
  from about 0.035 to about 0.0003. Never run with a microphone, on a call,
  in Firefox, Safari or the desktop app. iOS and Android are not built.
- **Date:** 2026-11-07
- **Context:** the call test of 2026-11-07 (typing and room noise reached
  every participant), the investigation behind it (call-audio report of the
  same day, summarised below), UX spec
  [`docs/ux/calls.md`](../ux/calls.md) section 4.1.3, the SFrame media path
  ([ADR-0013](0013-sframe-cross-platform-aad.md),
  [ADR-0035](0035-sframe-per-stream-kid-key.md),
  [ADR-0071](0071-codec-header-in-the-clear.md)), the desktop client
  ([ADR-0085](0085-desktop-client.md)), licensing
  ([ADR-0006](0006-licensing.md)).

## Context

Every client today relies on the platform for noise reduction, and the
platforms differ:

- **Web and desktop** ask `getUserMedia` for `echoCancellation`,
  `noiseSuppression` and `autoGainControl`
  (`apps/web/src/calls/call-session.ts` `audioTrackConstraint`). Which
  suppressor each engine then runs, how strong, and whether it touches
  keyboard transients is decided inside Chrome, Firefox and WebKit, and was
  not read. A mid-call microphone switch asked for none of the three
  (`sfu-call-session.ts` and `call-session.ts` `switchInput`), relying on
  browser defaults.
- **iOS** uses Apple's voice-processing audio unit; libwebrtc's own noise
  suppressor is off there by its engine rule (M125
  `webrtc_voice_engine.cc`).
- **Android** uses the phone's platform effects when the phone has them
  (seen in the Pixel 7a's logcat of the test call: `HW NS will be used`),
  else libwebrtc's software suppressor.
- libwebrtc's transient (keyboard) suppressor is compiled into both native
  binaries and switched on by nothing (`AudioProcessing::Config::
  TransientSuppression` defaults to off, and the voice engine never sets
  it); neither native SDK binding lets Lakko switch it on.

So the same keyboard sounds different, or is not removed at all, depending
on which client a person uses, and Lakko controls none of it. The project
rule for calls is that Lakko's own code decides and the browser or OS
executes. The maintainer chose RNNoise, on every client, behind one switch
that is on by default (UX spec section 4.1.3).

## Decision

Each client runs **one noise suppressor, RNNoise, compiled from Lakko's own
tree**, on the microphone signal **after the platform's echo canceller and
before the encoder**, behind a per-device switch that is on by default; when
it cannot start, the call sends the microphone as it does today.

### 1. Where it runs, and what it does not touch

```
microphone -> platform capture (AEC, platform NS, AGC) -> RNNoise -> encoder -> SFrame seal -> SFU
```

- The platform echo canceller stays on and stays first. RNNoise is not an
  echo canceller and does not replace one; it is added after it, so the
  canceller still sees the raw microphone and its own playback reference.
- The platform's own noise suppressor stays on too (no constraint changes
  apart from the mid-call switch fix below). Turning it off is a later
  measurement question (open question 2), not part of this decision.
- **Invariant #2 and #4 are unaffected.** The suppressor runs on the
  sender's own device, on its own plaintext PCM, before the encoder and
  therefore before SFrame. It adds nothing to the wire: no field, no
  header, no signalling. The SFU sees the same sealed frames. A suppressor
  that fails is a quality fallback (the platform-processed microphone is
  sent), never a downgrade of encryption: every frame is still sealed.
- The setting is per device and is never sent to anyone (UX section 4.1.3).

### 2. The implementation: `nnnoiseless`, one Rust crate, every client

- **Source.** RNNoise exists as xiph's C library (`xiph/rnnoise`,
  BSD-style licence in its `COPYING`: Xiph.Org Foundation, Jean-Marc Valin,
  Mozilla, Amazon) and as `nnnoiseless`, a pure-Rust port of it by Joe
  Neeman (crates.io `nnnoiseless` 0.5.2, BSD-3-Clause; its `COPYING` carries
  the Xiph.Org, Valin and Mozilla notices plus the port's own). Lakko uses
  **`nnnoiseless` 0.5.2, pinned exactly** (`=0.5.2` in the workspace,
  checksum in `Cargo.lock`), with its default features off (they are a CLI:
  clap, hound, dasp). What it pulls in: `easyfft` -> `rustfft` /
  `realfft`, `num-complex`, `once_cell`, `generic_singleton`, `anymap3`,
  `parking_lot`; `cargo deny check licenses` passes with them.
- **Why the port and not the C library.** Lakko's clients already share
  one Rust core compiled for wasm, iOS and Android (ADR-0002). The port
  goes through exactly that pipeline: no C toolchain, no emscripten, no
  second build system per platform. The C library would need emscripten for
  the web (not installed in the project's toolchain) and a separate C
  cross-build for each native target.
- **What the port costs.** It carries RNNoise's original model (the one
  shipped with RNNoise 0.1), not the newer model xiph published with
  RNNoise 0.2. Which of the two removes keyboard noise better was not
  measured (open question 1). The crate uses `unsafe` in two places for an
  `f32` to complex reinterpretation (its README); it is third-party code
  outside the `forbid(unsafe_code)` posture, like the other dependencies.
  Building the C library in-tree remains the fallback if the port's model
  proves too weak (alternative 2).
- **`crates/lakko-noise`** (`#![forbid(unsafe_code)]`): `NoiseSuppressor`
  wraps `nnnoiseless::DenoiseState`. RNNoise works on 480-sample frames at
  48 kHz; hosts deliver audio in other block sizes (128 in a web
  `AudioWorklet`), so the crate buffers with a **constant delay of one
  frame** (480 samples, 10 ms at 48 kHz) and an output that does not depend
  on how the input was cut into blocks (tested over block sizes 1 to
  4096). Samples are `f32` in `[-1, 1]`; non-finite input is silence;
  output is finite and bounded. All buffers are allocated once, so the
  audio thread never allocates. `SAMPLE_RATE` is 48 000 and a host at any
  other rate must not run it (the model is trained at 48 kHz only).
- **`crates/lakko-noise-wasm`**: a `cdylib` with raw C-ABI exports
  (`lakko_ns_init`, `lakko_ns_input_ptr`, `lakko_ns_output_ptr`,
  `lakko_ns_process(n)`, `lakko_ns_voice_probability`, and three constant
  getters) built for `wasm32-unknown-unknown`. It imports nothing, so a
  worklet can instantiate it with an empty import object. One wasm instance
  is one suppressor (state in a per-instance global). Its only `unsafe` is
  the `#[no_mangle]` attribute the exports need (the `unsafe_code` lint
  flags it); it dereferences no raw pointer. **This makes it a third crate
  outside `#![forbid(unsafe_code)]`** beside `lakko-ffi-uniffi` and
  `lakko-ffi-wasm`; the CLAUDE.md code-style rule names only those two and
  needs the maintainer's edit if this ADR is accepted. The release `.wasm`
  measured 450 357 bytes when first built (2026-11-07).
- **Why not `lakko-ffi-wasm`.** An `AudioWorkletGlobalScope` has no `fetch`
  and, in some engines, no `TextDecoder`; the main module's wasm-bindgen
  glue imports host functions a worklet cannot provide, and it is several
  megabytes. A separate, import-free module is the shape a worklet can load.
- **Why not an npm package.** Two were read:
  `@jitsi/rnnoise-wasm` 0.2.1 (Apache-2.0 `LICENSE` file, no licence field
  in its `package.json`; ships a prebuilt emscripten `rnnoise.wasm` built
  from a git submodule of xiph/rnnoise by its `build.sh`) and
  `@shiguredo/rnnoise-wasm` 2025.1.5 (Apache-2.0; ships one 4.8 MB
  JavaScript file with the wasm inlined). Both are prebuilt binaries Lakko
  would take on trust, rebuilt only with emscripten, and usable on the web
  only, so the three clients would run three different builds. Neither
  `LICENSE` reproduces the xiph notice the BSD terms ask for.

### 3. Web and desktop

The desktop app runs the web client's code (ADR-0085), so one
implementation serves both.

```
getUserMedia track (raw, AEC/NS/AGC on)
  -> AudioContext({ sampleRate: 48000 })
  -> MediaStreamAudioSourceNode
  -> AudioWorkletNode "lakko-noise" (lakko_noise.wasm inside)
  -> MediaStreamAudioDestinationNode (mono)
  -> processed track -> RTCRtpSender.replaceTrack -> encoder -> SFrame transform
```

- The page fetches `lakko_noise.wasm` (a same-origin asset, built by
  `npm run build:noise` into `apps/web/wasm/`), hands the bytes to the
  worklet in `processorOptions`, and the worklet compiles and instantiates
  it synchronously. The JavaScript side of the exchange
  (`noise-kernel.ts`) is shared by the worklet and the unit test, which
  drives the real `.wasm` under Node.
- **Publish first, swap after.** The call always publishes the raw track
  first, so building the audio graph never delays joining; once the graph
  runs, the sender's track is replaced with the processed one. The SFrame
  transform lives on the sender, so `replaceTrack` keeps it and needs no
  renegotiation. Turning the switch off replaces it back with the raw
  track: off means nothing of Lakko's in the path. A microphone switch
  mid-call rebuilds the graph on the new device's track.
- **Fallback.** If anything fails (no `AudioWorklet`, the wasm does not
  load or compile, the context cannot run at 48 kHz, the context will not
  start or is suspended later, the worklet reports an error), the sender
  keeps or gets back the raw track, one line is logged, and nothing is shown
  (UX section 4.1.3). A suspended context would otherwise send silence,
  which is why suspension swaps back to the raw track.
- **The log line**, once per capture, says which processing is applied and
  what the browser reports for the raw track:
  `track.getSettings()` `echoCancellation`, `noiseSuppression`,
  `autoGainControl`.
- **Content Security Policy.** The worklet module is a same-origin script
  (`script-src 'self'`), compiling wasm in it needs `'wasm-unsafe-eval'`,
  and fetching the `.wasm` needs `connect-src 'self'`; production and test
  stack nginx and the built page's meta policy already carry all three.
  No `Permissions-Policy` feature gates an audio worklet.
- **The mid-call microphone switch** now asks for the same three
  processing flags as the first capture, in both call sessions.

### 4. Android (not built)

The pinned AAR (`io.github.webrtc-sdk:android:125.6422.07`) has two hooks
on the captured signal (read with `javap` in the call-audio report):

- `PeerConnectionFactory.Builder.setAudioProcessingFactory(...)` with
  `ExternalAudioProcessingFactory.setCapturePostProcessing(AudioProcessing)`
  (`initialize(sampleRate, channels)`, `reset(sampleRate)`,
  `process(bands, frames, ByteBuffer)`), inside libwebrtc's audio
  processing. Its position relative to libwebrtc's own stages was not read
  in source.
- `JavaAudioDeviceModule.Builder.setAudioBufferCallback(...)`, called in
  the record thread after the platform AEC / NS effects and before the
  buffer reaches native code.

The proposal is the first: a JNI / UniFFI call into `lakko-noise` from the
post-processing hook, resampling being unnecessary when libwebrtc runs the
hook at 48 kHz (to be confirmed; a hook at another rate falls back to no
suppression rather than resampling, until measured). The switch toggles a
flag the hook reads per buffer, so it applies at once. Fallback: the hook
passes the buffer through unchanged.

### 5. iOS (not built), and its cost

The stasel/WebRTC 125.0.0 binary exposes no capture post-processing hook:
its audio headers are only `RTCAudioDevice`, `RTCAudioSession`,
`RTCAudioSessionConfiguration`, `RTCAudioSource`, `RTCAudioTrack`. Options,
none chosen here:

1. **A custom `RTCAudioDevice`** passed to
   `RTCPeerConnectionFactory initWithEncoderFactory:decoderFactory:audioDevice:`.
   Lakko would then own the audio unit, and so would have to run Apple's
   voice processing (echo cancellation) itself before `lakko-noise`, and
   take over playout, route changes, interruptions and CallKit activation
   that the stock module handles today. This is the largest cost in this
   ADR and the riskiest: an echo-cancellation regression on iPhone is worse
   than noise.
2. **A WebRTC build with a capture post-processing hook** (the Android
   AAR's hook is a webrtc-sdk addition). Which iOS builds expose one, and
   what moving off the pinned binary costs (the SFrame shim's ABI tables in
   `receiver-delay.h` are read per binary), was not checked.
3. Leave iOS on Apple's voice processing and hide the switch there until
   one of the above lands (the UX spec's rule for a client that cannot run
   the suppressor).

The recommendation is to ship web, desktop and Android first and to decide
iOS after the web build has been heard on real calls, since that is also
when the model question (open question 1) is answered.

## Alternatives

1. **Keep the platform suppressors only.** Rejected by the maintainer:
   behaviour differs per client, and keyboard noise reached every
   participant in the test call.
2. **xiph/rnnoise C, built in-tree** (emscripten for the web, a C
   cross-build per native target). Newer model available; two extra
   toolchains and three builds of one library. Kept as the fallback if the
   port's model is too weak.
3. **A prebuilt npm package** (`@jitsi/rnnoise-wasm`,
   `@shiguredo/rnnoise-wasm`): web only, prebuilt binaries, see section 2.
4. **DeepFilterNet** (code and weights MIT / Apache-2.0): reportedly
   stronger and heavier; no cost figure was read. A later swap behind the
   same `lakko-noise` interface if RNNoise is not good enough.
5. **libwebrtc's transient suppressor.** Aimed exactly at keyboard clicks,
   but neither native binding exposes the switch, and it does not exist on
   the web at all.
6. **The browser's `voiceIsolation` constraint** (Chrome, experimental):
   one browser, and the browser decides, against the project rule.

## Consequences

- **Positive:** one suppressor, one model and one implementation on every
  client that runs it; Lakko decides, the platform executes; no wire, server
  or E2EE change.
- **Negative:** one frame (10 ms at 48 kHz) of added latency on the sending
  side, plus the web audio graph's own buffering (not measured); CPU on the
  sender's device (not measured); a third crate outside
  `forbid(unsafe_code)`; a 450 KB asset in the web build; the web path
  depends on `AudioWorklet` and on an `AudioContext` that runs, which the
  fallback covers but which means a person on an engine without them gets
  no suppression, silently.
- **Neutral:** RNNoise may also attenuate some wanted sounds (music held up
  to the microphone); the switch exists for that.

## Open questions (to be measured, not claimed)

1. **Quality on real recordings:** keyboard clicks, fans, street noise,
   speech under noise; RNNoise 0.1 model (the port) against xiph's 0.2
   model and against each platform's own suppressor. Needs recordings; no
   figure is stated here. One host-test observation only: low-passed noise
   is strongly attenuated, flat full-band white noise from the test's
   generator came out only about 10 percent quieter
   (`lakko-noise` test comment).
2. **Whether the platform suppressor should be turned off** when RNNoise
   runs (two suppressors in series may over-suppress or add artifacts).
3. **CPU and battery** on a low-end phone and in each browser engine.
4. **Added latency** end to end, worklet and context buffering included.
5. **WebKit / WKWebView:** whether `AudioWorklet` is available in the macOS
   desktop app's web view and in Safari on iOS with a live capture, and
   whether `AudioContext({ sampleRate: 48000 })` with a microphone source is
   accepted in each engine (Firefox refuses a source at another rate; the
   fallback covers it).
6. **Android hook rate and position** (section 4).

## References

- xiph/rnnoise, `COPYING`, `include/rnnoise.h`
- `nnnoiseless` 0.5.2 (crates.io), `COPYING`, `README.md`
- W3C Web Audio API: `AudioWorklet`, `MediaStreamAudioSourceNode`,
  `MediaStreamAudioDestinationNode`
- libwebrtc M125 `media/engine/webrtc_voice_engine.cc`,
  `modules/audio_processing/include/audio_processing.h`
