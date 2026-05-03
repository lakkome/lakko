<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# ADR-0004: Pluggable transports for anti-censorship

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Phase 0 (decision pinned in advance, implementation lands in Phase 5)

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

- **TLS 1.3 + ECH** (RFC 9460/9525) — TCP/443
- **QUIC v1** ([quinn](https://github.com/quinn-rs/quinn)) — UDP/443
- **WebSocket-over-TLS** — TCP/443 fallback
- **MASQUE / CONNECT-UDP** (RFC 9298) — tunnels QUIC inside HTTP/3, defeats UDP blocking

### Pluggable transports (in priority order)

1. **REALITY** ([xtls/REALITY](https://github.com/XTLS/REALITY)) — TLS-in-TLS hiding behind a real third-party certificate; defeats SNI blocking + active probing. Key transport against the GFW.
2. **Shadowsocks-2022** with AEAD-2022 ciphers
3. **VLESS + XTLS-Vision** — alternative to REALITY
4. **WebTransport** — H3-based, for the web client
5. **obfs4** — legacy, for compatibility with Tor-like environments
6. **Domain fronting** via Cloudflare / Fastly / Akamai / CloudFront — multi-CDN, weighted random; considered deprecated, end-of-life planned, not a foundation

### Bootstrap rendezvous (chicken-and-egg)

1. Hardcoded DoH resolvers (Cloudflare 1.1.1.1, Google 8.8.8.8, Quad9, Mullvad) → resolve a rotating pool of innocuous-looking domains
2. Each domain returns an **ECH-protected signed bootstrap config** (Ed25519, JSON: current endpoints + working PT configs)
3. Fallback: DNS-over-Tor → static anycast IPs of major CDNs → out-of-band QR code for extreme cases

### Auto-discovery

Happy-eyeballs racing of several transports in parallel; the top two working ones are kept warm; switch on degradation; budget ≤ 10 seconds cold start.

### Mobile constraints

- **iOS:** pluggable transports are embedded inside URLSession + QUIC, **not** via NEPacketTunnelProvider (50 MB memory cap, no background CPU). An optional VPN mode is possible, but not the default.
- **Android:** VpnService is available, but the default is in-app transport.

### Pluggable transports as **data**, not as code

Transport configs are signed via [TUF (The Update Framework)](https://theupdateframework.io/) and delivered through signed updates. A new transport can be added without recompiling the client. Old configs rotate automatically.

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
- [RFC 9460 — Encrypted Client Hello](https://www.rfc-editor.org/rfc/rfc9460)
- [TUF — The Update Framework](https://theupdateframework.io/)
- [GFW Report — censorship research](https://gfw.report/)
- ADR-0002 — clients (where pluggable transports live)
