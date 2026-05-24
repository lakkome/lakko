<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# ADR-0008: LiveKit fork — operational strategy

- **Status:** **Superseded** by [ADR-0011](./0011-rust-sfu.md) (2026-05-24)
- **Date:** 2026-05-19
- **Context:** Phase 3b implementation kick-off
- **Supersedes:** none (extended ADR-0003 with operational detail)

> **Note (2026-05-24):** this ADR is **entirely superseded**.
> Lakko no longer forks LiveKit; the SFU is a minimal Rust
> crate built on `webrtc-rs` / `str0m`. Quarterly-rebase,
> patch-files, `LAKKO:` markers — none of that mechanism
> applies anymore. The whole "upstream tracking" framework
> goes away because there is no upstream to track.
>
> Two specific decisions from this ADR **survive** in
> [ADR-0011](./0011-rust-sfu.md):
>
> 1. **Auth token format** (§"Auth token format") — the
>    `CallJwtClaims` shape moved into `crates/lakko-call-jwt`
>    and is verified the same way against the Rust SFU.
> 2. **Single-VPS Hetzner deployment topology**
>    (§"Deployment") — still applies, see
>    [`infra/README-calls.md`](../../infra/README-calls.md).
>
> Everything else in this document — fork layout, patch
> sequencing (3b1b / 3b1c / 3b1d), `lakko-patches/`,
> `lakko-additions/`, rebase mechanics — is no longer in
> effect. Read the rest of this document as historical
> context for why we chose Rust SFU in
> [ADR-0011](./0011-rust-sfu.md).

## Context

[ADR-0003](./0003-livekit-fork.md) pinned LiveKit as the SFU
of record + SFrame as the media encryption primitive. That
decision is two years old; the Phase 3b sprint kicks off
implementation. Before writing fork-side Go code we need
the operational story locked in:

1. **What exactly do we fork?** LiveKit's monorepo is large
   (server + protocol + SDKs). We don't want to inherit
   every artefact.
2. **What's the upstream-tracking story?** A fork that
   diverges silently from upstream is a maintenance trap.
3. **Where does the SFrame keying integrate?** LiveKit ships
   its own E2EE story (`livekit-egress` + `participant_key`
   broadcast). Our keys come from MLS exporter. The
   integration point matters for the diff size.
4. **What's NOT forked but used as-is?** We want a minimal
   delta against upstream.

This ADR records the answers.

## Decision

### What we fork

We fork ONLY `livekit/server` (the SFU media-routing
process) and `livekit/protocol` (the Go protobuf + Go server
types). Reference:
- [github.com/livekit/livekit](https://github.com/livekit/livekit) — `services/sfu/` in our tree.
- [github.com/livekit/protocol](https://github.com/livekit/protocol) — vendored under `services/sfu/internal/protocol/`.

We do **NOT** fork:

- Client SDKs (`livekit-client`, `livekit-server-sdk-go`, etc.).
  The Lakko clients drive native `RTCPeerConnection` on each
  platform; the LiveKit SFU is the server we connect to.
  Speaking LiveKit's wire protocol from our own code is a
  smaller surface than maintaining their client SDK forks.
- Egress / ingress / agent services. We don't transcode and
  we don't relay outside the SFU.
- Cloud orchestration (`livekit-cli`, helm charts, etc.).
  Phase 5 deployment is single-VPS via systemd; we don't
  need the LiveKit ops machinery.

### Upstream-tracking policy

LiveKit's `main` branch ships breaking changes regularly.
The fork carries:

1. A pinned **upstream commit** in `services/sfu/UPSTREAM.md`.
   The file records: commit hash, release tag (or `main@<sha>`),
   date of pin, summary of changes since last pin.
2. A **rebase cadence**: re-pin upstream every ~3 months,
   regardless of feature need. The longer we drift, the
   bigger the rebase diff.
3. **No silent vendoring**: every change to a forked file
   carries a `LAKKO:` comment at the top of the function /
   block explaining the reason. CI greps for these markers
   to produce a "lakko diff vs upstream" report.

Rebase mechanics:
- Maintain a `lakko-patches/` directory of `git format-patch`
  files representing our diffs against the pinned upstream.
- On rebase: pull new upstream → re-apply patches → resolve
  conflicts → bump the pin → update `UPSTREAM.md`.
- This is the same playbook the [signal-server](https://github.com/signalapp/Signal-Server)
  fork practitioners use; it scales for the maintenance
  cadence we expect.

### Integration point: SFrame keying

LiveKit's [insertable-streams E2EE](https://github.com/livekit/livekit-client/blob/main/src/e2ee/E2EEManager.ts)
ships a `participant_key` flow where:

1. Each participant exchanges a key out-of-band (LiveKit
   delegates the "how").
2. The SFU forwards keys it doesn't understand to peers
   (key opacity).
3. Clients encrypt frames with SFrame using that key.

We replace **only step (1)** — the out-of-band exchange.
Lakko derives the per-call seed from
`mls_exporter("lakko/call/v1", call_id, 32)` (Phase 3a4,
shipped in `crates/lakko-calls/src/sframe.rs`). The seed
plus the participant's MLS leaf index produces the
SFrame key the platform-native `RTCPeerConnection`'s
insertable-streams hook applies per frame.

The SFU side does **not** need to validate or even forward
the keys — clients derive their own from the MLS exporter
they all share. This is a **strict reduction** of the
LiveKit E2EE surface: we delete LiveKit's
`participant_key` broadcast and rely on clients to
agree out-of-band via the MLS exporter.

### Fork-side modifications — minimal diff

The diff against upstream is intentionally small:

1. **Drop LiveKit's `participant_key` flow** from
   `server/e2ee.go` (~50 lines). Clients no longer publish
   keys to the SFU; the SFU passes through encrypted frames
   without inspection.
2. **Strip the plaintext-media path** — refuse to start a
   call without insertable-streams marker on every track.
   Defense-in-depth: even if a client misconfigures, the
   SFU MUST NOT serve unencrypted media.
3. **Replace LiveKit auth** with our token format. LiveKit's
   API-key/Secret-based JWT becomes a JWT signed by
   `services/auth` scoped to `(call_id, account_id)`.
4. **Wire** our gRPC routes to the SFU's room-create /
   participant-join / participant-leave events so the
   `services/auth`-issued JWT carries enough state.

Estimated total diff: < 800 lines of changed Go (LiveKit's
server is ~150k lines).

### Deployment

Phase 3b ships **one SFU instance** on a dedicated Hetzner
CCX-tier VPS (media bandwidth heavy). Topology:

```text
   client ───TURN over TLS:443──► coturn (same VPS) ──► SFU
                                                       │
                                                       ▼
                                                  Prometheus /
                                                  Grafana on
                                                  the management
                                                  VPS
```

- coturn alongside the SFU on the same VPS — same admin
  domain, same TLS cert, single network surface.
- TURN-over-TLS on port 443 — survives most NAT / firewall
  configurations; the censorship-resistance transports
  (Phase 5) layer on top.
- The SFU exposes Prometheus metrics. Grafana on a separate
  management VPS scrapes; no metric ever leaves the host
  (call counts + bandwidth, never plaintext).

Scaling: a second SFU VPS lands when the first hits 70 %
sustained CPU or bandwidth. SFU clustering / cascade is
Phase 4 (broadcasts); Phase 3b is single-node.

### Auth token format

```rust
// JWT claims signed by services/auth's signing key.
// Verified by the SFU using the public key the auth
// service publishes at startup.
struct CallJwtClaims {
    account_id: Vec<u8>,        // raw 16-byte UUIDv7
    call_id:    Vec<u8>,        // raw 16-byte random
    role:       enum {
        Participant,
        Publisher,              // can publish tracks
        Subscriber,             // listen-only (Phase 4)
    },
    issued_at:  u64,            // unix seconds
    expires_at: u64,            // 1 hour after issued_at
}
```

The SFU never validates the user against `services/auth` at
admission time — the JWT signature is the proof. Short
expiry (1 hour) bounds the damage from a leaked JWT.

### Phase ordering inside 3b

| Sub-sprint | What |
|---|---|
| 3b1a | Fork tree set up: `services/sfu/` + `UPSTREAM.md` + initial pin (latest upstream tag at the time of sprint kickoff) |
| 3b1b | Strip LiveKit `participant_key` flow + plaintext-media path |
| 3b1c | Wire Lakko JWT validation (verify against `services/auth`'s public key) |
| 3b1d | Replace LiveKit room-create with our `services/sfu`-fronted RPC |
| 3b2 | Deploy SFU on Hetzner VPS + coturn alongside |
| 3b3 | TURN-over-TLS on port 443 |
| 3b4 | Auth token issuance (`services/auth/call_jwt` endpoint) |
| 3b5 | Prometheus / Grafana monitoring |
| 3b6 | Cost analysis + scale projection |

### Client-side integration (Phase 3a-pod-integration)

The Lakko clients connect via `RTCPeerConnection` directly
to the SFU. The connection layer:

1. Client requests a Call JWT from `services/auth` (REST
   over the existing QUIC / WS transport).
2. Client opens `RTCPeerConnection` against the SFU's
   public URL, passing the JWT in the join handshake.
3. SFU verifies the JWT, accepts the connection, starts
   forwarding frames.
4. Client derives the SFrame key locally via the MLS
   exporter (`crates/lakko-calls`), wires it into the
   `RTCRtpSender.transform` / `RTCRtpReceiver.transform`
   insertable-streams hooks (web), or the equivalent
   `RTCFrameCryptor` API (Apple WebRTC, Google WebRTC).

We do not ship a Lakko-specific SDK. Each platform's
existing `RTCPeerConnection` integration (web `native`, iOS
`WebRTC.framework`, Android `org.webrtc:google-webrtc`)
carries the insertable-streams hook the LiveKit client SDK
also uses — we plug into the same hook from our own
codebase.

## Alternatives considered

### Custom Rust SFU

Tempting (full ownership, no Go runtime, native Rust core).
Rejected:

- A production-quality SFU is a 2-year detour. Simulcast,
  SVC, jitter buffer, congestion control, codec negotiation
  — LiveKit got all of this right at scale, we'd be
  re-inventing.
- `pion/webrtc` Go ecosystem is more mature than
  `webrtc-rs`. LiveKit builds on pion.
- The fork-side diff is small. The maintenance burden of
  the fork is less than the maintenance burden of a custom
  SFU.

Revisit: maybe in 2030 when Rust WebRTC reaches LiveKit
parity. Not before.

### Use [janus-gateway](https://github.com/meetecho/janus-gateway) or [mediasoup](https://github.com/versatica/mediasoup)

- janus — C, complex per-room plugin model. Insertable-
  streams E2EE story less mature.
- mediasoup — Node.js + C++ worker. Insertable streams
  supported but the operational shape is less familiar to
  our team.

LiveKit's Go server + Apache 2.0 + active maintenance + a
mature E2EE story is the lower-risk pick.

### Use LiveKit Cloud

Hosted SFU. Rejected: trust model violation. The SFU is
assumed-compromised (per `docs/security/threat-model.md`),
so the operator's identity doesn't matter cryptographically
— but operating it ourselves is the only way to keep the
deployment posture aligned with the rest of Lakko (single-
VPS, Hetzner, AGPL).

## Consequences

### Good

- Production-grade SFU on day one of Phase 3b.
- Tiny diff against upstream (< 800 lines), tracked via
  `lakko-patches/`.
- No client-side SDK fork — we plug into the platform's
  existing WebRTC stack at the insertable-streams hook.
- LiveKit's Apache 2.0 → AGPLv3 relicense is permitted
  (Apache 2.0 is compatible with AGPLv3 downstream).
- The deletion of LiveKit's `participant_key` flow is a
  security strict-improvement: the SFU literally can't see
  keys it doesn't possess.

### Bad

- Go on the SFU host. The rest of Lakko is Rust. We accept
  this as a one-process exception; the SFU never reads
  user-secret bytes anyway.
- Quarterly upstream-rebase cadence is real ops cost.
  Mitigation: small diff → small rebases.
- LiveKit's `participant_key` removal means a misconfigured
  client that DOES try to use LiveKit's E2EE flow fails
  loudly. We document this in the fork README + reject the
  join in the SFU side.

### Ugly

- Two protobuf domains: the LiveKit protocol (Go-side, fork)
  + the Lakko wire schemas (Rust-side, `crates/lakko-proto`).
  We keep them logically separate; the auth-JWT format is
  the only shape that crosses the boundary.
- LiveKit's stats / observability surfaces are Go-idiomatic.
  Our Prometheus dashboards have to live with two label
  conventions until / unless we standardise.

## Open items

- **3b1a pin source**: pick a recent LiveKit release tag at
  3b1a kickoff. As of this ADR, [v1.7.4](https://github.com/livekit/livekit/releases/tag/v1.7.4)
  is the current release; the 3b1a sprint re-checks.
- **Insertable-streams hook compatibility**: verify the
  `RTCRtpScriptTransform` API works in Safari (Apple's
  implementation has historical gaps). Web stack today
  defaults to `RTCRtpSender.createEncodedStreams` which is
  Chromium / Firefox only. The 3a-pod-integration sprint
  validates Safari per a feature-detection probe.
- **AES vs ChaCha for SFrame on the SFU host**: SFU itself
  doesn't decrypt frames, so this is a client-side concern
  only. Documented in `docs/security/crypto-overview.md`
  already (ChaCha default, AES for hardware-AES devices).
