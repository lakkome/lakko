<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Lakko

A messenger where everything is end-to-end encrypted by default: one-to-one chats, groups, channels, and voice and video calls. The servers are designed to be zero-trust: they relay and store ciphertext, need no phone number or email address to create an account, and do not keep a list of who talks to whom.

> **Status: public beta, capped at 1000 users. The cryptography has not been independently audited yet.**
> Lakko is built and reviewed by its author; an external security review is planned and is the condition for leaving the beta ([ADR-0046](docs/adr/0046-external-crypto-review.md), [scope](docs/security/external-review-scope.md)). Until then, do not rely on Lakko where your safety depends on it.

## What it does

- **Chats and groups** — text, photos, videos, files, voice messages and video notes; replies, forwards, edits, deletion for everyone, reactions, pins, typing and read indicators.
- **Channels** — one-to-many broadcasts that subscribers read and react to.
- **Calls** — one-to-one and group voice and video calls; group calls go through Lakko's own SFU, which forwards media it cannot decrypt.
- **Several devices per account** — phone, tablet, desktop and browser at once; a new device is added from one you already have.
- **Recovery** — a 12-word recovery phrase, a passkey, or a recovery email.
- **Finding people** — by `@username` or an invite link; who can find you is your choice.

Not every feature is equally finished on every platform. [`docs/PRODUCT-READINESS.md`](docs/PRODUCT-READINESS.md) is the per-feature, per-platform account of what an ordinary user can rely on today.

## Clients

| Platform | Where | Notes |
|---|---|---|
| Web | [`apps/web`](apps/web) | SolidJS + TypeScript, the Rust core compiled to WebAssembly; also installs as a PWA |
| Desktop | [`apps/desktop`](apps/desktop) | A Tauri host around the web client that keeps device keys in the OS key store ([ADR-0085](docs/adr/0085-desktop-client.md)); runs on macOS; the Windows and Linux builds have not yet been run on those systems |
| iOS / iPadOS | [`apps/ios`](apps/ios) | SwiftUI, the Rust core through UniFFI |
| Android | [`apps/android`](apps/android) | Jetpack Compose, the Rust core through UniFFI |

All clients share one Rust core ([ADR-0002](docs/adr/0002-rust-core.md)).

## Security properties

- **MLS (RFC 9420)** for every conversation: one-to-one, groups, channels and call keys ([ADR-0001](docs/adr/0001-mls-only.md)).
- **SFrame (RFC 9605)** for call media, keyed from the MLS group, so the SFU only ever forwards ciphertext.
- **No phone number or email address required**; username lookup uses OPRF-blinded queries, so the directory does not learn who you looked up.
- **Key transparency** — identity keys are published in an append-only log designed to be co-signed by independent witnesses ([ADR-0020](docs/adr/0020-witness-cosigning.md), [ADR-0039](docs/adr/0039-kt-log-witness-quorum.md)).
- **No fallback to unencrypted** — if a secure session cannot be set up, nothing is sent.
- **Hardware-backed device keys** where the platform has them (Secure Enclave, StrongBox, TPM).
- **Censorship-resistant transports** in the native clients — REALITY, Shadowsocks-2022 and WebTransport ([ADR-0026](docs/adr/0026-anti-censorship-wiring.md)).

The threat model, the cryptography overview and the list of known open issues are in [`docs/security/`](docs/security/README.md).

## Repository layout

| Path | Contents |
|---|---|
| [`crates/`](crates) | The Rust core: MLS, crypto, calls and SFrame, channels, contacts, sync, transport, and the FFI layers for Swift, Kotlin and WebAssembly |
| [`services/`](services) | Rust backend services: auth, delivery service (`ds`), storage, discovery, channels, push gateway, SFU, key transparency and witness, moderation, GIF proxy, anti-censorship proxy |
| [`apps/`](apps) | The clients above, the moderators' panel, and C++ shared by the native SFrame bridges |
| [`schemas/`](schemas) | Protobuf wire formats — the single source of truth for messages between components |
| [`infra/`](infra/README.md) | Deployment: systemd units, nginx, MariaDB, coturn, monitoring, and a Docker Compose stack for local runs |
| [`docs/`](docs) | Architecture decision records, security documentation, UX specifications, roadmap |
| [`tools/`](tools), [`ci/`](ci) | Developer tooling and CI harnesses |

## Building and running

Prerequisites: the Rust toolchain (pinned in [`rust-toolchain.toml`](rust-toolchain.toml); `rustup` installs it on first use), [`just`](https://github.com/casey/just), Node.js with npm for the web and desktop clients, and Docker for a local database or the full local stack. iOS needs Xcode; Android needs the Android SDK and NDK.

```bash
just check          # type-check the Rust workspace
just test           # run the Rust tests
just ci             # everything CI runs: format, lint, tests, audits, fuzz build, web checks
```

Run the backend locally with Docker Compose ([details](infra/docker/README.md)), then a client:

```bash
just stack-up       # auth, ds, storage, discovery, push-gw and channels on 127.0.0.1
just dev-web        # web client dev server
just dev-desktop    # desktop app around a fresh web build
just build-ios      # Rust core for iOS (then build apps/ios in Xcode)
just build-android  # Rust core for Android (then build apps/android with Gradle)
```

`just --list` shows every recipe. Each client's README covers its own setup in more detail.

## Documentation

- [`docs/adr/`](docs/adr/README.md) — architecture decisions, one record per decision.
- [`docs/PRODUCT-READINESS.md`](docs/PRODUCT-READINESS.md) — what works, for whom, on which platform.
- [`docs/security/`](docs/security/README.md) — threat model, invariants, known gaps.
- [`docs/ux/`](docs/ux) — user-facing behaviour, screen by screen.
- [`CLAUDE.md`](CLAUDE.md) — a detailed map of the repository.

## License

[AGPLv3](LICENSE) ([ADR-0006](docs/adr/0006-licensing.md)).

## Security

Please report vulnerabilities privately, **not** through public issues. The address and the key are in [`SECURITY.md`](SECURITY.md). Known open issues are listed in [`docs/security/known-gaps.md`](docs/security/known-gaps.md).

## Funding

Lakko is an independent project. How to support it, and what the money pays for, is in [`funding.json`](funding.json).
