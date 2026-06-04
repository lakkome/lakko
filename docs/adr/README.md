<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Architecture Decision Records

ADR — Architecture Decision Record. Each file captures **one** significant decision, its motivation, the alternatives considered, and the consequences. Decisions are immutable: if our position changes, we write a new ADR with status `Supersedes 000X` and mark the old one `Superseded by 000Y`.

## Format

See [`0000-template.md`](0000-template.md). Style — Michael Nygard, minimalist.

## Index

| # | Status | Title |
|---|---|---|
| [0001](0001-mls-only.md) | Accepted | MLS (RFC 9420) for every conversation type |
| [0002](0002-rust-core.md) | Accepted | Shared Rust core + native UI on every platform |
| [0003](0003-livekit-fork.md) | Accepted | LiveKit fork as the SFU, SFrame for media |
| [0004](0004-anti-censorship.md) | Accepted | Pluggable transports: REALITY + MASQUE + Shadowsocks-2022 |
| [0005](0005-contact-invitations.md) | Accepted | Capability-based invites + 6 scenarios + pseudonym lifecycle |
| [0006](0006-licensing.md) | Accepted | AGPLv3 monorepo + CLA |
| [0007](0007-multi-device-identity.md) | Accepted | Two-tier identity (account-level deterministic + per-device random) |
| [0008](0008-livekit-fork-strategy.md) | Accepted | LiveKit fork — operational strategy (extends 0003) |
| [0009](0009-link-based-call-invitations.md) | Accepted | Link-based call invitations protocol (extends 0005) |
| [0010](0010-channels-two-tier-broadcast.md) | Accepted | Channels — two-tier PublisherGroup + sender-key broadcast (Phase 4 design) |
| [0011](0011-rust-sfu.md) | Accepted | Rust SFU on webrtc-rs / str0m — supersedes 0003 (SFU choice) + 0008 |
| [0012](0012-ech-hpke-adapter.md) | Accepted | ECH wire-up via RustCrypto/hpke adapter (HTTP + WebTransport); REALITY n/a |
| [0013](0013-sframe-cross-platform-aad.md) | Accepted | Cross-platform SFrame AEAD AAD = the RFC 9605 SframeHeader bytes |
| [0014](0014-webauthn-action-binding.md) | Proposed | WebAuthn action binding via a minimal webauthn-rs-core challenge-injection fork (G-13) |
