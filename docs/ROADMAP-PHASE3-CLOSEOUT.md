<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Phase 3 closeout — gap inventory + sprint plan

Living document. Source of truth for what is left to take
Phase 3 from "code + tests green in CI" to "official close
under the audit-pack standard". Tracks Phase 4 / Phase 5 /
audit gaps in the same table so that one document covers
"what is left in Lakko before public beta".

The strategic plan lives at
`~/.claude/plans/1-nested-wreath.md`; the per-phase
tactical breakdown is [`ROADMAP.md`](./ROADMAP.md). This
file zooms in on the **post-Phase-3** punch list and the
revised sprint shapes that follow from
[ADR-0011](./adr/0011-rust-sfu.md) (Rust SFU instead of
LiveKit fork).

## Reading guide

- Rows tagged **G3-\*** belong to Phase 3 closeout (calls).
- Rows tagged **G4-\*** belong to Phase 4 (channels).
- Rows tagged **G5-\*** belong to Phase 5 (anti-censorship).
- Rows tagged **GA-\*** belong to the audit pack / process.
- "Chat?" = can a Claude agent close this gap from the
  terminal without physical hardware? `✅` = yes; `⚠` =
  partial (code in chat, validation needs hardware /
  upstream / external party); `❌` = no.

## Summary table

| # | Gap | Group | Blocks | Chat? | Estimate |
|---|---|---|---|---|---|
| **G3-1** | WebRTC pod iOS — `WebRTC.framework` + peer connection | Phase 3 closeout | 1×1 + group calls | ✅ + acceptance | 2 sprints |
| **G3-2** | WebRTC pod Android — `webrtc-sdk:android` + peer connection | Phase 3 closeout | 1×1 + group calls | ✅ + acceptance | 2 sprints |
| **G3-3** | Rust SFU on `webrtc-rs` / `str0m` (replaces LiveKit fork — see [ADR-0011](./adr/0011-rust-sfu.md)) | Phase 3 closeout | **group** calls only (1×1 needs no SFU) | ✅ | 7.5 sprints |
| **G3-4** | End-to-end CI gate (iOS / Android ↔ Dockerised SFU) | Phase 3 closeout | acceptance Phase 3 | ✅ | 1 sprint |
| **G3-5** | Settings modal — audio / video / output device + toggles | UI Phase 3 | UX completeness | ✅ | 1 sprint (3 platforms parallel) |
| **G3-6** | In-call chat panel — MLS application messages in call group | UI Phase 3 | UX completeness | ✅ | 1 sprint |
| **G3-7** | Sound cues — resources + player on 3 platforms | UI Phase 3 | UX completeness | ✅ | 0.5 sprint |
| **G3-8** | Acceptance on hardware (mic, camera, BT, CallKit / ConnectionService) | Phase 3 closeout | official Phase 3 close | ❌ maintainer + devices | 1 sprint |
| **G3-9** | Cost analysis (3b6) — real measurements after a month in production | Phase 3 closeout | scale-out trigger calibration | ❌ needs deploy | out of scope of chat |
| **G4-1** | `services/channels` backend (Rust) | Phase 4a | publishing over the wire | ✅ | 1 sprint |
| **G4-2** | Client integration of channels on 3 platforms | Phase 4a | UX channel publishing | ✅ | 2 sprints |
| **G4-3** | Subscriber path — HPKE seed envelope + subscribe handshake | Phase 4b | real subscribers | ✅ | 2 sprints |
| **G4-4** | Rotation + fanout service | Phase 4c | scale-out 100k+ | ✅ | 2 sprints |
| **G4-5** | Channel discovery (`@channelname` via OPRF) | Phase 4b | UX "find a channel" | ✅ | 1 sprint |
| **G5-1** | REALITY pluggable transport | Phase 5 | RU / CN / IR block bypass | ✅ + lab | 3 sprints |
| **G5-2** | MASQUE / CONNECT-UDP | Phase 5 | QUIC under H3 | ✅ + lab | 2 sprints |
| **G5-3** | Shadowsocks-2022 | Phase 5 | alternative obfs | ✅ | 1 sprint |
| **G5-4** | WebTransport (web client) | Phase 5 | web client in censored regions | ✅ | 1 sprint |
| **G5-5** | Transport rendezvous (signed bootstrap config) | Phase 5 | PT endpoint discovery | ✅ | 1 sprint |
| **G5-6** | ECH (Encrypted Client Hello) | Phase 5 | SNI privacy | ✅ | 1 sprint |
| **G5-7** | Censorship lab (GFW / TSPU / IR emulation) | Phase 5 | CI gate for PT survival | ✅ | 2 sprints |
| **GA-0** | [ADR-0011](./adr/0011-rust-sfu.md) — supersedes ADR-0003 / ADR-0008 | Audit / Design | rationale for G3-3 rewrite | ✅ | landed (this commit) |
| **GA-1** | G-23 libcrux — waits for openmls upgrade | Audit | upstream CVE | ⚠ depends on upstream | tracker |
| **GA-2** | G-26 RFC 9605 wire-header interop | Audit | interop with mlspp | ✅ | 1 sprint |
| **GA-3** | External crypto audit (Phase 2 exit gate per plan) | Audit | public-beta launch | ❌ external auditor | out of scope of chat |
| **GA-4** | Reproducible builds verification — 2 runners byte-identical | Process | SLSA L3 | ✅ | 1 sprint |
| **GA-5** | TUF auto-update infrastructure | Process | secure updates | ✅ | 2 sprints |
| **GA-6** | Bug bounty programme | Process | ongoing post-launch | ❌ org work | out of scope of chat |

## Group A — Phase 3 closeout sprints

### S-3-A-iOS-pod (G3-1)

**Composition.**
- Add `WebRTC.framework` via SPM as a binary target
  (pinned version recorded in `apps/ios/Package.swift`).
- `apps/ios/Lakko/Calls/WebRtcPod.swift` —
  `RTCPeerConnectionFactory` wrapper, ICE / DTLS setup.
- `apps/ios/Lakko/Calls/SframeBridge.swift` —
  `RTCFrameEncryptor` / `RTCFrameDecryptor` calling our
  `crates/lakko-calls::sframe` via UniFFI.
- `apps/ios/Tests/WebRtc/` — peer-pair tests,
  ICE-gathering, codec-preference, SFrame round-trip.
- GitHub Actions workflow `ci/ios-webrtc-test.yml` with a
  macOS runner.

**Exit criteria.**
- [ ] All 9 unit / integration tests green locally and in CI.
- [ ] `RTCFrameEncryptor` pipeline: plaintext frame →
      ciphertext on the wire differs from plaintext;
      decryptor recovers bit-exact.
- [ ] Codec preferences: SDP contains AV1 before VP9
      before H264; Opus carries `useinbandfec=1`.
- [ ] Locally-spawned coturn fixture; ICE gathering
      completes.

**Out of scope.** Real app integration (a real call placed
through the app) — that is `S-3-A-acceptance`.

### S-3-A-Android-pod (G3-2)

Mirror of `S-3-A-iOS-pod`. `io.github.webrtc-sdk:android`,
JUnit + Robolectric, `apps/android/app/src/test/kotlin/me/lakko/app/calls/webrtc/`.
Workflow `ci/android-webrtc-test.yml` with a Linux runner.

**Exit criteria.** Identical to iOS, plus a Robolectric
shadow for `AudioManager` device enumeration.

### S-3-A-settings (G3-5)

3 platforms in parallel; same state shape (`CallSettings`)
across web / iOS / Android.

**Composition.**
- State: `selectedMic / selectedCamera / selectedSpeaker:
  DeviceId`, `availableDevices: list`,
  `noiseSuppression / echoCancellation / autoGain: bool`,
  `mirrorLocal: bool`, `lowDataMode: bool`.
- Device enumeration: `AVCaptureDevice.devices(for: .audio)`
  (iOS), `AudioManager.getDevices()` (Android),
  `navigator.mediaDevices.enumerateDevices()` (web).
- UI: gear button to the left of end-call → bottom-sheet
  (mobile) / modal (web / desktop).
- Persistence: encrypted under DeviceGroup; syncs between
  devices.
- Live apply: device switch during an active call does
  not drop the PC.

**Exit criteria.**
- [ ] UI snapshot tests green on all 3 platforms.
- [ ] Unit test: mid-call device switch keeps the PC alive.
- [ ] Settings persist across relaunch.

### S-3-A-chat (G3-6)

**Composition.**
- State in `CallUiState`: `chatMessages`, `chatOpen`,
  `composeText`, `unreadCount`.
- Transport: MLS application message in the **call's**
  MLS group. NOT `lakko-channels` — this is intra-call
  chat, not channel posts.
- UI: side panel on web (right rail, 320 px),
  bottom-sheet detent on mobile (~70 %).
- System events (Alice joined, hand raised, screen share
  started) — generated client-side from `CallUiState`
  deltas, printed inline as grey lines.
- Persistence: chat remains in the regular messaging
  store after `endCall()` (it's a normal MLS group —
  history bundle for a new device works "for free").

**Exit criteria.**
- [ ] UI snapshot tests green.
- [ ] Send / receive round-trip through a mock MLS group
      in unit test.
- [ ] Unread badge clears on panel open.
- [ ] Chat persists in storage after `endCall`.

### S-3-A-sounds (G3-7)

**Composition.**
- Synthesise sounds via `tools/audio/synth-sounds.sh`
  (SoX / FFmpeg — sine bursts + ADSR envelopes). All
  CC0, fully owned. Output: `apps/*/Resources/Sounds/*.m4a`.
  - `ringtone-incoming.m4a` (loop)
  - `ringback-outgoing.m4a` (loop)
  - `join.m4a`, `leave.m4a`, `error.m4a`
- iOS: `AVAudioPlayer` for one-shot, `AVAudioEngine` for
  loops, CallKit ringtone via
  `CXProviderConfiguration.ringtoneSound`.
- Android: `SoundPool` + `MediaPlayer`, ConnectionService
  ringtone.
- Web: `HTMLAudioElement` for one-shot, `AudioContext`
  for timing precision.

**Exit criteria.**
- [ ] Sounds reproducibly generated by the script
      (CI artifact comparison).
- [ ] Unit tests: phase transition triggers the correct
      sound on each platform.
- [ ] No audio plays when the device is in silent mode
      (platform politeness).

### S-3-A-e2e-gate (G3-4)

**Composition.**
- `docker-compose.e2e.yml` fixture: `auth` + `ds` +
  `discovery` + the Rust SFU + coturn.
- Test runner on macOS GitHub Actions runs an iOS XCTest
  that connects to the containerised SFU for real.
- Linux GitHub Actions runs an Android instrumented test
  via headless emulator against the same fixture.
- Scenario:
  1. Two clients pull JWT from auth.
  2. Connect to the SFU via WS.
  3. Publish encrypted media frames.
  4. SFU forwards.
  5. Receiver decrypts → byte-exact match.

**Exit criteria.**
- [ ] E2E scenario green in CI on both platforms.
- [ ] PR blocked when the scenario goes red.

### S-3-A-acceptance (G3-8) — the only "not in chat" sprint

Performed by the maintainer with hardware. Checklist:

- [ ] iPhone ↔ Pixel 1×1 call via P2P, clean audio,
      crisp video.
- [ ] Same scenario through the group SFU.
- [ ] BT headset routing (AirPods, Sony).
- [ ] CallKit behaviour on iOS lock screen.
- [ ] ConnectionService behaviour on Android lock screen.
- [ ] Battery drain: 10-minute call < 5 % drain on
      iPhone 13+.
- [ ] Subjective MOS rating ≥ 4.0 on 4G / 5G in the office.

## Group B — Rust SFU sprints (replaces the LiveKit-fork plan)

Per [ADR-0011](./adr/0011-rust-sfu.md). Lives in
`services/sfu/` as a regular workspace Rust crate
(`lakko-svc-sfu`).

| Sprint | Composition | Estimate |
|---|---|---|
| **S-3-B-spike** | Echo-test bake-off `webrtc-rs` vs `str0m`; throughput / RTT / memory per peer; deal-breaker API gaps. Decision recorded as an amendment to ADR-0011 §"Spike outcome" | 0.5 sprint |
| **S-3-B-skeleton** | `services/sfu/` Rust crate. axum WS signaling endpoint. Peer registry (`PeerId → PeerConnection`). Echo test (one peer publishes, sees itself receiving) | 1 sprint |
| **S-3-B-routing** | RTP track fanout — incoming track → outgoing tracks on every other peer. No simulcast yet. Test: 3 peers, A → B + C forwarding | 1 sprint |
| **S-3-B-simulcast** | Simulcast layer extraction (RID + dependency descriptor). Per-receiver layer selection (low / mid / high) based on downlink bitrate | 1 sprint |
| **S-3-B-jwt** | `lakko-call-jwt` verification — **same crate** that `services/auth` mints from, direct `workspace.dependencies` link. Test: HS256 → reject, expired → reject, wrong audience → reject | 0.25 sprint |
| **S-3-B-twcc** | TWCC aggregation. Per-receiver bandwidth estimation. Layer-switching feedback (when to step down / up) | 1 sprint |
| **S-3-B-metrics** | Prometheus exporter via the `metrics` crate (same surface as other services). Every metric referenced by `infra/prometheus/rules/calls.yml` exposed | 0.5 sprint |
| **S-3-B-prod** | Config via env-vars (project convention). Graceful shutdown. `/health` + `/ready`. `tracing` structured logging (same shape as auth / ds / discovery) | 1 sprint |
| **S-3-B-tests** | Integration tests with mock peers through the chosen WebRTC stack's test harness. Includes SFrame passthrough (ciphertext byte-exact unmodified by SFU) | 1 sprint |
| **Total** | | **≈ 7.5 sprints** |

### Group B exit criteria (whole group)

- [ ] `services/sfu/` builds as a regular Rust crate
      (`cargo build -p lakko-svc-sfu`).
- [ ] `#![forbid(unsafe_code)]`, `clippy::pedantic` clean.
- [ ] Docker image ≤ 30 MB (`distroless/cc` base).
- [ ] Every metric referenced by
      `infra/prometheus/rules/calls.yml` is exposed.
- [ ] Integration test: 3 mock peers, A publish → B + C
      receive, SFrame ciphertext byte-exact passthrough.
- [ ] Simulcast: slow-link receiver gets low layer,
      fast-link gets high; switch under TWCC feedback.
- [ ] `lakko-call-jwt` is the only auth path — no
      alternative verifier exists.
- [ ] Memory per active call ≤ 20 MB.

## Wave plan

```
W1: UI parity, no WebRTC dependencies
    S-3-A-settings ∥ S-3-A-chat ∥ S-3-A-sounds       (~1.5 sprints)
    └─ closes: G3-5, G3-6, G3-7

W2.1: Client peer connections — 1×1 P2P calls work
    S-3-A-iOS-pod ∥ S-3-A-Android-pod                (~2 sprints, parallel toolchains)
    └─ closes: G3-1, G3-2
    └─ MILESTONE: 1×1 calls end-to-end through coturn
                  (STUN / TURN), with no SFU involved

W2.2: Rust SFU — group calls work
    S-3-B-spike → S-3-B-skeleton → S-3-B-routing →
      S-3-B-simulcast ∥ S-3-B-jwt ∥ S-3-B-twcc ∥ S-3-B-metrics →
      S-3-B-prod → S-3-B-tests                       (~7.5 sprints)
    └─ closes: G3-3, GA-0
    └─ MILESTONE: group calls up to ~50 participants

W3: End-to-end CI gate
    S-3-A-e2e-gate                                    (~1 sprint)
    └─ closes: G3-4

W4: Acceptance on hardware
    S-3-A-acceptance                                  (out of chat)
    └─ closes: G3-8 → official Phase 3 close

W5+: Phase 4 (channels) + Phase 5 (anti-censorship) + audit gaps
     (see ROADMAP.md §2c / §3 for the detailed sub-sprint breakdowns)
```

## Dependencies

```
W1 (UI parity) ─────────────────────────────────────┐
                                                     │
W2.1 (G3-1, G3-2) ──→ MILESTONE: 1×1 works          │
   │                                                 │
   ├──→ W3 (G3-4) ───────────────────────────────────┼──→ W4 (G3-8)
   │                                                 │
W2.2 (G3-3, GA-0) ──→ MILESTONE: groups work        │
   │                                                 │
   └──→ W3 (G3-4)                                    │
                                                     │
W5 (G4-*) — independent of W2 / W3; can start in parallel with W2.2
W6 (G5-*) — independent; can start in parallel
```

## What changed vs the previous plan

| Before (ADR-0003 / ADR-0008) | After (ADR-0011) |
|---|---|
| LiveKit Go fork: 2 sprints + ongoing rebase tax | Rust SFU: 7.5 sprints + ~0 maintenance |
| ~150 k LOC audit surface | ~3 k LOC audit surface |
| Go + Rust toolchains in CI | Rust only |
| Recording architecturally possible (patched out) | Recording **not in the code** |
| Cross-language FFI in the dep graph | None |
| Wire schemas duplicated (Go-prost + Rust-prost) | `lakko-proto.workspace = true` directly |
| Auth-JWT logic duplicated (Go lib + Rust) | `lakko-call-jwt` linked directly into SFU |
| 1×1 milestone not called out | 1×1 milestone explicit, lands after W2.1 |

## What is unchanged

- Client side stays on native `WebRTC.framework` (iOS) and
  `webrtc-sdk:android` (Android) — needed for hardware
  codecs + CallKit / ConnectionService.
- W1 (UI parity) — unchanged.
- coturn for STUN / TURN — unchanged; used by both 1×1
  and group flows for NAT traversal.
- Phase 4 / Phase 5 sprints — unchanged.
- Audit gaps — only GA-0 added (the ADR-0011 record).

## Cross-links

- [ADR-0011](./adr/0011-rust-sfu.md) — Rust SFU strategy
- [ADR-0003](./adr/0003-livekit-fork.md) — superseded
- [ADR-0008](./adr/0008-livekit-fork-strategy.md) — superseded
- [`ROADMAP.md`](./ROADMAP.md) — long-form per-phase plan
- [`services/sfu/README.md`](../services/sfu/README.md) — SFU crate entry point
- [`services/sfu/PLAYBOOK.md`](../services/sfu/PLAYBOOK.md) — build / run / test runbook
- [`infra/README-calls.md`](../infra/README-calls.md) — deployment topology
- [`docs/security/calls.md`](./security/calls.md) — call-side crypto invariants (unchanged)
- [`docs/security/known-gaps.md`](./security/known-gaps.md) — audit-pack gap register
