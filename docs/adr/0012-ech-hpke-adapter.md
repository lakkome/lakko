<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0012: ECH wire-up via RustCrypto/hpke adapter, ring crypto provider retained

- **Status:** Accepted
- **Date:** 2026-05-28
- **Context:** Phase 5 G5-6-wireup sprint — Encrypted Client Hello support across the HTTPS-based pluggable transports.

## Context

[ADR-0004](./0004-anti-censorship.md) lists ECH (Encrypted
Client Hello, draft-ietf-tls-esni-18) as one of the
anti-censorship transports Lakko targets in Phase 5. The
G5-6 parser sprint (committed earlier in Phase 5) already
ships a defense-in-depth `parse_ech_config_list` that the
audit pack pins as invariant T-12. What was missing was
the *wire-up* — turning a parsed ECH config-list into an
actual `rustls::client::EchMode::Enable(..)` that the
client uses on every outgoing TLS handshake.

The wire-up sprint hit a constraint that wasn't obvious
from the parser side: **rustls 0.23 ships HPKE only under
the `aws_lc_rs` crypto provider**. The `ring` provider —
which `lakko-transport` is pinned to across REALITY,
WebTransport, HTTP, and Shadowsocks — has no
`crypto::hpke` module at all. Calling
`rustls::client::ClientConfig::with_ech(..)` is gated on
supplying `&[&'static dyn rustls::crypto::hpke::Hpke]`,
and ring does not produce that slice.

Three options were considered:

1. **Switch the affected transports to `aws_lc_rs`.**
   Mechanical change but breaks the explicit "stay on
   ring" rationale captured in
   [ADR-0004](./0004-anti-censorship.md) and reinforced
   by the REALITY rng-hook landing
   ([S-5-reality-rng-hook](../security/transport.md#t-13)),
   which hard-codes
   `rustls::crypto::ring::default_provider()` as its
   base. A provider swap also affects the cargo-deny
   posture and pulls aws-lc-sys into every Lakko binary.

2. **Write `lakko-hpke` from scratch on top of
   `ring::{agreement, hkdf, aead}`.** ~1000 lines of new
   audit-critical RFC 9180 implementation (DH-KEM, key
   schedule, AEAD nonce sequence, single-shot Seal/Open).
   Cleanest crypto-architecturally but a multi-sprint
   commitment with its own ext-audit budget.

3. **Adapter on RustCrypto/hpke.** Add `hpke = "0.13"`
   (MIT/Apache, ~3000 lines, 5.7M downloads, the
   reference impl that Mozilla / Cloudflare publications
   reference in their ECH literature) and write a thin
   adapter (~500 lines) that implements
   `rustls::crypto::hpke::Hpke` against it. RustCrypto
   primitives (`x25519-dalek`, `aes-gcm`,
   `chacha20poly1305`) are already in our dep tree via
   `lakko-crypto`, so the marginal dep surface is just
   the `hpke` crate itself.

## Decision

Adopt option 3 — RustCrypto/hpke adapter — and keep the
`ring` crypto provider in `lakko-transport`.

Crate dep: `hpke = "0.13"` with features `std + x25519`
(no defaults — defaults pull P-256, which we do not need
since Cloudflare's published ECH suite is X25519-only).

Adapter lives in
[`crates/lakko-transport/src/ech/adapter.rs`](../../crates/lakko-transport/src/ech/adapter.rs).
Surface: three unit structs
(`LakkoHpkeSuiteAesGcm128` / `AesGcm256` /
`ChaCha20Poly1305`), each implementing
`rustls::crypto::hpke::Hpke` for the
DH-KEM-X25519-HKDF-SHA256 KEM paired with the matching
AEAD. They are exposed as
`ALL_SUPPORTED_SUITES: &[&'static dyn Hpke]` which feeds
directly into `rustls::client::EchConfig::new(bytes,
&ALL_SUPPORTED_SUITES)`.

A two-stage validation pipeline (in
[`src/ech/wireup.rs`](../../crates/lakko-transport/src/ech/wireup.rs))
runs every attacker-touchable ECH payload through:

1. Our T-12 parser
   ([`parse_ech_config_list`](../../crates/lakko-transport/src/ech/mod.rs))
   — version-pinned (0xfe0d only), bounded allocations.
2. `rustls::client::EchConfig::new` — selects a
   compatible suite from `ALL_SUPPORTED_SUITES`. If
   nothing matches the published config, refuses
   loudly. No silent downgrade to GREASE.

The wire-up is plumbed into the two HTTPS-based pluggable
transports:

| Transport | Wire-up | Mechanism |
|-----------|---------|-----------|
| `HttpTransport` | landed | `HttpTransportConfig::ech_config_list: Option<Vec<u8>>`; `with_config` switches from `ClientConfig::builder()` to `builder_with_provider(...).with_ech(mode)?`. The existing `PinnedVerifier` still drives cert validation. |
| `WebTransportPt` | landed | `WebTransportConfig::ech_config_list_b64: Option<String>` (serde-default for backwards compat). When set, `open_session` bypasses `web-transport-quinn::ClientBuilder` (which does not expose `with_ech`) and assembles the rustls + quinn::ClientConfig manually. Ships `CertHashVerifier` — a Lakko-side replica of the W3C `serverCertificateHashes` semantics. **Refuses ECH-without-cert-pins at parse time** (`EchRequiresCertHashes`): under ECH the outer cert is the cover-site's, and silently falling back to system CA roots there would let a compromised CA fronting the cover SNI MITM the whole transport. |
| `RealityTransport` | **not applicable** | REALITY and ECH are mutually exclusive — both want to own the SNI / `legacy_session_id` slot of the outer ClientHello (REALITY embeds a HMAC authenticator there; ECH replaces SNI entirely with an HPKE-sealed inner ClientHello). A client cannot do both on the same handshake — and REALITY already provides SNI-hiding via its own mechanism. The caller picks one transport, not both. |

## Trust contract

- **Cryptographic guarantee** comes from the `hpke`
  crate; the adapter only re-shapes its input/output
  types. Any logic bug in the adapter (wrong type
  parameter, swapped sealer/opener) surfaces as a
  round-trip test failure rather than a silent leak.
- **Marshalling** of `HpkePublicKey` / `HpkePrivateKey` /
  `EncapsulatedSecret` round-trips through hpke's
  `Serializable` / `Deserializable` traits, the same
  bytes RFC 9180 wire format mandates.
- **RNG**: `rand_core::OsRng` wrapped in
  `UnwrapErr<OsRng>`. `rand_core` 0.9 made `OsRng`
  fallible-only (`TryCryptoRng`); `hpke` 0.13 still
  takes the infallible bound, so we adapt with the
  standard wrapper. A panic from the wrapper means the
  kernel CSPRNG itself broke, in which case TLS is no
  longer safe — surfacing the panic immediately is the
  right hygiene.
- **No silent downgrade** per security invariant #4:
  every error variant on the wire-up path
  (`EchWireUpError::ParserRefused`,
  `EchWireUpError::RustlsRefused`,
  `HttpError::EchConfig`,
  `WebTransportConfigError::BadEchConfigList`,
  `WebTransportConfigError::EchRequiresCertHashes`)
  refuses to build the transport, not to fall back to
  non-ECH.

## Internal-API caveat

rustls 0.23 surfaces the `HpkeKem` / `HpkeKdf` / `HpkeAead`
/ `HpkeSymmetricCipherSuite` enums only via its
`#[doc(hidden)] pub mod internal` path. That path is
documented as the consumer surface for external
crypto-provider adapters — exactly our use case — but it
is explicitly excluded from rustls's stable interface. A
rustls major-version bump may require re-pointing those
imports in `adapter.rs`. This is an accepted tradeoff
against the alternative of adding aws-lc-rs or rewriting
HPKE; the rebreak path is well-localised (single
`use` block in `adapter.rs`).

## Test coverage

- **Adapter** ([`adapter.rs::tests`](../../crates/lakko-transport/src/ech/adapter.rs))
  — 9 tests: round-trip per suite (3), stateful round-
  trip per suite (3), wrong-AAD-fails-open (1), IANA
  suite-ID drift (1), preference-order of
  `ALL_SUPPORTED_SUITES` (1).
- **Wire-up** ([`wireup.rs::tests`](../../crates/lakko-transport/src/ech/wireup.rs))
  — 4 tests: empty payload refused, malformed inner
  config refused, valid one-config list assembles into
  `EchMode::Enable`, unsupported KEM (P-256) refused by
  rustls's matcher.
- **HTTP** ([`http.rs::tests`](../../crates/lakko-transport/src/http.rs))
  — 2 new tests: malformed ECH bytes refused at
  construction, empty ECH bytes refused at construction.
- **WebTransport** ([`webtransport_pt.rs::tests`](../../crates/lakko-transport/src/webtransport_pt.rs))
  — 5 new tests: valid ECH + cert pins accepted, ECH
  without cert pins refused, malformed ECH bytes
  refused, bad ECH base64 refused, ECH-armed config
  round-trips through serde.
- **Fuzz** —
  [`crates/lakko-transport/fuzz/fuzz_targets/ech_wireup_build_mode.rs`](../../crates/lakko-transport/fuzz/fuzz_targets/ech_wireup_build_mode.rs)
  runs the full parse → assembly chain on
  attacker-controlled bytes. The pre-existing
  `ech_decode` target still covers the parser in
  isolation.

## Consequences

- **Positive.** ECH SNI privacy lands on HTTP and
  WebTransport without disturbing the existing ring
  crypto stack. The adapter's audit surface is bounded:
  ~500 lines of marshalling on top of a widely-used
  pure-Rust HPKE implementation; the cryptographic
  primitives themselves are already in the Lakko dep
  tree.
- **Negative.** A second-source HPKE impl is now in the
  trusted code base (RustCrypto/hpke + our adapter,
  separate from `lakko-channels::subscribe` which rolls
  its own sealed-envelope construction on
  `lakko-crypto`). External crypto audit needs to cover
  the adapter pre-public-beta. We rely on rustls's
  `internal` re-export path, which is documented as
  out-of-stable-API; a rustls major bump may need a
  small port.
- **Open.** Live integration test against a real
  ECH-publishing endpoint (Cloudflare) is gated on the
  censorship-lab sprint (G5-7); current coverage is
  unit + integration + fuzz on synthetic vectors.

## Alternatives revisited if circumstances change

- If rustls ever moves HPKE into the `ring` provider
  (or into a provider-neutral module), drop the adapter
  and switch to the upstream surface.
- If the `hpke` crate goes unmaintained, the adapter
  shape stays — the dep underneath can be swapped to
  any RFC 9180-compliant successor.

## References

- [`crates/lakko-transport/src/ech/`](../../crates/lakko-transport/src/ech/) — adapter + wireup + parser.
- [`docs/security/transport.md`](../security/transport.md) T-12 — audit-pinned invariant.
- [RustCrypto/hpke crate](https://crates.io/crates/hpke) — `hpke = "0.13"`.
- [draft-ietf-tls-esni-18](https://datatracker.ietf.org/doc/draft-ietf-tls-esni/) — ECH spec.
- [W3C `serverCertificateHashes`](https://www.w3.org/TR/webtransport/#dom-webtransportoptions-servercertificatehashes) — WebTransport cert-pinning semantics our `CertHashVerifier` replicates.
