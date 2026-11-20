<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko

A secure messenger with **E2EE by default** for everything: 1×1 chats, groups, channels, and audio / video calls. The server is designed to be zero-trust: it sees only ciphertext, stores no phone numbers or email addresses, and does not hold the social graph.

> **Status: public beta, capped at 1000 users. The cryptography has not been independently audited yet.**
> Lakko is built and reviewed by its author; an external security review is planned and is the condition for leaving the beta ([ADR-0046](docs/adr/0046-external-crypto-review.md), [scope](docs/security/external-review-scope.md)). Until then, do not rely on Lakko where your safety depends on it.

## Key properties

- **MLS (RFC 9420)** for every conversation type — a single E2EE protocol
- **SFrame (RFC 9605)** for media encryption in calls, keys derived from an MLS exporter
- **No phone numbers or email addresses required** — accounts are found by username
- **Discovery without exposing the graph** — username lookup via OPRF blinded queries
- **Key transparency** — identity keys are published in a log designed for witness co-signing
- **Hardware-backed device keys** where the platform has them
- **Censorship-resistant transports** — REALITY, Shadowsocks-2022, WebTransport (native clients)
- **One Rust core** shared by the iOS, Android, web and desktop clients

## What works today

[`docs/PRODUCT-READINESS.md`](docs/PRODUCT-READINESS.md) is the honest, per-feature account of what an ordinary user can rely on, and on which platform. Architectural decisions live in [`docs/adr/`](docs/adr/); repository navigation lives in [`CLAUDE.md`](CLAUDE.md).

## License

[AGPLv3](LICENSE).

## Security

Please report vulnerabilities privately, **not** through public issues. The address and the key live in [`SECURITY.md`](SECURITY.md). The known open issues are listed in [`docs/security/known-gaps.md`](docs/security/known-gaps.md).

## Funding

Lakko is an independent project. How to support it, and what the money pays for, is in [`funding.json`](funding.json).
