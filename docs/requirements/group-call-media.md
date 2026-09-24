<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Group-call media — requirements derived from LiveKit

Written 2026-09-24. Status: **draft requirements, nothing here is built or decided.**
Each section below becomes an ADR (for anything touching the wire, the SFU or a
security invariant) or a UX spec change before code is written. The grid UI is
specified separately in [`docs/ux/group-call-grid.md`](../ux/group-call-grid.md).

## 0. Scope and method

We are **porting ideas, not code and not integrating**. LiveKit
(`livekit/livekit` server in Go on pion, `client-sdk-js`, `components-js`,
`livekit-cli`) is Apache-2.0, compatible with AGPLv3; where an algorithm is
transcribed closely (constants, state machines) the Rust/TS file carries an
attribution comment naming the LiveKit file it follows. Everything that breaks
zero-trust is excluded: egress/recording, ingress/SIP, agents, server-side
transcription, cloud telemetry, Krisp.

The LiveKit mechanisms were read from source at livekit `6b2e3ec`,
client-sdk-js `5cadc93`, components-js `409e60a`, livekit-cli `3bb6e55`
(all 2026-09-23/24). The Swift and Android LiveKit SDKs were **not** read.
Constants quoted below are LiveKit's defaults as found in that source; they are
starting points for our own tuning, not measurements of ours.

Security invariants that constrain every section: #2 (the server sees only
ciphertext and public artefacts), #4 (no fallback to non-E2EE), #5 (no social or
behavioural graph materialised on the server). Nothing here changes SFrame keying
(ADR-0013/0029/0035) or MLS.

## 1. What exists today (recon, 2026-09-24)

Findings that shape the requirements; file references are to the Lakko tree.

- **Simulcast policy exists but cannot act.** The SFU has a layer gate, per-layer
  bitrate meters and a budget-based `drive_layer_selection`
  (`services/sfu/src/media.rs:2950-3032`), but **no client publishes simulcast**
  (no `sendEncodings` / rids on any platform) and **str0m bandwidth estimation is
  disabled** (`Rtc::builder().enable_bwe(None)`, `media.rs:595`) because the
  pacer caused 15 s stalls. The policy therefore never switches.
- **No downlink allocation across tracks.** Each track gets the subscriber's whole
  budget independently (`media.rs:2987-2994`).
- **No group-call quality.** Per-participant quality is hardcoded `"good"` on all
  three clients (web `sfu-call-ui.ts:138`, iOS `SfuCallUi.swift:439`, Android
  `SfuCallUi.kt:280`). The SFU logs str0m RTCP stats only under
  `LAKKO_SFU_MEDIA_DIAG_SECS` (`media.rs:2152-2205`).
- **No resume.** Group calls fail immediately on ICE `failed` (web) or
  `failed|disconnected` (iOS, Android); every rejoin gets a fresh `PeerId`
  (`signaling.rs:195`). `Reconnecting` exists in all three UI models but is never
  set. The only concession is `CALL_EMPTY_GRACE = 5 s`.
- **No load test.** `services/sfu/tests/synthetic_client_e2e.rs` has real str0m
  clients over loopback (2–3 peers); nothing drives many publishers.
- **`speaking` is always false** on all three clients; `setSpeaking` has no caller.
- **The audio-level header extension is negotiated by default.** The SFU never
  registers it explicitly, but str0m's `ExtensionMap::standard()` maps
  `urn:ietf:params:rtp-hdrext:ssrc-audio-level` at id 1
  (`crates/vendor/str0m/src/rtp/ext.rs:373`), no client strips it, and libwebrtc
  sends it once negotiated. The SFU neither reads nor relays it — but **every
  client is sending the SFU a cleartext speech-activity signal**, 50 times a
  second, which the SFU's operator can log. See section 8.
- **Opus is unconfigured.** No client sets DTX, CBR or bitrate; libwebrtc defaults
  apply (VBR). `crates/lakko-calls/src/codec_audio.rs` has an `OpusConfig` nothing uses.
- **Unused Rust that overlaps this work:** `crates/lakko-calls/src/bandwidth.rs`
  (`BandwidthController`, a per-participant layer ladder) and
  `crates/lakko-calls/src/quality.rs` (E-model MOS, exported as wasm
  `estimateCallQuality` / uniffi `FfiCallQualitySampler`, no caller). Each section
  says whether to reuse or retire them.
- **Signaling today:** `ServerFrame` = Welcome / PeerJoined / PeerLeft /
  SignalRelay / Error / Pong / SdpAnswer / SdpOffer; `ClientFrame` = SignalRelay /
  Ping / SdpOffer / IceCandidate / SdpAnswer (`crates/lakko-sfu-proto`). No track
  metadata, subscription control, quality, speakers or resume.
- Incidental: Android's group More menu still offers "Share screen" and
  "Raise hand" that only toggle a local flag (`ui/call/CallScreen.kt:2027-2046`),
  and `docs/ux/calls.md` §4.3 says the server drops a silent phone "within a
  minute" while it now does so after 15 s (`config.rs:50`).

## 2. Prerequisites (P) — block sections 3–5

- **P-1 Simulcast publishing on all three clients.** Camera video published with
  three encodings, rids `q` / `h` / `f` (quarter, half, full), each with an explicit
  `maxBitrate` and `scaleResolutionDownBy`; the ladder is one shared table (a
  `lakko-ui-rules`-style Rust constant exposed over both FFI surfaces) so the three
  clients cannot drift. Audio stays single-encoding. SFrame arming is unchanged —
  every encoding of one sender shares the sender's KID (ADR-0035 is per stream,
  and simulcast layers of one camera are one stream); **this must be confirmed in
  the ADR** against how the SFrame transform sees simulcast frames on web
  (`RTCRtpScriptTransform` per sender) and natives (one `FrameEncryptor` per
  sender).
- **P-2 A working bandwidth estimate.** Re-enable str0m BWE (TWCC) without the
  pacer stall that got it disabled, or, failing that, take REMB/TWCC-derived
  estimates from receiver feedback. Exit criterion: `drive_layer_selection`
  observably switches layers in the synthetic e2e under a throttled link, with no
  stall. LiveKit's default is remote BWE from client REMB, with send-side TWCC as
  an option.
- **P-3 A load harness** (section 7) exists before sections 3–5 are tuned, so
  thresholds are measured rather than copied.

## 3. Dynacast — pause encodings nobody watches

**LiveKit mechanism.** Per subscriber, the SFU tracks the highest spatial layer
that subscriber is being forwarded (held at the current layer until the switch
completes). Per codec, the maximum across subscribers becomes a set of
`enabled` flags per quality, sent to the publisher as `SubscribedQualityUpdate`.
Upgrades apply immediately; downgrades are debounced (`DynacastPauseDelay`, 5 s);
the first report is forced after 10 s. The client sets `encoding.active`
accordingly (Firefox cannot, so it degrades the encoding to 1/4 scale, 10 bps,
2 fps instead).

**Requirements.**
- **DYN-1** New `ServerFrame::SubscribedLayers { track: <msid/mid>, layers: [q,h,f bools] }`,
  sent only to the track's publisher.
- **DYN-2** The SFU computes, per published video track, the union of layers
  currently forwarded or selected for any subscriber; a layer with no consumer is
  reported disabled. Upgrade immediately; downgrade after a debounce (start at 5 s,
  tune with P-3). Never report all layers disabled while a subscriber exists that
  has the track enabled — the lowest wanted layer stays on.
- **DYN-3** Clients apply it via `RTCRtpSender.setParameters` (`encodings[i].active`),
  only when the value changes, serialised with the existing per-call lock so it
  cannot race renegotiation. Firefox fallback as LiveKit. Re-apply after any
  renegotiation (browsers re-enable encodings).
- **DYN-4** Zero-knowledge check: the frame reveals nothing the SFU does not
  already know (it chooses layers). No invariant impact.
- **DYN-5** Metrics: `lakko_sfu_dynacast_layer_updates_total{direction}`; client
  log line on each applied change (plain text, no emoji).

## 4. Adaptive stream — the receiver says what it can show

**LiveKit mechanism.** The JS client observes every rendered video element
(`ResizeObserver` + `IntersectionObserver` + PiP events), reports the largest
rendered size × a pixel-density factor (2 if DPR > 2, else 1) and visibility,
debounced 100 ms (5 s for backgrounding). The server maps the requested height to
a layer with a 0.9 tolerance and mutes the down-track when disabled; debounce
100 ms, un-disable applied immediately. Off by default in the SDK.

**Requirements.**
- **ADS-1** New `ClientFrame::TrackSettings { source_peer, track, visible: bool, max_layer: q|h|f }`.
  The client sends a **quantised layer**, not pixel dimensions (see ADS-5): it maps
  its own tile size to `q` / `h` / `f` with the same shared ladder as P-1.
- **ADS-2** Each client measures tile visibility and size in its own UI toolkit:
  web `IntersectionObserver` + `ResizeObserver` on the tile element; iOS
  `onGeometryChange` / visibility via the `LazyVGrid` appearance; Android
  `onGloballyPositioned` + `LazyGrid` visible items. Off-screen pages of the grid
  (UX spec pagination) are `visible: false`. App backgrounded ⇒ all `visible: false`
  after a delay (LiveKit uses 5 s; audio is never paused).
- **ADS-3** SFU: `visible: false` stops forwarding that video track to that
  subscriber (not audio); `max_layer` caps the layer the policy may choose.
  Un-hide applies immediately with a PLI; hide/downsize debounced.
- **ADS-4** UI must distinguish "video paused by the system" from "camera off"
  (UX spec). The last frame or avatar is shown; the state is local knowledge.
- **ADS-5 Privacy (needs a maintainer decision).** Tile size tells the SFU which
  participant each viewer has enlarged. Because the featured tile follows the
  active speaker, a per-viewer "this participant is featured" signal is a
  **server-visible proxy for who is speaking**, re-creating the leak section 8
  removes. Options, in decreasing privacy:
  (a) adaptive stream limited to `visible` only, no size (grid pages, background);
  (b) size quantised to three layers and changes rate-limited / delayed by a
  randomised few seconds so featured-tile switches do not track speech in time;
  (c) LiveKit-like continuous size. Recommendation: (b), with (a) as the first
  step. The ADR records the residual.

## 5. Stream allocator — share a subscriber's downlink across tracks

**LiveKit mechanism.** Per subscriber, video tracks are *managed* (screen share
exempt unless multi-layer). Priority 1–255 (default 255 screen share, 1 camera,
plus a client-set `priority`). On congestion every track is allocated layer by
layer, lowest first, in priority order ("stream as many as possible, then
fairness"); spare bandwidth boosts deficient tracks one step at a time; padding
probes (goal = usage + max(1.2 × next step, 200 kbps)) discover headroom. States
STABLE/DEFICIENT; pausing a track is possible but off by default.

**Requirements.**
- **ALLOC-1** Replace the per-track independent budget in `drive_layer_selection`
  with a per-subscriber allocator: exempt tracks (screen share, audio) first; then
  managed camera tracks, layer by layer, ordered by priority, then by
  `TrackSettings.max_layer` wanted, then stable join order.
- **ALLOC-2** Priority comes only from the subscriber's own `TrackSettings`
  (pinned/featured tile ⇒ higher) and from the track kind; the SFU derives no
  priority from audio (it has none — section 8).
- **ALLOC-3** Pausing a whole track under congestion: allowed, but surfaced to the
  subscriber as `ServerFrame::StreamState { source_peer, track, paused }` so the
  UI shows "video paused (bandwidth)" per ADS-4. Off by default until P-3 measures it.
- **ALLOC-4** Probing: padding probes as LiveKit, only once P-2 is stable. Padding
  packets carry no payload and no SFrame; they are RTP padding only.
- **ALLOC-5** Decide the fate of `lakko-calls::bandwidth::BandwidthController`:
  it is a client-side single-track ladder with no caller; retire it or reuse its
  hysteresis constants. Do not run two layer policies.

## 6. Connection quality per participant

**LiveKit mechanism.** Server-side, per track, from RTCP receiver reports
(downstream) and received RTP (upstream): packet score = 100 − delay effect −
loss × codec weight (Opus 8, video 10, scaled by packet-rate normalisation so DTX
or a static screen is not penalised), bitrate score, layer-distance score; the
minimum, smoothed (0.4 up / 0.8 down), floored at 30. Levels: EXCELLENT > 80,
GOOD > 40, POOR > 20, else LOST. Per participant = the worst track; computed
every 5 s, sent only on level change, each participant receives its own entry and
those it subscribes to. In LiveKit's default config RTT, jitter and bitrate score
are **off**, so the score is loss + layer distance only.

**Requirements.**
- **CQ-1** The SFU computes a per-peer level from what it already sees (RTCP RR
  loss from each subscriber, upstream loss/NACK from each publisher, jitter, RTT)
  — never from payloads. Transcribe LiveKit's scorer, including the packet-rate
  normalisation (our audio may become CBR/non-DTX per section 8, so re-check it).
- **CQ-2** New `ServerFrame::ConnectionQuality { entries: [(peer_id, level)] }`,
  levels `excellent | good | poor | lost`, sent every 5 s only on change, filtered
  to the recipient and the peers it receives. No numeric score on the wire (the UI
  uses the level; the score adds nothing and is one more thing to keep stable).
- **CQ-3** Clients replace the hardcoded `"good"` with the received level for
  remote tiles, and keep their local getStats-based tier for the self tile; the
  mapping to `QualityTier` is one shared rule in `lakko-ui-rules`.
- **CQ-4** Invariant check: quality derives from transport statistics the SFU
  already holds; sending it back to the peers of the same call reveals nothing new
  to the server. It does reveal to call members a peer's network condition — that
  is the feature, and it matches the 1×1 path.
- **CQ-5** Retire or reuse `lakko-calls::quality` (client-side E-model, no caller).
  One scorer, on the server, is the recommendation; the crate's MOS code may be
  the starting point for CQ-1.
- **CQ-6** As LiveKit's client does: the self level stuck at `lost` for 10 s while
  signaling is up triggers the rejoin path of section 9 (RC-4).

## 7. Load testing

**LiveKit mechanism.** `livekit-cli load-test` runs N real SDK clients, publishers
looping encoded H.264/VP8/Opus files (simulcast from three files), subscribers
choosing layers by a layout (speaker / 3×3 / 4×4 / 5×5) and reporting packets,
bitrate and sample-builder drops. It reports no latency.

**Requirements.**
- **LT-1** A `tools/sfu-loadtest` binary reusing the str0m client code from
  `synthetic_client_e2e.rs`: N publishers × M subscribers per call, K calls, ramp
  rate; publishers send **opaque synthetic payloads** of realistic frame sizes and
  cadence (the SFU must never parse payloads, so realistic codec bytes are not
  needed) with simulcast rids and the generic frame descriptor extension.
- **LT-2** The payload embeds a send timestamp and sequence so the harness reports
  **one-way forwarding latency** (p50/p95/p99), loss and reorder per subscriber —
  the metric LiveKit's tool lacks.
- **LT-3** Scenarios: 1 publisher → 100 subscribers; 9 publishers all subscribed
  (3×3 grid); 25 publishers with adaptive stream (one high, rest low / hidden);
  throttled subscriber link (P-2, ALLOC); churn (join/leave every few seconds).
- **LT-4** Runs locally and in a `workflow_dispatch` workflow; results are written
  as a table in the run summary. No numbers in docs until measured.

## 8. Active speaker detection without weakening zero-trust

### 8.1 Why not LiveKit's approach

LiveKit detects speakers **on the server** from the `ssrc-audio-level` RTP header
extension (RFC 6464), which SFrame does not encrypt: 400 ms windows, active if at
least 40 % of the window is at or above −35 dBov, level smoothed (EMA 0.667),
changes broadcast every 400 ms as `SpeakersChanged`. The JS client does not
compute speaking itself. For Lakko that is a per-participant speech-activity
timeline handed to the server — a behavioural graph (invariant #5) and a
content-adjacent signal. We do not port it.

### 8.2 The leak we already have

As found in section 1, Lakko clients already **send** `ssrc-audio-level` to the SFU
because str0m negotiates it by default. The SFU ignores it, but a server we do not
trust is exactly the party invariant #2 is written against.

- **SPK-1 Stop negotiating it, on both sides.** The SFU builds its extension map
  without `AudioLevel` (and without any other audio header extension that carries
  content-derived data). Each client independently refuses to offer or accept it:
  web via `RTCRtpTransceiver.setHeaderExtensionsToNegotiate` (already used for the
  frame descriptor in `frame-descriptor.ts`), natives via the transceiver header
  extension API or SDP filtering. Belt and braces: a malicious SFU that accepts it
  must still get nothing. A test asserts the published audio carries no
  audio-level extension (synthetic e2e on the SFU side; per-client SDP assertion).
- **SPK-2 Record the residual honestly: traffic analysis.** Removing the header
  removes the explicit signal, not the inferable one. With Opus VBR (the libwebrtc
  default we run today) packet sizes vary with speech, and with DTX packets stop in
  silence; either lets a server estimate who speaks when. Options, as a maintainer
  decision recorded in the ADR:
  (a) accept and document the residual (VBR, no DTX);
  (b) **Opus CBR with DTX off** for group calls (`cbr=1; usedtx=0`), so every
  packet of a stream is the same size at a fixed cadence; SFrame adds a constant
  overhead and keeps it constant. Costs some bandwidth and quality in silence;
  measure with P-3 before deciding.
  The recommendation is (b) for group calls, since the SFU sees every participant's
  audio at once; 1×1 calls are peer-to-peer DTLS-SRTP with no server in the media path.

### 8.3 Local detection

Every device computes "who is speaking" from the audio it has already **decrypted
and decoded**, for its own display only. Nothing about it goes on the wire.

- **SPK-3 Input.** Per remote audio track, poll WebRTC stats `inbound-rtp`
  `totalAudioEnergy` and `totalSamplesDuration`; the energy delta over the duration
  delta gives the mean energy of the last window. In libwebrtc these are computed
  from the decoded playout audio rather than from the header extension — **to be
  verified on all three stacks with the extension removed (SPK-1) before relying
  on it**; if a stack does not populate them without the extension, fall back to a
  decoded-audio tap (web: Web Audio `AnalyserNode` on the remote track; iOS/Android:
  an audio track sink on the decoded remote track). Self: `media-source`
  `totalAudioEnergy` of the local microphone track.
  One `getStats()` on the single SFU peer connection covers every receiver, so
  cost is one call per window per call, not per participant.
- **SPK-4 The rule, written once.** A pure function in `lakko-ui-rules` (it is
  presentation, not security — nothing about it leaves the device), exposed on
  both FFI surfaces, so the three clients cannot drift:
  `speaking_update(prev_state, window_energy, window_ms) -> (state, level)`,
  following LiveKit's audiolevel shape with our own inputs: window 400 ms (start
  value), active when the window's level is above a threshold equivalent to
  −35 dBov for at least 40 % of the window, level smoothed with EMA 2/(N+1), N = 2;
  reset after two silent windows. The visual indicator's on/off timing (on after
  200 ms of speech, off after 800 ms of silence) and the separate **ordering
  hysteresis** for the grid (1.5 s of speech to count as the speaker, 10 s minimum
  stay, at most one swap per 3 s, 3 s stage hold) are owned by
  [`docs/ux/group-call-grid.md`](../ux/group-call-grid.md); both are start values.
  Thresholds start as LiveKit's and are tuned with recorded test audio, not guessed.
- **SPK-5 Muted participants.** A participant whose `MediaState` says the mic is
  muted is never "speaking", whatever the energy (echo, leakage). This needs a
  `mic_on` flag in the existing E2EE `media_state` envelope (today camera only) —
  a small `lakko-contacts` wire change, bounded and fuzzed like the camera flag.
- **SPK-6 Accessibility.** The speaking state feeds tile labels
  ("Anna, speaking") and never relies on colour alone (UX spec).
- **SPK-7 Interaction with adaptive stream / allocator.** A client's local speaker
  decision may raise that participant's tile to featured and therefore its
  `TrackSettings.max_layer`. This is the ADS-5 proxy leak; it is governed by the
  ADS-5 decision, not duplicated here.

### 8.4 What the server can still learn, stated

With SPK-1 and SPK-2(b) and ADS-5(a/b): the server learns that a peer is in the
call and publishing audio at a constant rate; it does not learn loudness or
speech timing from the audio stream. With SPK-2(a) it can estimate speech timing
from packet sizes. With ADS-5(c) it can estimate the featured speaker from viewer
tile sizes. The ADR states which of these the product accepts.

## 9. Reconnect — resume before rejoin

**LiveKit mechanism.** Two levels. *Resume*: reconnect the WebSocket with
`reconnect=1&sid=<participant>`, the server swaps the signal channel onto the
existing participant, both sides ICE-restart the existing peer connections, the
client re-sends a `SyncState` (last SDP, subscriptions, published tracks);
events during the gap are buffered. *Full reconnect*: leave, tear down, join
fresh, republish. Triggers: signal close or PC `failed` ⇒ resume; server says
reconnect, PC state NEW after sleep, quality LOST 10 s, or a failed resume ⇒ full.
Retry delays `0, 300, 1200, 2700, 4800, 7000×5` ms, jitter 0–1000 ms after the
second attempt, 10 attempts. The server keeps a participant whose transport
failed for 5 s (`disconnectCleanupDuration`) awaiting a resume. UI events:
`SignalReconnecting` / `Reconnecting` / `Reconnected`.

**Requirements.**
- **RC-1 Resume token.** `Welcome` gains `resume_token`: 32 random bytes, stored
  server-side only as a hash, bound to `(call_id, JWT sub, peer_id)`, single use,
  rotated on every successful resume, valid for a short grace (start at LiveKit's
  5 s plus ICE time — decided in the ADR). A resume request must also present a
  valid call JWT for the same `sub` and `call_id`; the token alone admits nothing.
  Invariant check: the token is a server-side session handle, carries no key
  material, and does not survive the call.
- **RC-2 Server side.** On transport or socket loss the SFU keeps the peer (its
  `PeerId`, published tracks, down-tracks) for the grace period without announcing
  `PeerLeft`; a `ClientFrame::Resume { token }` on a new socket re-attaches it and
  triggers an SFU-side ICE restart. Grace expiry ⇒ the existing leave path.
  `CALL_EMPTY_GRACE` must not end a call whose only peers are in resume grace.
- **RC-3 Client side, all three.** ICE `disconnected` starts a short grace (web
  already does 10 s on 1×1; group currently fails instantly), then `restartIce()`
  on the existing peer connection; socket loss ⇒ reconnect the socket and
  `Resume`. The SFrame keys, call key ring and MLS state are untouched by a
  resume; if the MLS epoch advanced meanwhile, the existing ADR-0029 rekey path
  runs as usual. The parked-offer / ufrag-change handling in
  `sfu-call-session.ts:1117-1156` changes from "restart is fatal" to "restart is
  the resume path".
- **RC-4 Rejoin.** A failed resume, a server refusal, a device that slept (PC
  state `new`), or CQ-6 ⇒ full rejoin: new socket, new JWT if needed, republish;
  same retry schedule as LiveKit (tuned by P-3), then give up with the existing
  failure state.
- **RC-5 UI.** Set the existing `Reconnecting` phase for a resume ("Reconnecting…",
  media keeps its state) and a distinct rejoin state ("Rejoining the call…"),
  per the UX spec; remote peers in grace show a tile state, not a departure.
  Update `docs/ux/calls.md` §4.3, which says group calls cannot resume, in the
  same change.
- **RC-6 Network change on natives.** iOS `NWPathMonitor` / Android
  `ConnectivityManager` network-change events trigger an immediate ICE restart
  rather than waiting for ICE to fail (the equivalent of the browser `online`
  event shortcut).

## 10. Group grid UI

Specified in [`docs/ux/group-call-grid.md`](../ux/group-call-grid.md) with a
visual mockup. LiveKit's components-js was used as a reference for layout
breakpoints (1×1 … 5×5 by container width), pagination at the layout's capacity,
**slot-stable ordering** (a newcomer takes a leaver's slot instead of reflowing)
and tile ordering (local, screen share, then by speaking / last spoke / camera on /
join time). Grid-order rules that must agree across clients go in
`lakko-ui-rules` beside `order_conversations`.

## 11. Suggested order

1. SPK-1 (stop sending audio level) — small, closes a live leak, independent.
2. P-3 load harness skeleton, then P-2 (BWE) — unblocks everything measurable.
3. SPK-2..6 local speaker detection + grid UI (no wire change except `mic_on`).
4. RC-1..6 resume.
5. P-1 simulcast, then DYN, ADS (with the ADS-5 decision), ALLOC.
6. CQ.

Each numbered step is its own ADR + change; SPK-1, SPK-2, ADS-5 and RC-1 go
through `lakko-security` on request.

## 12. Open decisions for the maintainer

1. SPK-2: Opus CBR/no-DTX for group calls (closes timing leak, costs bandwidth) vs accept.
2. ADS-5: how much tile-size information the SFU may receive.
3. RC-1: resume grace length (affects how long a vanished peer's tile lingers).
4. ALLOC-3: may the SFU pause a camera entirely under congestion?
5. Retire or reuse `lakko-calls::bandwidth` and `lakko-calls::quality`.
6. Android's placeholder "Share screen" / "Raise hand" menu items: remove now or
   leave for the grid work.
