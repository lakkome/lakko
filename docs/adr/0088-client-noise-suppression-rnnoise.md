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
  in Firefox, Safari or the desktop app. **Android built the same day**
  (section 4 as built): the capture post-processing hook of the pinned
  libwebrtc, `lakko_noise_*` C ABI in `crates/lakko-sframe-native`
  (`src/noise.rs`), JNI `noise_jni.cc` in `libsframe_jni.so`, the switch
  in the call Settings sheet; Rust tests and Android host unit tests only
  (a fake native suppressor); never run on a device, an emulator or a
  call, and nothing was heard. iOS is not built; on 2026-11-09 the
  maintainer chose section 5 option 2 (the webrtc-sdk binary and its
  capture post-processing delegate), built the same day, not yet run in a call.
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

### 4. Android (built, see 4a)

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

### 4a. Android as built (2026-11-07)

Read before building, from the webrtc-sdk `m125_release` sources the
pinned AAR is built from (`sdk/android/src/jni/pc/external_audio_processor.cc`,
`external_audio_processing_factory.cc`) and libwebrtc M125
`modules/audio_processing/audio_processing_impl.cc`:

- **Position.** The hook is the audio processing module's
  `capture_post_processor`, run in `ProcessCaptureStreamLocked` after echo
  cancellation (AEC3 or the mobile AECM), libwebrtc's noise suppressor and
  gain control, and after the bands are merged back to full band. On phones
  whose platform AEC / NS effects run (the Pixel 7a of the test call), those
  ran earlier still, on the `AudioRecord`. So the order of section 1 holds:
  platform processing, then RNNoise, then the encoder and SFrame.
- **What `process(numBands, numFrames, buffer)` hands over.** A direct
  `ByteBuffer` (new each call, made by the SDK glue) over channel 0 of
  libwebrtc's capture buffer: `numFrames` floats (10 ms at the stage's rate),
  full band, in the range of 16-bit PCM ("FloatS16"), native byte order. It
  is called under the adapter's own mutex, on libwebrtc's audio thread;
  `initialize(rate, channels)` and `reset(rate)` are called under the same
  mutex when the stage's format changes.
- **Rate.** The stage runs at the capture rate rounded up to 16, 32 or
  48 kHz (`SuitableProcessRate`, maximum 48 kHz unless the pipeline caps it
  at 32 kHz, which the voice engine does not). A phone recording at 48 kHz
  therefore gets 48 kHz. **At any other rate, or with more than one channel,
  the buffer is passed through untouched; there is no resampling.**
  (Amended 2026-11-10, see the amendment below: a mono stage at 24, 16, 12
  or 8 kHz is now resampled; other rates and more than one channel still
  pass through.) The
  format line in the log says which (`capture post-processing format ... Hz,
  ... ch: RNNoise applies when the switch is on` or `... bypassed`).

What was built:

- `lakko_noise::NoiseSuppressor::process_s16_in_place` cleans FloatS16
  samples in place, with the same constant one-frame delay and block-size
  independence as `process` (tested against it), allocating nothing.
- **Why a hand-written JNI path and not UniFFI.** Cleaning the buffer in
  place needs its address (`GetDirectBufferAddress`), which only a JNI
  function can take; UniFFI would copy the 480 samples into a fresh buffer
  every 10 ms. The C ABI (`lakko_noise_new`, `lakko_noise_free`,
  `lakko_noise_process_s16_in_place`, `lakko_noise_voice_probability`) lives
  in `crates/lakko-sframe-native` because `libsframe_jni.so` already links
  that crate's static archive and already carries JNI entry points for the
  real-time media threads; a second Rust static archive cannot be linked
  into the same shared library (both carry the Rust standard library). iOS
  links the same archive, so the functions are there for an iOS hook.
  `apps/android/lakko-sframe/src/main/cpp/noise_jni.cc` (built by
  `tools/build-sframe-jni.sh`) and `me.lakko.sframe.NativeNoiseSuppressor`
  are the JNI pair; the process call checks the buffer is direct, long
  enough and aligned, else touches nothing.
- `me.lakko.app.calls.CaptureNoiseSuppression` builds the process-wide
  `PeerConnectionFactory` with an `ExternalAudioProcessingFactory` whose
  capture post-processing is `CaptureNoiseProcessor`, once per process. When
  `libsframe_jni.so` has no noise functions the hook is not installed and
  the factory is built exactly as before. The SDK's `destroy()` is never
  called (it deletes the process-wide adapter without clearing the pointer)
  and `setBypassFlagForCapturePost` is not used (it takes the mutex the audio
  thread holds).
- `CaptureNoiseProcessor.process` allocates nothing, takes no lock and never
  throws (a throw would abort inside libwebrtc's JNI glue). A fresh native
  suppressor is made on a control thread for every call (when its
  microphone track is added) and every switch-on, so none of the
  suppressor's delay line or overlap memory from an earlier call or
  stretch reaches a new one; an old one is freed only after the audio thread
  has been seen outside `process` (a counter it raises around its use),
  else left allocated with one log line. The switch is a volatile the audio
  thread reads per buffer, so it applies at once mid-call.
- **Failure:** a refused buffer (or a throw) leaves that buffer untouched,
  stops the suppressor for the rest of the call and logs one line. With the
  switch off the stage still runs but returns at once; libwebrtc's audio
  processing module is the one the SDK builds with the default builder plus
  this stage.
- **Logs** (tag `CaptureNoise`): whether the hook was installed; per call,
  which processing applies with the platform AEC / NS kind
  (`JavaAudioDeviceModule.isBuiltIn*Supported`); the stage's format; every
  switch change; at the call's end how many buffers were cleaned and how
  many passed through.
- The switch: `NoiseSuppressionSetting`, its own `SharedPreferences` file
  (`lakko_call_audio`, key `noise_suppression`), on by default, this device
  only; the `Sound` section heads the call Settings sheet, shown even with
  no device list.
- **Not verified:** that the pinned AAR's binary behaves as the m125_release
  source read (the symbols match; the code was not disassembled), that the
  stage runs at 48 kHz on a real phone, CPU, and how it sounds. The SDK glue
  makes a `ByteBuffer` object per 10 ms whatever Lakko does.

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

**Decided 2026-11-09 (maintainer): option 2.** Checked that day in the
webrtc-sdk sources (branch `m125_release`): the iOS build exposes the same
stage as the Android AAR, `RTCDefaultAudioProcessingModule` with a
`capturePostProcessingDelegate` conforming to
`RTCAudioCustomProcessingDelegate` (`audioProcessingInitialize`,
`audioProcessingProcess(audioBuffer:)`, `audioProcessingRelease`), passed to
`RTCPeerConnectionFactory initWithBypassVoiceProcessing:encoderFactory:decoderFactory:audioProcessingModule:`;
its Swift package is `webrtc-sdk/Specs`, tag `125.6422.07`, the version the
Android AAR pins. iOS moves from stasel/WebRTC 125.0.0 to that binary and
runs `lakko_noise_*` in the capture post-processing delegate, after Apple's
voice processing, as Android does. The move is gated on re-checking, against
the new binary's machine code, every assumption the native shim makes about
the old one: the frame encryptor and decryptor attach (security invariant
#4) and the receiver playout-delay slots of
`apps/shared/sframe-bridge/receiver-delay.h`, which stay observe-only for an
image they were not read from. Option 1 was rejected as the costliest and
riskiest; option 3 as leaving iOS the one client without the switch.

**Built 2026-11-09 on iOS** (`Calls/CaptureNoiseProcessor.{h,mm}`,
`Calls/CaptureNoiseSuppression.swift`, the `Sound` section of the call
Settings sheet). Checked against the new binary's machine code, all three
slices: the frame encryptor and decryptor attach is instruction-identical to
stasel 125.0.0's, and the playout-delay slots, slot encoding and argument
convention are the same; the three new image identities are pinned in
`receiver-delay.h` / `SFrameAttach.mm`. Run: LakkoTests on a simulator (the
setter offered on the simulator arm64 slice); the device app installed and
launched on an iPhone with no crash. Not yet run in a call: neither the
encryptor on this binary, nor the setter on a real receiver, nor the
suppressor heard.

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

## Amendment 2026-11-10: a capture stage below 48 kHz is resampled

Seen on devices: the Pixel 7a's capture post-processing stage opened at
16 kHz mono before its microphone settled at 48 kHz, and the iOS Simulator
captures at 16 kHz for the whole call. Both were passed through untouched,
so on such a stream RNNoise never ran. What a real iPhone's voice-processing
I/O runs at is not measured.

Decision (the maintainer's, 2026-11-10): the suppressor takes a mono stream
at 48 kHz or at a rate 48 kHz divides by a whole factor (24, 16, 12, 8 kHz).
A lower rate is raised to 48 kHz (zeros between samples, then a low-pass
filter), cleaned by RNNoise, filtered again and lowered back by keeping one
sample in each factor; `lakko_noise::NoiseSuppressor::process_s16_in_place_at`
and the C ABI's `lakko_noise_process_s16_in_place_at_rate` /
`lakko_noise_supports_rate` (status `UNSUPPORTED_RATE = 4`). The rate is
passed with every block, so a stage that re-opens at another rate mid-call
keeps working; the filters then start from silence. The filter is a
windowed-sinc low-pass (Blackman window), 16 taps per unit of the factor
plus one, corner at 0.45 of the lower rate: chosen, not measured. It adds its
group delay to RNNoise's 10 ms (two filters of `16 * factor / 2` samples at
48 kHz each, for example 1 ms at 16 kHz). Allocation-free on the audio
thread: the filters for every factor are made when the suppressor is.

Every other rate (44.1 or 32 kHz, for example) and more than one channel is
still passed through, and the log line says so. Host tests: a tone inside
the band survives a raise and a lower at its level within 3 percent, a tone
above the band is stopped, low-passed noise is attenuated at each lower rate,
and a 16 to 48 kHz change mid-stream keeps working. How RNNoise treats a
voice that reaches it band-limited by a 16 or 8 kHz capture is not measured.

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
6. **Android hook rate and position**: read in source (section 4a), not
   observed on a phone; the call log's format line answers it on the first
   call.

## References

- xiph/rnnoise, `COPYING`, `include/rnnoise.h`
- `nnnoiseless` 0.5.2 (crates.io), `COPYING`, `README.md`
- W3C Web Audio API: `AudioWorklet`, `MediaStreamAudioSourceNode`,
  `MediaStreamAudioDestinationNode`
- libwebrtc M125 `media/engine/webrtc_voice_engine.cc`,
  `modules/audio_processing/include/audio_processing.h`
