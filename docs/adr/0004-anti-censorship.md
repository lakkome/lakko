<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0004: Pluggable transports for anti-censorship

- **Status:** Accepted — the transport *set* below is narrower in practice, and
  three later ADRs amend this one; see the note (documentation audit,
  2026-09-02).
- **Date:** 2026-04-28
- **Context:** Phase 0 (decision pinned in advance, implementation lands in Phase 5)

> **AMENDED 2026-09-02 (documentation audit).** The strategy holds: multiple
> obfuscated transports, REALITY as the anchor, a DoH-fetched signed bootstrap
> config, happy-eyeballs selection, no VPN-extension dependency. **Four things
> below no longer describe the tree**, each marked where it appears — three
> transports that were never built (VLESS, obfs4, domain fronting), the iOS
> embedding mechanism, the "transports as data" claim, and two wrong RFC
> citations.
>
> **The single most important correction is not a naming slip:** this ADR lists
> WebTransport as the transport **"for the web client"**, and
> [ADR-0026](./0026-anti-censorship-wiring.md) later established that a browser
> **cannot** run any of these — the sandbox forbids raw sockets. **Anti-censorship
> is a native-only feature**; browser web stays on direct HTTPS by design.
> A reader taking this ADR at face value would expect a censorship-resistant
> web client that does not and will not exist.
>
> **Amended by:** [ADR-0012](./0012-ech-hpke-adapter.md) (how ECH was actually
> wired, and why REALITY is excluded from it),
> [ADR-0025](./0025-bootstrap-transport-dialer.md) (the runtime dispatcher that
> turns a verified config into a dialed transport), and
> [ADR-0026](./0026-anti-censorship-wiring.md) (the end-to-end wiring, the
> server-side obfs proxy, and the browser exclusion). None of the three was
> linked from here until this audit.

## Context

Lakko must work in Russia / Iran / China and other regions with DPI-based blocking. The adversary can:

- Block by SNI / IP / ASN
- Apply active probing (Great Firewall style)
- Disable UDP (anti-QUIC)
- Dynamically block domains

Domain fronting (encrypting the SNI to one domain + Host header to another via a CDN) used to work, but Cloudflare (2015), Fastly (2018), Google Cloud (2018) all officially shut it down. Relying on it is dangerous.

## Decision

**Multi-transport architecture:** the client ships a set of pluggable transports and picks the working one at runtime via happy-eyeballs racing.

### Edge listeners (everything listens on 443)

- **TLS 1.3 + ECH** (~~RFC 9460/9525~~ **draft-ietf-tls-esni-18** — corrected
  2026-09-02: RFC 9460 is SVCB/HTTPS RRs and RFC 9525 is TLS service identity;
  ECH is still an IETF draft and `crates/lakko-transport/src/ech/` implements
  draft-18, pinning `ECH_VERSION_DRAFT_13` = `0xfe0d`. See
  [ADR-0012](./0012-ech-hpke-adapter.md), which cites it correctly.) — TCP/443
- **QUIC v1** ([quinn](https://github.com/quinn-rs/quinn)) — UDP/443
- **WebSocket-over-TLS** — TCP/443 fallback
- **MASQUE / CONNECT-UDP** (RFC 9298) — tunnels QUIC inside HTTP/3, defeats UDP blocking

### Pluggable transports (in priority order)

1. **REALITY** ([xtls/REALITY](https://github.com/XTLS/REALITY)) — TLS-in-TLS hiding behind a real third-party certificate; defeats SNI blocking + active probing. Key transport against the GFW.
2. **Shadowsocks-2022** with AEAD-2022 ciphers
3. ~~**VLESS + XTLS-Vision** — alternative to REALITY~~
4. **WebTransport** — H3-based, ~~for the web client~~
5. ~~**obfs4** — legacy, for compatibility with Tor-like environments~~
6. ~~**Domain fronting** via Cloudflare / Fastly / Akamai / CloudFront — multi-CDN, weighted random; considered deprecated, end-of-life planned, not a foundation~~

> **Three of these six were never built, and one is wired to a different client
> than stated (marked 2026-09-02).** `PluggableKind` has exactly five variants —
> `Reality`, `Shadowsocks2022`, `WebTransport`, `Masque`, `Quic`
> ([`pluggable.rs`](../../crates/lakko-transport/src/pluggable.rs)) — and
> `TransportSpec` four.
>
> - **VLESS + XTLS-Vision** — no module, no variant, no mention anywhere. Never
>   started. Reasonable: it overlaps REALITY, which was built.
> - **obfs4** — survives only as a stale name-drop in a `lakko-transport`
>   doc-comment. No module, no variant.
> - **Domain fronting** — appears nowhere in the codebase. Consistent with this
>   ADR calling it "not a foundation", but it is listed as a shipped transport
>   rather than as a rejected one, which is what makes the list misleading.
> - **WebTransport is NOT for the web client.** This is the one that matters:
>   [ADR-0026](./0026-anti-censorship-wiring.md) established that a **browser
>   cannot open raw-socket transports at all**, so anti-censorship is a
>   **native-only** feature and browser web stays on direct HTTPS. WebTransport
>   was built and is wired — into iOS/Android and the `services/obfs-proxy`
>   listener set, not into the browser.
>
> **MASQUE deserves its own footnote:** it is built and listed under Edge
> listeners above, but it is **not wired into `services/obfs-proxy`**, because
> CONNECT-UDP carries unreliable, unordered datagrams and cannot carry a
> reliable HTTP service. See ADR-0026 § Components 1.

### Bootstrap rendezvous (chicken-and-egg)

1. Hardcoded DoH resolvers (Cloudflare, Google, Quad9, ~~Mullvad~~ **AdGuard**) →
   resolve a rotating pool of innocuous-looking domains. *(Corrected
   2026-09-02 against
   [`doh_fetch.rs`](../../crates/lakko-transport/src/doh_fetch.rs); the same
   stale "Mullvad" was copied into `rendezvous.rs`'s module doc and is
   reported, not fixed here.)*
2. Each domain returns an **ECH-protected signed bootstrap config** (Ed25519, JSON: current endpoints + working PT configs)
3. Fallback: DNS-over-Tor → static anycast IPs of major CDNs → out-of-band QR code for extreme cases

### Auto-discovery

Happy-eyeballs racing of several transports in parallel; the top two working ones are kept warm; switch on degradation; budget ≤ 10 seconds cold start.

### Mobile constraints

- **iOS:** pluggable transports are embedded ~~inside URLSession + QUIC~~ **in a Rust FFI call driving its own tokio runtime over raw sockets**, **not** via NEPacketTunnelProvider (50 MB memory cap, no background CPU). An optional VPN mode is possible, but not the default.

  > **Corrected 2026-09-02.** The "not NEPacketTunnelProvider" half held —
  > there is no such extension anywhere. The "inside URLSession" half did not:
  > URLSession cannot have a `Box<dyn Connection>` injected beneath its TLS,
  > which [ADR-0026](./0026-anti-censorship-wiring.md) names explicitly as the
  > reason it rejected that approach. When armed, iOS calls the FFI
  > `tunneledHttpRequest`
  > ([`AntiCensorshipTransport.swift`](../../apps/ios/Lakko/Transport/AntiCensorshipTransport.swift));
  > **URLSession is now only the disarmed / direct path.** Android is as
  > written — in-app transport by default, via a custom Ktor engine.
- **Android:** VpnService is available, but the default is in-app transport.

### Pluggable transports as **data**, not as code

Transport configs are signed via [TUF (The Update Framework)](https://theupdateframework.io/) and delivered through signed updates. A new transport can be added without recompiling the client. Old configs rotate automatically.

> **Not what shipped, in both halves (marked 2026-09-02) — and the second half
> is a design property, not a missing feature.**
>
> **It is not TUF.** The bootstrap config is plain JSON with a **bare Ed25519
> signature over a canonical serialisation**, verified against a binary-pinned
> trust anchor (`BootstrapConfig` + its 4-stage `verify` in
> [`rendezvous.rs`](../../crates/lakko-transport/src/rendezvous.rs));
> `lakko-transport` does not depend on `lakko-tuf` at all. `crates/lakko-tuf`
> exists and is a real TUF v1.1 verifier, but it is aimed at **binary-artifact**
> updates (the sideloaded/desktop self-swap flow), and even there it is
> currently unwired — see [ADR-0038](./0038-update-available-notice.md), which
> deliberately did *not* use it for the store-app update notice.
>
> **A new transport cannot be added without recompiling.** `TransportSpec` is a
> **closed Rust enum of four variants**, and `BootstrapDialer::build_candidates`
> matches on it exhaustively
> ([ADR-0025](./0025-bootstrap-transport-dialer.md)). What the signed config
> genuinely delivers as data is the **configuration and ordering** of the
> transports the client already knows — endpoints, keys, short-ids, priority.
> That is the operationally important half (rotating a burned REALITY endpoint
> needs no release), and it is worth being precise that the *code* half is not
> there, because "add a transport without recompiling" would otherwise imply a
> plugin surface that does not exist — and that this project would not want,
> since an operator-signed config that could introduce executable transport
> behaviour is a much larger trust grant than one that reconfigures a
> fixed set.

## Alternatives

### Domain fronting only

Cons: CDN providers systematically disable it; policy is variable; SNI extraction makes fronting visible.

**Rejected:** not a foundation in 2026+. We treat it as a bonus, not the base.

### Tor only / obfuscated Tor only

Cons: high latency is incompatible with realtime calls; Tor exit nodes are blocked en masse.

**Rejected:** Tor is unsuitable for realtime media. We can use it as an escape hatch for control-plane messages.

### Vanilla QUIC without obfuscation

Cons: the GFW and TSPU block QUIC by packet characteristics; SNI / ESNI still needs to be hidden.

**Rejected:** insufficient against active censors.

### A fully custom obfuscated protocol

Cons: it would be detected and blocked quickly because of its distinctive patterns — there would be only one such protocol in the wild.

**Rejected:** mimicking already widely deployed protocols (TLS handshake, HTTP/3) is safer than a unique pattern.

## Consequences

### Positive

- The adversary must block everything that Lakko mimics (Cloudflare, Apple iCloud, …) — politically expensive
- Pluggable transports as signed data → new transports can be added in production without a code release
- Multi-CDN bootstrap reduces single point of failure
- TUF-signed configs guard against malicious substitution of transport configs

### Negative

- A constant arms race: the adversary learns to detect, we add new transports
- Multi-transport racing adds 2–5 seconds to cold start latency
- iOS Network Extension limitations do not allow a full VPN mode without a UX compromise
- In extreme regions (DPRK-style allowlist-only internet) the service is unavailable — abandon plan: we do not engineer for that case

### Neutral

- The TUF pipeline for transport configs requires separate key management (separate from release signing)

## Open questions

- How do we monitor production transport effectiveness without de-anonymising users? Telemetry-without-PII via differential privacy — Phase 5
- Should we embed Tor as the ultimate fallback for the control plane (signaling, push)? Phase 5
- A stockpile of "innocuous" domains for bootstrap — needs continuous rotation, separate legal / operational resource

## References

- [REALITY — XTLS](https://github.com/XTLS/REALITY)
- [RFC 9298 — CONNECT-UDP / MASQUE](https://www.rfc-editor.org/rfc/rfc9298)
- ~~[RFC 9460 — Encrypted Client Hello]~~ → **[draft-ietf-tls-esni](https://datatracker.ietf.org/doc/draft-ietf-tls-esni/)**
  — ECH is a draft, not an RFC; RFC 9460 is SVCB/HTTPS RRs (corrected 2026-09-02)
- [TUF — The Update Framework](https://theupdateframework.io/)
- [GFW Report — censorship research](https://gfw.report/)
- ADR-0002 — clients (where pluggable transports live)
- [ADR-0012](./0012-ech-hpke-adapter.md) — ECH wire-up via a RustCrypto/hpke
  adapter (added 2026-09-02)
- [ADR-0025](./0025-bootstrap-transport-dialer.md) — the `BootstrapDialer` that
  drives the transports this ADR chose (added 2026-09-02)
- [ADR-0026](./0026-anti-censorship-wiring.md) — the end-to-end wiring, and the
  decision that browser web is excluded (added 2026-09-02)
