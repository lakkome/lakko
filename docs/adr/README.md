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
