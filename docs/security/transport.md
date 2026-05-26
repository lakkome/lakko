<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Lakko Contributors -->

# Transport-layer security invariants (Phase 5)

Audit-pack subsystem doc for the anti-censorship +
rendezvous transport surface. Phase 5 per
[ADR-0004](../adr/0004-anti-censorship.md); decomposition
in [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md)
group **G5-\***. Sister doc to
[`calls.md`](./calls.md) (call-side crypto) and
[`mls-state-machine.md`](./mls-state-machine.md) (MLS
group state); same audit-pack convention — every
invariant cites a property test or a fuzz target.

The transport layer's invariants get their own `T-N`
namespace so the C-N call-side numbering stays
contiguous as it accumulates.

## Crate layout

| Module | Status | What |
|---|---|---|
| [`crates/lakko-transport/src/transport.rs`](../../crates/lakko-transport/src/transport.rs) | shipped | `Transport` / `Connection` / `Listener` trait surface (the physical wire) |
| [`crates/lakko-transport/src/quic.rs`](../../crates/lakko-transport/src/quic.rs) | shipped | quinn-backed QUIC v1 + 0-RTT |
| [`crates/lakko-transport/src/http.rs`](../../crates/lakko-transport/src/http.rs) | shipped | reqwest+rustls REST client with SPKI pinning |
| [`crates/lakko-transport/src/pluggable.rs`](../../crates/lakko-transport/src/pluggable.rs) | shipped | `PluggableTransport` policy + `HappyEyeballs` selector |
| [`crates/lakko-transport/src/rendezvous.rs`](../../crates/lakko-transport/src/rendezvous.rs) | shipped | Ed25519-signed bootstrap config + sign / verify |
| [`crates/lakko-transport/src/reality.rs`](../../crates/lakko-transport/src/reality.rs) | scaffold (G5-1) | REALITY TLS-in-TLS config + stub transport |
| [`crates/lakko-transport/src/masque.rs`](../../crates/lakko-transport/src/masque.rs) | scaffold (G5-2) | MASQUE / CONNECT-UDP config + stub transport |
| [`crates/lakko-transport/src/shadowsocks.rs`](../../crates/lakko-transport/src/shadowsocks.rs) | scaffold (G5-3) | Shadowsocks-2022 config + stub transport |
| [`crates/lakko-transport/src/webtransport_pt.rs`](../../crates/lakko-transport/src/webtransport_pt.rs) | scaffold (G5-4) | WebTransport config + stub transport |

## Threat-model context

Transport-side adversary in Lakko's threat model:

- **Censor on path** sees ciphertext + packet metadata
  (size, timing). Goal: drop or distinguish Lakko's
  traffic from background flow.
- **Active probe** drills the apparent rendezvous host
  with handshake variants to fingerprint. Goal: confirm
  it's a Lakko endpoint so it can be blocked.
- **Rendezvous-server compromise** — a malicious or
  coerced rendezvous can serve an attacker-crafted
  bootstrap config that points clients at honeypot
  transports.

The pluggable + rendezvous surfaces defend against each
of these three. Server-side TLS termination + SFU /
DS / channels / auth opacity are out of scope of THIS
file — they live in [`calls.md`](./calls.md) §"What the
SFU sees" and [`auth-flow.md`](./auth-flow.md).

## Invariants the transport layer MUST preserve

### T-1: Pluggable-kind wire identifiers are frozen at v1

[`PluggableKind`](../../crates/lakko-transport/src/pluggable.rs)
enumerates the five transport classes Lakko's
rendezvous + selector know about. Each variant has a
**stable lowercase string identifier** returned by
`PluggableKind::as_str()`:

| Variant | Wire string |
|---|---|
| `Quic` | `"quic"` |
| `Reality` | `"reality"` |
| `Shadowsocks2022` | `"shadowsocks-2022"` |
| `WebTransport` | `"webtransport"` |
| `Masque` | `"masque"` |

These strings are the discriminator the rendezvous JSON
config (`TransportSpec`) tags transport entries with.
**Renaming any of them is a breaking protocol change**:
every already-pinned bootstrap config the client side
holds in its trust store would silently mis-route.

Test: `pluggable_kind_string_table_pinned` in
`pluggable.rs::tests` — explicit byte-string equality
for every variant.

### T-2: Scaffold transports surface `NotImplemented` loudly

The four PT scaffolds (REALITY / MASQUE / Shadowsocks-
2022 / WebTransport) return
[`crate::error::Error::NotImplemented { feature, sprint }`](../../crates/lakko-transport/src/error.rs)
from their `Transport::connect` impl. The error message
carries the sprint label (`S-5-reality`, `S-5-masque`,
…) so an operator log line points straight at the open
ROADMAP item.

This is structural: a build that accidentally enables a
scaffold transport in production fails at the first
connect attempt with a typed error, never falls back to
unauthenticated transport, never crashes silently.

The `Pluggable` selector wraps these per-candidate
errors inside
[`PluggableError::AllFailed::per_candidate`](../../crates/lakko-transport/src/pluggable.rs)
so the operator can see "every candidate is still a
stub" at a glance.

Test: `scaffold_transport_surfaces_not_implemented` in
`pluggable.rs::tests`.

### T-3: Selector race preserves submission order

[`HappyEyeballs::new`](../../crates/lakko-transport/src/pluggable.rs)
takes a `Vec<PluggableCandidate>` and races them in
submission order. The race is sequential in the current
scaffold — one candidate's outer-`tokio::time::timeout`
slice of the deadline, then the next — so the
bootstrap config's priority list is honoured exactly
as the server signed it.

A future concurrent (true happy-eyeballs) refactor MUST
preserve "first success wins". The first candidate to
return `Ok` shorts the rest; the
[`PluggableTransport::connect`](../../crates/lakko-transport/src/pluggable.rs)
return type is `(PluggableKind, Box<dyn Connection>)` so
the caller knows which kind won and can record the
choice in metrics + the user-facing connection-detail
panel.

Test: `candidate_kinds_in_race_order_preserved` in
`pluggable.rs::tests`.

### T-4: Rendezvous verify is a 4-stage gate

[`BootstrapConfig::verify`](../../crates/lakko-transport/src/rendezvous.rs)
enforces four checks **in this order** before returning
`Ok(())`:

1. **Schema version equals `SCHEMA_VERSION`** (currently
   `1`). A forward-incompatible bump is a hard reject:
   the client refuses rather than try to parse new
   fields it does not understand. Stops a coerced
   rendezvous from silencing a client by pushing a
   not-yet-supported config shape.
2. **`expires_at > now`** (`RendezvousError::Expired`).
3. **`issued_at <= now + ISSUED_AT_CLOCK_SKEW_SECS`**
   (currently 1 hour;
   `RendezvousError::IssuedInFuture`). Two-sided
   bound — too-future configs are rejected on the
   theory that either the server clock is wrong or
   the attacker is replaying.
4. **Ed25519 verify** against the trust anchor over the
   canonical bytes (T-5). Failure produces
   `RendezvousError::SignatureRejected`.

The four checks produce four distinct error variants
so the caller can branch on "fetched a stale cache"
vs "the rendezvous endpoint is compromised"; the wire
error surface is part of the public contract.

Tests: `verify_rejects_expired`,
`verify_rejects_issued_in_future`,
`verify_rejects_version_mismatch`,
`verify_rejects_wrong_trust_anchor`,
`verify_rejects_tampered_payload`,
`verify_tolerates_minor_clock_skew`,
`skew_window_boundary` in `rendezvous.rs::tests`.

### T-5: Canonical bytes = JSON with `signature_hex` zeroed

The Ed25519 signature covers
`serde_json::to_vec(&self)` **with `signature_hex` set
to `String::new()`**. The signer
([`BootstrapConfig::sign`](../../crates/lakko-transport/src/rendezvous.rs))
and the verifier
([`BootstrapConfig::canonical_bytes`](../../crates/lakko-transport/src/rendezvous.rs))
use the same code path; that one source of truth is the
audit-pack guarantee.

`serde` preserves struct field order on re-serialise,
so a parsed `BootstrapConfig` round-tripped through
JSON re-produces the same canonical bytes the signer
saw. The test
`json_round_trip_preserves_canonical_bytes` makes the
property visible end-to-end (encode → parse → verify
still green).

Tamper detection covers any field except
`signature_hex` itself: modifying a `TransportSpec` (or
any of its inner config fields) after signing makes
the canonical bytes differ → Ed25519 verify
returns `SignatureRejected`.

Test:
- `sign_then_verify_round_trip` (happy path)
- `verify_rejects_tampered_payload` (modify a
  RealityConfig's `sni` after signing → rejected)
- `json_round_trip_preserves_canonical_bytes` (the
  encoder-decoder symmetry property)

### T-6: TransportSpec kebab-case kind is part of the protocol surface

[`TransportSpec`](../../crates/lakko-transport/src/rendezvous.rs)
is tagged via `#[serde(tag = "kind", rename_all =
"kebab-case")]`. The kind strings inside the rendered
JSON match T-1 exactly:

```json
{ "kind": "reality", ... }
{ "kind": "shadowsocks-2022", ... }
{ "kind": "webtransport", ... }
{ "kind": "masque", ... }
```

The discriminator on the rendezvous wire is the same
string `PluggableKind::as_str()` produces — a single
source of truth.

Test: `transport_spec_kebab_kind_pinned` (asserts the
JSON-serialised form carries `"kind":"shadowsocks-2022"`
literally).

### T-7: Trust anchor is binary-pinned

The Ed25519 public key
[`BootstrapConfig::verify`](../../crates/lakko-transport/src/rendezvous.rs)
checks against is **not** fetched from the network. It
ships in the client binary (TUF / app bundle). Compromise
of the signing key requires a new build with a rotated
trust anchor — the rotation procedure is tracked in
[`known-gaps.md`](./known-gaps.md) until the TUF
auto-update sprint (GA-5) lands.

Network-resident attackers can substitute a malicious
rendezvous response but cannot mint a signature that
verifies against the pinned key.

This is a **structural** invariant — it lives in the
caller's wiring rather than inside the rendezvous
module — so the test for it is the absence of any
`reqwest::get` / network fetch path inside
`rendezvous.rs`. A future PR that introduces a
network-resolved trust anchor MUST update this
section.

## Known soft spots — out of scope for Phase 5 scaffold

### Real `Transport` implementations for REALITY / MASQUE / SS-2022 / WebTransport

Tracked as G5-1 / G5-2 / G5-3 / G5-4 in
[`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md).
Until those land, the only candidate that **actually
completes a connect** through the pluggable selector
is the existing QUIC transport. T-2 protects us from
silently using a half-built scaffold.

### DoH-resolved bootstrap fetch

The rendezvous module implements parse + sign + verify.
The actual fetch path (DoH → innocuous-looking domain →
HTTP GET → `BootstrapConfig`) is a follow-up sub-sprint
that reuses [`crate::http::HttpTransport`](../../crates/lakko-transport/src/http.rs)
with SPKI pinning.

### Concurrent happy-eyeballs

The scaffold selector is sequential (per-candidate
outer-`tokio::time::timeout`). True concurrent race
(`tokio::select!` over spawned per-candidate tasks)
needs the `Transport` trait to support cheap cloning
(`Arc<dyn Transport>` wrapping or a `Clone` bound). The
first G5-N sprint that wants sub-RTT first-byte does
the refactor.

### ECH (Encrypted Client Hello)

Tracked as G5-6. Rides on rustls 0.23+ ECH support
once it stabilises. Until then the outer SNI on the
TLS-based transports (REALITY, WebTransport,
HttpTransport) is observable to the path.

### Censorship lab tests

Tracked as G5-7. A real CI gate that exercises every
PT against GFW / TSPU / IR DPI emulators is the audit
acceptance for Phase 5; today the invariants above are
unit-level + offline.

## Cross-links

- [ADR-0004](../adr/0004-anti-censorship.md) —
  pluggable-transport strategy
- [`docs/ROADMAP-PHASE3-CLOSEOUT.md`](../ROADMAP-PHASE3-CLOSEOUT.md) —
  G5-\* sprint decomposition
- [`calls.md`](./calls.md) — call-side cryptography
  (sister doc — disjoint subsystem, shared audit
  approach)
- [`known-gaps.md`](./known-gaps.md) — open work register
- [`threat-model.md`](./threat-model.md) — the censor
  + active-probe + compromised-rendezvous adversary
  models this doc's invariants resist
