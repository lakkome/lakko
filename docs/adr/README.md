<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Lakko Contributors
-->

# Architecture Decision Records

ADR — Architecture Decision Record. Each file captures **one** significant decision, its motivation, the alternatives considered, and the consequences.

A **decision** is immutable: if our position changes, we write a new ADR with status `Supersedes 000X` and mark the old one `Superseded by 000Y`. The **record of it** is not — an ADR is amended in place when implementation reveals something the decision did not anticipate, or when a later ADR extends it. Amendments are marked (a dated `AMENDED` / `Update` note, or strikethrough) rather than silently rewritten, so the original reasoning stays readable beside the correction. The rule that matters: **never make the record look like it was always right.**

## Format

See [`0000-template.md`](0000-template.md). Style — Michael Nygard, minimalist.

## Index

Status here is the ADR's own `Status:` line, trimmed to one word. Read the ADR
for the qualifier — several `Accepted` rows carry per-sprint implementation
detail, and `Implemented` is used where the decision is fully landed.

| # | Status | Title |
|---|---|---|
| [0001](0001-mls-only.md) | Accepted | MLS (RFC 9420) for every conversation type |
| [0002](0002-rust-core.md) | Accepted | Shared Rust core + native UI on every platform |
| [0003](0003-livekit-fork.md) | **Superseded** by 0011 | LiveKit fork as the SFU, SFrame for media |
| [0004](0004-anti-censorship.md) | Accepted | Pluggable transports: REALITY + MASQUE + Shadowsocks-2022 |
| [0005](0005-contact-invitations.md) | Accepted | Capability-based invites + 6 scenarios + pseudonym lifecycle |
| [0006](0006-licensing.md) | Accepted | AGPLv3 monorepo + CLA |
| [0007](0007-multi-device-identity.md) | Accepted | Two-tier identity (account-level deterministic + per-device random) |
| [0008](0008-livekit-fork-strategy.md) | **Superseded** by 0011 | LiveKit fork — operational strategy (extended 0003) |
| [0009](0009-link-based-call-invitations.md) | Accepted | Link-based call invitations protocol (extends 0005) |
| [0010](0010-channels-two-tier-broadcast.md) | Accepted | Channels — two-tier PublisherGroup + sender-key broadcast (Phase 4 design) |
| [0011](0011-rust-sfu.md) | Accepted | Rust SFU on webrtc-rs / str0m — supersedes 0003 (SFU choice) + 0008 |
| [0012](0012-ech-hpke-adapter.md) | Accepted | ECH wire-up via RustCrypto/hpke adapter (HTTP + WebTransport); REALITY n/a |
| [0013](0013-sframe-cross-platform-aad.md) | Accepted | Cross-platform SFrame AEAD AAD = the RFC 9605 SframeHeader bytes |
| [0014](0014-webauthn-action-binding.md) | Accepted | WebAuthn action binding via a minimal webauthn-rs-core challenge-injection fork (G-13) |
| [0015](0015-device-removal-authorization.md) | Accepted | Device-removal authorization — peer-verified passkey attestation (G-13) |
| [0016](0016-identity-supersession-transport.md) | Accepted | Identity-supersession proof transport — the discovery identity bundle (Carrier B) |
| [0017](0017-phrase-seeded-identity.md) | Accepted | Phrase-seeded AccountIdentity — decouple identity from the password |
| [0018](0018-recovery-factor-cascade.md) | Accepted | Recovery factor cascade — ACCESS-only factors |
| [0019](0019-client-authorised-log-leaf.md) | Accepted | Client-authorised transparency-log leaf (G-30 #2) |
| [0020](0020-witness-cosigning.md) | Accepted | Witness co-signing for KT-log split-view resistance (G-30 #1) |
| [0021](0021-sfu-peer-identity-exposure.md) | Accepted | SFU per-call peer tag for group-call roster names |
| [0022](0022-group-admin-roles.md) | Accepted | Group-admin roles — owner / admin / member, peer-verified |
| [0023](0023-message-edit-delete-reactions.md) | Accepted | Message edit / delete / reactions |
| [0024](0024-typing-read-receipts.md) | Accepted | Typing indicators + read receipts |
| [0025](0025-bootstrap-transport-dialer.md) | Proposed | Bootstrap transport dialer |
| [0026](0026-anti-censorship-wiring.md) | Accepted | Anti-censorship wiring — end-to-end architecture |
| [0027](0027-external-join-approval.md) | Accepted | External-join proposal approval (G-3) |
| [0028](0028-group-join-ticket.md) | Accepted | Group join-ticket (invite-link for joiner-initiated external join) |
| [0029](0029-mid-call-key-rotation.md) | Accepted | Mid-call call-key rotation (overlap keyring) |
| [0030](0030-batched-rotation-fanout.md) | Accepted | Batched channel-rotation fanout — staged prepare + atomic commit |
| [0031](0031-multi-epoch-envelope-retention.md) | Accepted | Multi-epoch channel seed-envelope retention + subscriber catch-up |
| [0032](0032-multi-joiner-call-links.md) | Accepted | Multi-joiner shareable call-invite links — owner-founds + committer election |
| [0033](0033-call-admission-antispam-throttle.md) | Accepted | Call-admission anti-spam throttle for guest-account link joins |
| [0034](0034-sframe-per-media-stream-key.md) | Implemented; discriminator **superseded** by 0035 | Per-(leaf, media-type) SFrame key derivation |
| [0035](0035-sframe-per-stream-kid-key.md) | Implemented | Per-(leaf, stream) SFrame key derivation via the header KID — supersedes 0034's discriminator |
| [0036](0036-auth-attested-genesis.md) | Implemented | Auth-attested KT-log genesis (prevent first-entry squatting) |
| [0037](0037-backup-head-rollback-commitment.md) | Accepted | Auth-signed backup-head commitment (prevent fresh-install rollback) |
| [0038](0038-update-available-notice.md) | Accepted | Signed "update available" notice + store deep-link |
| [0039](0039-kt-log-witness-quorum.md) | Accepted | KT-log witness quorum — K-of-N co-signing (extends 0020) |
| [0040](0040-durable-pending-commit-outbox.md) | Accepted | Durable pending-commit outbox for crash-safe MLS group commits |
| [0041](0041-ios-silent-push-nse-decrypt.md) | Accepted | iOS silent-push background decrypt in the notification-service extension |
| [0042](0042-push-opaque-group-routing-id.md) | Accepted | Opaque per-group push routing id — close the push social-graph leak |
| [0043](0043-push-routing-secret-rotation-on-removal.md) | Accepted | Rotate the push routing secret on member removal (extends 0042) |
| [0044](0044-push-routing-secret-recency.md) | Accepted | Mint-epoch recency ordering for the push routing secret (extends 0042 + 0043) |
| [0045](0045-remote-commit-epoch-fork.md) | Proposed | Proposal-based membership commits with a designated committer (G-38 residual 1) |
| [0046](0046-external-crypto-review.md) | Accepted | External cryptographic review — a programme gate, not a per-change blocker |
| [0047](0047-media-messages-and-gif-proxy.md) | Accepted | Media messages, emoji picker and a server-side GIF proxy |
| [0048](0048-profile-names.md) | Accepted | Profile names, carried end-to-end encrypted and bound to the sender's key |
| [0049](0049-identity-recovery-word-vault.md) | Accepted | Identity recovery: three of the twenty-four words, a confirmation prompt on another device, and a passkey |
| [0050](0050-per-device-conversation-leaves.md) | Accepted | Per-device conversation leaves, a second device is a full client (ADR-0007 Phase 2b, G-40) |
| [0051](0051-group-and-channel-names.md) | Accepted | Group and channel names carried end-to-end encrypted to every member and subscriber |
| [0052](0052-reply-forward-pin.md) | Accepted | Reply, forward and pin |
| [0053](0053-ds-journal-authorisation.md) | Accepted | A journal on the Delivery Service is opened by a key, not by its name (G-42) |
| [0058](0058-chat-folders.md) | Accepted | Chat folders, carried between the account's own devices |
| [0059](0059-channel-discover.md) | Accepted | Discover: an opt-in channel directory, topic buckets, search, and channels shared in groups |
| [0061](0061-interface-localization.md) | Accepted | Interface localization — one catalog, built-in languages, signed updates that do not reveal the language |
| [0062](0062-conversation-kind.md) | Accepted | A conversation's kind is set in the group at creation, and a one-to-one keeps its two people |
| [0063](0063-on-device-feed.md) | Accepted | The feed is ranked on the device, by one shared rule engine |
| [0064](0064-conversation-backup-and-device-re-entry.md) | Accepted | Conversations come back on another device: saved history, and a device that re-enters by itself |
| [0065](0065-attachment-lifetime-and-access.md) | Accepted | Attachment files have a lifetime, an owner who can delete them, and a gate |
