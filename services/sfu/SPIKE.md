<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# WebRTC-stack bake-off — `webrtc-rs` vs `str0m`

[ADR-0011](../../docs/adr/0011-rust-sfu.md) deferred the stack
choice to this sprint (`S-3-B-spike`). The picks below are
the rationale for that choice; the result is appended to
ADR-0011 §"Spike outcome".

## Decision

**`str0m`** is the chosen stack for Lakko's SFU.

The audit-friendliness gap is decisive: `str0m`'s sync,
caller-driven event loop is auditable in one sitting,
while `webrtc-rs`'s tokio-spawning surface multiplies the
reasoning footprint. `webrtc-rs` would be a defensible
pick for a generic WebRTC application; for a forwarding-
only SFU under a "server is assumed compromised" threat
model, `str0m` wins on the property that matters most.

Mapping back to the [ADR-0011](../../docs/adr/0011-rust-sfu.md)
criteria:

| Criterion | webrtc-rs | str0m | Winner |
|---|---|---|---|
| Audit surface | ~30 k LOC, tokio-spawned tasks | ~6 k LOC, sync event loop driven by caller | **str0m** |
| API model | async / tokio, hidden internal scheduling | sync, every state transition explicit at the call site | **str0m** for our use case |
| SFU-fitness | full WebRTC stack (peer / data-channel / SCTP / RTP); SFU is one of many use cases | designed around SFU forwarding; data-channel + SCTP minimal | **str0m** |
| DataChannel + SCTP | full | minimal | webrtc-rs — but **not used by Lakko**; signaling rides our own WS + media is SFrame-encrypted RTP, so SCTP-control is dead weight |
| Production users | Cloudflare Calls; some Discord internals | Eyevinn streaming, sentry.io, Embark internals | tie — both run in production |
| Maintenance activity | active; large contributor surface | active; small core team | both viable; **str0m**'s smaller core matches our crate-size posture |
| Documentation | extensive, tutorial-heavy | concise, example-driven | tie — different tastes; both adequate |
| Codec passthrough (we never decode) | yes | yes | tie |
| Simulcast / SVC layer parsing (we need this for per-receiver layer selection) | yes | yes | tie |
| TWCC aggregation | yes | yes (caller drives the math) | str0m exposes the inputs more directly; both workable |
| Compatibility with `#![forbid(unsafe_code)]` | clean | clean | tie |

## Why audit-friendliness wins for Lakko

The SFU is **assumed compromised** by Lakko's threat model
([`docs/security/threat-model.md`](../../docs/security/threat-model.md)).
Two implications follow:

1. **External audit will scrutinise the SFU codebase.** An
   audit budget that has to also walk every spawned tokio
   task and every internal channel inside the WebRTC stack
   spends most of its hours on plumbing instead of on the
   Lakko-specific glue. A sync stack with one event loop
   leaves the auditor free to focus on what we wrote, not
   what the library does behind a `tokio::spawn`.

2. **Recording / capture must be architecturally absent.**
   `str0m`'s narrower surface makes it easier to assert,
   line by line, that no codepath buffers payload to disk,
   forks frames to a side-channel, or hands them to a
   recorder. `webrtc-rs` has the same property in practice,
   but proving it takes more pages of audit report.

The "smaller is better" pattern also matches the rest of
Lakko's posture: every other crate in the workspace is
written in the same `#![forbid(unsafe_code)]` /
`clippy::pedantic` / `tracing` / `thiserror` shape; pulling
in a 30k-LOC dependency that exposes its own scheduling
philosophy is the wrong default for this codebase.

## What we give up

- **Data channels / SCTP.** Lakko does not use them — call
  signaling rides our own WS protocol (Lakko-native, not
  LiveKit JSON-RPC; see `services/sfu/src/signaling.rs`),
  and chat-during-call rides MLS application messages
  (S-3-A-chat). The "no SCTP / DataChannel" property is
  fine.
- **A tutorial-heavy ecosystem.** `webrtc-rs` has more
  examples on the open web. `str0m`'s examples are tighter
  but cover the SFU case directly. Internal docs in
  `services/sfu/` carry the patterns Lakko-specific code
  will copy.
- **Some niceties hidden behind `webrtc-rs`'s higher-level
  API.** `str0m` asks us to write the event loop ourselves.
  That's the same property that makes it auditable —
  explicit > magical for security-critical code.

## What we keep

- The same `cargo deny` / `cargo audit` / SBOM posture as
  every other Lakko crate.
- The architectural absence of recording from
  [ADR-0011](../../docs/adr/0011-rust-sfu.md) §"What the SFU
  does NOT do" — both stacks support this; `str0m` simply
  makes it easier to verify.
- The expected ~3 k LOC final SFU size — both stacks
  support the Lakko-specific glue at roughly the same
  budget.

## Pin

`str0m` version pinning is recorded in
[`UPSTREAM.md`](./UPSTREAM.md). The first concrete pin
lands in **S-3-B-routing** when the crate is first
imported into `services/sfu/Cargo.toml`.

## What this spike did NOT measure

Honest list of deferrals:

- **Empirical throughput / RTT / memory per peer.** A real
  benchmark needs the routing sprint's RTP fanout to be
  in place; running an echo-only spike on each stack would
  measure "echo loopback", which is not a useful proxy for
  N-peer fanout. The routing sprint includes a benchmark
  task that measures the chosen stack against the budget
  in [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../../docs/ROADMAP-PHASE3-CLOSEOUT.md)
  Group-B exit criteria.
- **Long-running soak / NAT-traversal at scale.** Needs
  real deploy — out of scope for the spike.
- **Codec corner cases (AV1 dependency descriptor edge
  cases, simulcast RID-to-track mapping under aggressive
  bandwidth-controller switching).** Lands with
  S-3-B-simulcast once we have track fanout to exercise.

The criteria above are sufficient to make the choice with
**no regret risk** — both stacks would produce a working
SFU; we are picking the one that aligns with Lakko's
audit-first posture.

## Reversal criteria

If `S-3-B-routing` hits a `str0m` API gap that is hard to
work around — most likely candidates: TWCC implementation
detail, simulcast layer-extraction edge case, or a long-
unmerged bug fix — the routing sprint is empowered to
re-open this decision and either:

1. Patch `str0m` upstream + pin to a fork of it; or
2. Switch to `webrtc-rs` with the trade-offs above
   re-evaluated against the concrete blocker.

Either branch is documented as a follow-up amendment to
ADR-0011, not as an emergency.
