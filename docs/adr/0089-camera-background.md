<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0089: Camera background in calls, one segmentation model run by Lakko's own code

- **Status:** Accepted (2026-11-08). The maintainer decided the feature (blur,
  built-in backgrounds, own photo), that the choice lives on the device, and
  that one model run by Lakko's own code is tried on every client before any
  platform engine (2026-11-07), and accepted this record (2026-11-08).
- **Date:** 2026-11-07
- **Context:** requests from people in video calls; UX spec
  [`docs/ux/call-backgrounds.md`](../ux/call-backgrounds.md) and
  [`docs/ux/calls.md`](../ux/calls.md) section 4.1.5; the camera copies of
  [ADR-0070](0070-call-media-through-a-loss-burst.md); the SFrame media path
  ([ADR-0035](0035-sframe-per-stream-kid-key.md),
  [ADR-0071](0071-codec-header-in-the-clear.md)); the pattern of
  [ADR-0088](0088-client-noise-suppression-rnnoise.md) (one Rust
  implementation, wasm for web, C ABI for native); the account store gate
  ([ADR-0077](0077-account-binding-and-store-gate.md)); the desktop client
  ([ADR-0085](0085-desktop-client.md)); licensing
  ([ADR-0006](0006-licensing.md)).

## Context

No client can change what is behind the person on camera. Doing it needs, per
frame: a mask of where the person is (segmentation), then a composite of the
person over a blurred copy of the frame or over a picture.

**Where the frames are.** Every client already has one point between the
camera and the call where every frame passes before it is copied:

- iOS: `CapturerFan` (`apps/ios/Lakko/Calls/CapturerFan.swift`) hands each
  capturer frame to one `RTCVideoSource` per camera copy (main, middle, light).
- Android: `CapturerFan.kt`, the same shape over `CapturerObserver`.
- Web and desktop: the camera `MediaStreamTrack` from `getUserMedia`, which the
  session clones once per copy (`sfu-call-session.ts` `followCameraWith`); the
  track is acquired in several places (first capture, device switch, camera on
  mid-call) in both `sfu-call-session.ts` and `call-session.ts`.

All of them sit before the frame encryption, so a processed frame is sealed
exactly like a camera frame is today.

**Engines.** The maintainer's rule for calls is that Lakko's own code decides
and the platform executes, and the platform engines differ: Apple Vision on
iOS, Google ML Kit on Android (its terms say it sends usage metrics to Google),
nothing common on the web. Google's MediaPipe Tasks libraries (web, Android,
iOS) are the obvious runtime and are excluded: their privacy notice says the
Tasks APIs send performance and utilisation metrics to Google, and no opt-out is
documented ([mediapipe issue 6291](https://github.com/google-ai-edge/mediapipe/issues/6291)).
Only the model weights are a candidate.

**The model.** Google's MediaPipe selfie segmenter, two variants: square
256x256 and landscape 144x256 input, RGB in [0, 1], one output channel, a person
probability in [0, 1]. The model card
(`storage.googleapis.com/mediapipe-assets/Model Card MediaPipe Selfie Segmentation.pdf`,
dated 2021-05-06, read in full in the spike below) states Apache License 2.0 and
covers both variants.

**The spike (2026-11-07, scratch code outside the repository).**

- `tract-tflite` 0.23.10 cannot load Google's `.tflite` files: they use the
  MediaPipe custom operator `Convolution2DTransposeBias`.
- `tract-onnx` 0.23.10 loads the ONNX conversion published by `onnx-community`
  on Hugging Face (462 352 / 462 338 bytes, tagged Apache-2.0; a third-party
  conversion, not checked against Google's weights beyond the card and tag),
  with `with_ignore_value_info(true)` and `with_ignore_output_shapes(true)` and
  a fixed `[1, 3, H, W]` input. Load and optimise took 36-39 ms natively and
  151 ms in wasm.
- Inference only, one thread, Apple M3 Pro, 30 warm-up and 300 timed runs:

  | | 256x256 median / p90 | 144x256 median / p90 |
  |---|---|---|
  | Native, release | 6.4-6.6 / 6.5-6.7 ms | 3.55-3.57 / 3.58-3.62 ms |
  | wasm32 under Node 25 (V8), `+simd128` | 9.4-9.5 / 9.6-9.7 ms | 4.86-4.93 / 4.99-5.02 ms |
  | wasm32 under Node 25, no SIMD | about 23-24 ms | about 12-13 ms |

- Native and both wasm builds gave the same output on the same input. On a
  public-domain portrait the masks were meaningful (head, torso and arms solid,
  the studio background black, soft edges).
- The wasm module with the whole `tract-onnx` operator set is about 19.7 MB raw,
  about 4.7 MB with `gzip -9`. With a `getrandom` custom backend that returns
  "unsupported" it has no imports.
- Not measured: any phone, any real browser, preprocessing and compositing
  (only inference was timed), threading.

## Decision

### 1. One model, one runtime, written once in Rust

- **Model:** the MediaPipe selfie segmenter, **landscape** variant (144x256),
  since a call camera is 16:9 and it is the faster one. The repository carries
  the ONNX file, pinned by SHA-256, with a `.license` file naming Apache-2.0 and
  the model card. A conversion of Google's own `.tflite` by a pinned script in
  `tools/` replaces the third-party file before release (open question 1).
- **Runtime:** `tract-onnx` (MIT / Apache-2.0, pure Rust), in a new crate
  **`crates/lakko-segment`** (`#![forbid(unsafe_code)]`). It owns everything
  that is not pixels on a GPU:
  - loading and optimising the bundled model once per process;
  - `segment(rgb, width, height) -> mask`: the caller hands a small RGB image
    (144x256, or the square size if chosen later), the crate returns a mask of
    the same size in `u8`;
  - mask smoothing across frames (an exponential average per pixel, so the edge
    does not flicker), and the thresholds that turn probability into alpha;
    constants chosen, not measured;
  - **the frame gate** (section 3), the one rule for what may leave the device.
- **Web and desktop:** `crates/lakko-segment-wasm`, import-free like
  `lakko-noise-wasm`, built to `apps/web/wasm/lakko_segment.wasm`, run in a
  worker. Built with `+simd128`; a browser without wasm SIMD gets no background
  (the section is absent, UX section 3) rather than a slow one.
- **iOS and Android:** a C ABI `lakko_segment_*` in `crates/lakko-sframe-native`
  (as `lakko_noise_*` is), so no new native library is added to either client.

### 2. Compositing on each platform's GPU

The blur and the blend touch every pixel of a 1280x720 frame and stay on the
GPU, per platform: WebGL2 on web and desktop, Core Image or Metal on iOS,
OpenGL ES on Android. Each platform also makes the small RGB input for the
model from the frame. The compositing is two shaders (a separable blur, a
mask-weighted blend of the frame over the blurred frame or the picture) and is
the one piece written four times; its inputs (mask, blur strength for `Blur` and
`Strong blur`, picture fit) come from `lakko-segment` so the four agree on what
is drawn.

The processed frame replaces the camera frame **before** the copies: on iOS and
Android inside `CapturerFan`, so the main, middle and light sources all receive
processed frames; on web and desktop a processed track is made once and the
session clones it instead of the camera track. A shared screen is never
processed.

### 3. The frame gate: never the raw camera while a background is selected

The UX spec's security-relevant rule, written once in `lakko-segment` and
exposed on both FFI surfaces:

| Selection | Processor state | Frame |
|---|---|---|
| `None` | any | the camera frame, unprocessed |
| a background | `Starting` | **dropped**, nothing is sent |
| a background | `Working` | the processed frame |
| a background | `Failed` | **dropped**, and the client turns the camera off and shows the failure line |

`Failed` is entered when the model cannot load, the processing throws, or
processing stays over budget (a frame interval) for a sustained window; window
and budget are constants in the crate, chosen, not measured. A late mask is not
a failure: the last mask is reused while a new one is computed, so inference can
run below the camera's frame rate.

Each platform's pipeline asks the gate per frame. A test on each client drives
the pipeline with a failing processor and asserts that no unprocessed frame
reaches a sender.

### 4. Web and desktop pipeline

Browser-independent, two ways chosen by feature detection, like
`sframe-transport.ts`:

- where `MediaStreamTrackProcessor` and a track generator exist, frames are read
  and written in a worker (no main-thread timing, survives a hidden tab);
- otherwise a `<video>` of the camera drawn into a WebGL2 canvas, published with
  `canvas.captureStream()`.

Which engines take which way, and whether the canvas way keeps running in a
hidden tab, is measured in Chrome, Firefox, Safari and the desktop app before
the web build is called working (open question 3). The wasm module is loaded
only when a background is first selected, which is the UX's `Preparing
background...` state.

### 5. What is stored

- The choice: a per-device setting (`none`, `blur`, `strong-blur`,
  `builtin:<id>`, `photo:<local id>`), not synced, never sent.
- Own photos: up to twelve, longer side at most 1920 px, behind the ADR-0077
  store gate of the account (web IndexedDB, iOS and Android app files), wiped
  with the account on the device.
- Built-in backgrounds: six pictures drawn for Lakko, shipped in each client.

Nothing goes on the wire, nothing reaches a server.

## As built (core, 2026-11-08)

Host-tested only; no client uses it yet at the time of writing.

- `crates/lakko-segment`: the model behind a default `model` feature (so
  `lakko-ffi-uniffi` and `lakko-ffi-wasm` link no `tract`), `Segmenter`
  (RGBA, BGRA or RGB input, `u8` mask), `mask` (per-pixel average, weight
  0.6; alpha ramp from probability 0.3 to 0.7), `gate` (`Choice`,
  `HealthWatch`, `frame_action`) and `render`. Constants, all chosen, not
  measured: first mask within 15 s, a stall of 2 s while frames arrive, an
  average above 250 ms over 30 masks; blur at a quarter size with sigma
  1.2 % (`Blur`) and 3.6 % (`Strong blur`) of the frame height.
- **Beyond this record:** an unreadable stored choice reads as `Blur`, not
  `None`, so a corrupt setting still hides the room; and the six built-in
  backgrounds are drawn by code in `render` (soft shapes over a gradient in
  the design system's palette, a fixed dither) instead of shipping image files
  per client, so the four clients show the same pictures.
- `crates/lakko-segment-wasm` and `apps/web/scripts/build-segment-wasm.mjs`
  (its own target directory, `+simd128`, a refusing `getrandom` backend): no
  imports, 4 283 142 bytes gzipped; in Node 25 on the M3 Pro the model loaded
  in 154 ms and a frame (RGBA conversion and inference) took about 5.3 ms.
- C ABI `lakko_segment_*` in `lakko-sframe-native` (`src/segment.rs`): the
  model and the health watch behind separate locks, so judging a frame on the
  capture thread never waits for an inference; a buffer of the wrong size
  fails the watch.
- Not per frame, on UniFFI and wasm-bindgen (`background.rs` in both FFI
  crates): the choice, the built-in ids and pictures, blur parameters, the
  cover rect, and `model_input_rect`: where a whole upright frame is drawn
  inside the 256x144 input, shape kept, centred, black bars where the shapes
  differ, and the mask read back through the same rectangle. Added after the
  first client builds disagreed (two stretched a portrait frame into the
  landscape input, one letterboxed it); a phone held upright therefore gives a
  person about 81 px wide in the model input, and how well that segments is
  open question 5.
- 21 catalog strings (`call.settings.background*`, `call.background.*`),
  machine translations in the ten other languages.

## As built (clients, 2026-11-08)

- **Web and desktop** (`apps/web/src/calls/background-*.ts`,
  `CallBackgroundSection.tsx`): frame streams in a worker where the browser
  has them, else a WebGL2 canvas published with `captureStream` (Firefox's
  canvas track has no `requestFrame`, so it is captured at 30 fps; only
  processed pictures are ever drawn on that canvas); inference at most every
  40 ms with the last mask reused; every camera acquisition path of both
  sessions goes through one controller; photos in IndexedDB sealed under the
  device key behind the store gate; a new wipe step. The desktop app takes the
  canvas path (WKWebView has no frame streams); not run there.
- **iOS** (`apps/ios/Lakko/Calls/Background/`): the stage in `CapturerFan`,
  Core Image compositing on one Metal context, inference at most every 66 ms;
  the choice is filed per account on the device (a `photo:` choice names one
  account's photo); the failure turns the camera off from the session layer,
  so it also happens while the call is collapsed into the bar. iOS has no
  Sound section, so Background is the sheet's first section.
- **Android** (`apps/android/.../calls/Background*.kt`, `segment_jni.cc`):
  the stage in `CapturerFan`, OpenGL ES 2 compositing into a pool of output
  textures, photos sealed with the profile-photo wrap key behind the store
  gate.
- On every client a selected background whose engine is unavailable on the
  device shows the failure line and its button in the sheet, so the person
  can still turn the camera on without it; nothing is processed while the
  camera is off unless the sheet's preview is open.
- Found while building: `noise_jni.cc` (ADR-0088) included the C header
  without `extern "C"`, so `libsframe_jni.so` failed to load and every
  Android call failed; fixed there and by `cpp_compat` in
  `lakko-sframe-native/cbindgen.toml`.

## Alternatives

- **Platform engines (Apple Vision, Google ML Kit, plus a model on web).**
  Kept as the fallback the maintainer named. Against them now: three engines
  with three edges and three sets of bugs, ML Kit's metrics to Google, and the
  web needs its own model anyway.
- **MediaPipe Tasks libraries everywhere.** One engine, Google's tuned pipeline;
  excluded for their metrics to Google.
- **ONNX Runtime on every platform** (MIT; native libraries per platform,
  `onnxruntime-web` on the web). A tested, fast runtime with GPU back ends, but
  four native dependencies and four wrappers where `tract` is one Rust crate on
  the pattern ADR-0088 already uses. Kept as the answer if `tract` is too slow
  on phones (open question 2).
- **`tract-tflite` on Google's own file.** Needs the
  `Convolution2DTransposeBias` operator implemented in `tract`; possible, and an
  alternative to open question 1's conversion.
- **Inference on the GPU per platform (Core ML, a WebGPU or WebGL network).**
  Faster per frame, four implementations of the network instead of one; not
  needed if the CPU numbers hold on phones.
- **Chrome's built-in background blur** (a camera constraint on some
  platforms). Chrome only, blur only, decided by the browser: against the rule
  for calls.

## Consequences

- **Positive:** one model and one gate on every client, the same edge
  everywhere; nothing leaves the device but the processed picture; no server,
  wire or encryption change.
- **Negative:**
  - a new runtime dependency (`tract-onnx` and its tree) in the native static
    library and a wasm module of about 4.7 MB compressed as built, loaded on
    first use (open question 4);
  - CPU and battery cost on phones, unmeasured;
  - the compositing shaders are written four times;
  - a model file in the repository whose provenance is a third-party conversion
    until open question 1 is closed.
- **Neutral:** the security invariants are untouched: frames are processed
  before the SFrame seal as before (#2, #4), no state is sent (#5). The gate is
  a privacy rule of its own (UX section 3), not an E2EE gate.

## Open questions

1. **Model provenance.** Convert Google's `.tflite` with a pinned script (the
   custom transposed convolution must map to ONNX `ConvTranspose` plus `Add`),
   or implement the operator in `tract-tflite`, and compare outputs with the
   third-party file before release.
2. **Phones.** Inference time on a mid-range Android phone and an iPhone through
   the C ABI; if over the budget, inference at a lower rate with mask reuse,
   threads, or ONNX Runtime.
3. **Browsers.** The two web ways in Chrome, Firefox, Safari and the desktop
   app, including a hidden tab.
4. **wasm size.** A slimmer build: the model converted to NNEF at build time and
   only `tract-core` and `tract-nnef` in the module, `wasm-opt`, the name
   section stripped.
5. **Square or landscape.** Landscape is chosen for speed; a portrait camera
   (a phone held upright) may segment better with the square model.

## References

- UX: [`docs/ux/call-backgrounds.md`](../ux/call-backgrounds.md)
- [ADR-0088](0088-client-noise-suppression-rnnoise.md), the same shape for audio
- MediaPipe Tasks privacy notice: https://developers.google.com/edge/mediapipe/solutions/tasks
- ML Kit terms: https://developers.google.com/ml-kit/terms
- Model card: https://storage.googleapis.com/mediapipe-assets/Model%20Card%20MediaPipe%20Selfie%20Segmentation.pdf
- ONNX conversion: https://huggingface.co/onnx-community/mediapipe_selfie_segmentation_landscape
- tract: https://github.com/sonos/tract
