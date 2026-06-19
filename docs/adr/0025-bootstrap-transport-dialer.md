<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0025: Bootstrap transport dialer

- **Status:** Proposed
- **Date:** 2026-06-19
- **Context:** Phase 5 — anti-censorship operational wiring (Phase-3 block B entry)

## Context

`crates/lakko-transport` ships a full set of censorship-resistant transports —
REALITY, MASQUE, Shadowsocks-2022, WebTransport, and vanilla QUIC — each
implementing the `Transport` trait (`connect(&self, &Endpoint) -> Box<dyn
Connection>`), a happy-eyeballs selector (`HappyEyeballs` / `race_select`, which
is fail-closed: all candidates fail → `AllFailed`, never a silent plaintext
downgrade), and a signed bootstrap-config layer (`BootstrapConfig` + a 4-stage
`verify` + the `DohFetcher`). **Every one of these is tested but dead** — a grep
confirms `lakko-transport` is a dependency of no client and no service; the
clients dial over their platform HTTP stacks (web `fetch`, iOS `URLSession`,
Android Ktor/OkHttp).

The missing piece is the **runtime dispatcher**: nothing fetches the verified
config, turns its ordered `TransportSpec` list into selector candidates, and
dials the best transport. The primitives compose, but the composition does not
exist.

## Decision

**Add a `BootstrapDialer` to `lakko-transport` that composes the existing
pieces into one fail-closed entry point: a verified `BootstrapConfig` →
build a `PluggableCandidate` per `TransportSpec` → race them via
`HappyEyeballs` → return `(winning kind, Box<dyn Connection>)` or a fail-closed
error.** This is the operational wiring of the transport layer itself; routing
each client's HTTP through it is a separate, deferred Phase-5 effort (see
Consequences).

### API (`crates/lakko-transport/src/selector.rs`)

```rust
pub struct BootstrapDialer { /* HappyEyeballs policy */ }

impl BootstrapDialer {
    /// Build from a config the caller has ALREADY verified.
    pub fn from_verified_config(config: &BootstrapConfig) -> Result<Self, SelectorError>;
    /// Verify (signature + expiry + version + clock) THEN build —
    /// the entry that cannot skip the gate.
    pub fn from_signed_config(config: &BootstrapConfig, trust_anchor: &VerifyingKey, now: u64) -> Result<Self, SelectorError>;
    /// Race the candidates; first success wins, else fail-closed.
    pub async fn dial(&self, endpoint: &Endpoint, deadline: Duration) -> Result<(PluggableKind, Box<dyn Connection>), SelectorError>;
    /// Candidate kinds in config order — for logging/metrics.
    pub fn candidate_kinds(&self) -> Vec<PluggableKind>;
}
```

`build_candidates` maps each `TransportSpec::{Reality,Shadowsocks2022,
WebTransport,Masque}` to its `Transport::new(cfg)`. A spec whose `new()` rejects
its config is **skipped** (recorded), not fatal — one malformed entry in an
operator-signed config must not brick the rest; but if **zero** candidates
build, that is a fail-closed error (no transport → no connection). The dialer
never adds a direct/QUIC fallback that the signed config did not list:
`TransportSpec` has no `Direct` variant, so the candidate set is exactly the
operator's obfuscated transports (invariant #4 — no silent downgrade to a
censored path).

### What this deliberately does NOT do

- It does not fetch the config (the caller runs `DohFetcher::fetch_bootstrap_config`
  then hands the result in — keeps the dialer unit-testable without a network).
- It does not put the transport on any client's wire. The platform HTTP stacks
  (browser `fetch`, `URLSession`, Ktor) do not expose socket-level transport
  substitution; routing them through a `Box<dyn Connection>` is per-platform
  surgery (web: a WS/WebTransport-proxy or Service-Worker shim; iOS: a custom
  `URLProtocol`; Android: an OkHttp/Ktor socket factory) and cannot be validated
  without a live censored-network test. That is the deferred Phase-5 arc.

## Alternatives

1. **Bake fetch + verify into the dialer** (`dial_via_bootstrap(domain, anchor)`).
   Rejected as the primary API: it couples the dispatcher to the network +
   DoH, so the selection logic can only be integration-tested. Kept separable —
   `DohFetcher::fetch_bootstrap_config` already exists; the dialer takes the
   verified result.
2. **Fail the whole build on any malformed `TransportSpec`.** Rejected: a single
   operator typo in a signed config would disable all transports. Skipping the
   bad entry while keeping the rest (empty → fail-closed) maximises availability
   without weakening #4 — the signature gate already authenticated the config.
3. **Allow a direct/plaintext fallback when all obfuscated transports fail.**
   Rejected outright — that is the exact silent downgrade invariant #4 forbids.
   `AllFailed` is the correct, loud terminal state.
4. **Skip the dialer; wire each client's HTTP stack to the selector directly.**
   Rejected as the first step: the per-platform connection surgery is large,
   risky, and needs the consolidated dispatcher as its building block. The
   dialer is the prerequisite regardless of which client lands first.

## Consequences

- **Positive:** the selector becomes a real, single, fail-closed, property-tested
  entry point driven by the verified config — the prerequisite for any client
  wiring; consolidates five transports + the race + the config gate into one
  audited surface; no new dependency, no wire-format change.
- **Negative (honest):** it remains Rust-internal — no user-facing
  censorship-resistance ships until the deferred per-client wiring lands. The
  dialer is "better-composed dead code" until then. The happy-eyeballs race is
  still sequential (the concurrent `tokio::select!` upgrade is a separate,
  already-tracked follow-up).
- **Neutral:** the config's transport ORDER is the race order; an operator tunes
  priority by ordering `transports`.

## Open questions

- **Per-client connection-layer wiring** (web / iOS / Android) — the large
  Phase-5 arc that actually puts the transport on the wire. Needs a live
  censored-network (or the `ci/censorship-lab`) test to validate end-to-end.
- **Concurrent happy-eyeballs** (sub-RTT first-byte) — upgrade `race_select`
  from sequential to a true parallel race.

## References

- `crates/lakko-transport/src/pluggable.rs` — `HappyEyeballs` / `race_select` /
  `PluggableCandidate` (the selector this dialer drives).
- `crates/lakko-transport/src/rendezvous.rs` — `BootstrapConfig` + `TransportSpec`
  + the 4-stage `verify`.
- `crates/lakko-transport/src/doh_fetch.rs` — `DohFetcher::fetch_bootstrap_config`
  (the separable fetch the caller runs first).
- `docs/security/transport.md` — T-1..T-15 (the per-transport invariants).
- Security invariant #4 (no silent downgrade / fail-closed).
