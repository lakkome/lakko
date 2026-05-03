<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko

A secure messenger with **E2EE by default** for everything: 1×1 chats, groups, channels, audio / video calls, audio rooms and broadcasts. The server is zero-trust — even on full compromise it cannot decrypt messages, history or calls, and does not expose the social graph.

> **Status: Phase 0 — Foundations.** The repository is under active development. Do not use it for real communication.

## Key properties

- **MLS (RFC 9420)** for every conversation type — a single E2EE protocol
- **SFrame (RFC 9605)** for media encryption in calls, keys derived from an MLS exporter
- **Zero PII** — phone numbers and emails are never stored; only usernames for discovery
- **Discovery without exposing the graph** — username lookup via OPRF blinded queries
- **Hardware-backed device keys** on every platform
- **Anti-censorship transport** — REALITY, MASQUE, Shadowsocks-2022, WebTransport
- **All clients** sharing one Rust core: iOS / iPadOS / macOS / watchOS / Android / Windows / Linux / Web

## Architecture

The full plan lives in `~/.claude/plans/1-nested-wreath.md`. Architectural decisions live in `docs/adr/`. Repo navigation lives in `CLAUDE.md`.

## License

[AGPLv3](LICENSE). All contributions are accepted under a [CLA](CLA.md).

## Security

Vulnerabilities — only via GPG-encrypted email, **not** via public issues. The address and the key live in `SECURITY.md` (lands by Phase 1).
