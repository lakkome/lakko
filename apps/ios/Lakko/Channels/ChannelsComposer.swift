// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Thin Swift shim around the `LakkoCore` UniFFI surface
/// for `lakko-channels` (Phase 4 G4-2 iOS). Mirrors the
/// web shim at `apps/web/src/channels/composer.ts`.
///
/// Why this exists: the UniFFI-generated `LakkoCore.*`
/// types (Data ↔ Vec<u8>, UInt64 ↔ u64, Arc<...>
/// reference handles) are ergonomic enough for the call
/// sites, but a couple of paper cuts deserve hiding in
/// one place:
///
/// 1. The hex-string ↔ Data conversion the URL form of
///    `channelId` requires (32 hex chars ↔ 16 bytes).
/// 2. UTF-8 encode / decode for the publish form.
/// 3. Forwarding `ChannelSigningKey` lifetime through
///    every callsite instead of constructing one per
///    publish.
///
/// The shim is intentionally tiny — every method maps
/// 1:1 to a single UniFFI call so an auditor reading
/// this file can locate the underlying Rust surface in
/// `crates/lakko-ffi-uniffi/src/channels.rs` by name
/// without translating.

/// Local publisher keys for a single channel. Held in
/// `ChannelsUiState`; the `signingKeySeed` half is persisted
/// in the hardware-backed Keychain (``ChannelPublisherKeyStore``,
/// G-29) so a relaunch keeps signing under the same key
/// (invariant #7). The content-key `seed` re-derives from the
/// persisted MLS PublisherGroup exporter on each open.
public struct ChannelPublisherKeys: Equatable, Sendable {
    /// 32-byte content-key seed. **Demo only** — production
    /// seeds come from PublisherGroup
    /// `mls_exporter("lakko/channel/v1", channel_id, 32)`
    /// once the MLS-per-channel keying lands.
    public let seed: Data
    /// 32-byte Ed25519 signing-key seed.
    public let signingKeySeed: Data
}

/// Subscriber-side X25519 keypair (Phase 4b G4-3c).
/// The public half registers on
/// `POST /v1/channels/:id/subscribers`; the secret half
/// stays on the device + unwraps the per-subscriber HPKE
/// seed envelopes the publisher fans out on every
/// rotation.
///
/// Both halves are 32 bytes. The secret half is persisted by
/// `ChannelsUiState` in the hardware-backed Keychain
/// (``ChannelSubscriberKeyStore``, G-29) — never `UserDefaults`
/// plaintext (invariant #7).
public struct ChannelSubscriberKeys: Equatable, Sendable {
    /// 32-byte X25519 secret.
    public let secret: Data
    /// 32-byte X25519 public.
    public let publicKey: Data

    public init(secret: Data, publicKey: Data) {
        self.secret = secret
        self.publicKey = publicKey
    }
}

/// Errors the composer surfaces. Wraps both the UniFFI
/// `FfiError.Channels` variant + Swift-side framing
/// failures (hex decode, UTF-8 surprise) into one type
/// the UI can switch on.
public enum ChannelsComposerError: Error, Equatable {
    case badChannelIdHex(String)
    case badBase64(String)
    case ffi(String)
}

public enum ChannelsComposer {

    /// Build the publisher's `(seed, signingKeySeed)` pair
    /// for `channelIdHex`, deriving the content-key seed
    /// from a fresh MLS **PublisherGroup** exporter
    /// (G-27 / ADR-0010 Tier-1).
    ///
    /// Mirror of
    /// `apps/web/src/channels/composer.ts::freshPublisherKeys`.
    /// The flow is:
    ///
    /// 1. Build a transient `Group` with `identity` as the
    ///    founding member (`Group.create(identity:)`).
    /// 2. Call `group.exportSecret(identity:, label:
    ///    channelExporterLabel(), context: channel_id,
    ///    length: 32)` — produces 32 bytes domain-separated
    ///    by `"lakko/channel/v1"` + the 16-byte channel id.
    /// 3. Hand the bytes to
    ///    `channelSeedFromMlsExporterOutput` so the seed
    ///    crosses the FFI boundary through the audit-pack
    ///    surface (C-44) rather than a raw `Data` cast.
    ///
    /// The group is throwaway — it lives only for the
    /// duration of this call, exactly like the web shim.
    /// PublisherGroup persistence + rotation across
    /// composer calls lands in a follow-up sub-sprint
    /// (working name `G4-2-mls-persistence`) per the
    /// note in [`docs/security/known-gaps.md`](../../../../docs/security/known-gaps.md)
    /// G-27.
    ///
    /// The `signingKeySeed` is a long-lived per-channel publisher identity
    /// ([ADR-0010](../../../../docs/adr/0010-channels-two-tier-broadcast.md)) —
    /// NOT derived from the call-time exporter. When `existingSigningSeed`
    /// is supplied (the hardware-backed seed ``ChannelsUiState`` persisted on
    /// a prior open, G-29), the publisher key is reconstructed from it so this
    /// device keeps signing under the same key across a relaunch; otherwise a
    /// fresh seed is minted by the underlying iOS Security CSPRNG. The caller
    /// persists the returned `signingKeySeed` (via ``ChannelsUiState/setKeys``
    /// → ``ChannelPublisherKeyStore``) so a subsequent open reuses it.
    ///
    /// A corrupt / wrong-length `existingSigningSeed` falls back to a fresh
    /// seed rather than failing the open — the channel stays usable, and the
    /// caller re-persists the fresh seed.
    public static func freshPublisherKeys(
        identity: Identity,
        channelIdHex: String,
        existingSigningSeed: Data? = nil
    ) throws -> ChannelPublisherKeys {
        let channelId = try channelIdFromHex(channelIdHex)

        // G4-2-mls-persistence step 3 — load-or-create the
        // PublisherGroup keyed by `channelId`. The first
        // publish on this device mints + persists; every
        // subsequent publish reuses the same MLS state, so
        // the exporter output is stable across process
        // restarts. Subscribers who received a sealed seed
        // envelope in a prior epoch continue to decrypt new
        // posts.
        //
        // Replaces the throwaway-group path that lived here
        // before (and was explicitly flagged as a follow-up
        // in the G-27 closeout). See
        // `crates/lakko-mls/src/publisher_group.rs` for the
        // Rust-core invariant.
        let seedBytes: Data
        do {
            // ADR-0057 — a PublisherGroup founded here is marked, so its first
            // journal claim may reset one the same root claimed before.
            let group: PublisherGroup
            if let existing = try LakkoCore.publisherGroupLoad(identity: identity, channelId: channelId) {
                group = existing
            } else {
                group = try LakkoCore.publisherGroupOpenOrCreate(
                    identity: identity,
                    channelId: channelId
                )
                CoPublisher.markFounded(group)
            }
            seedBytes = try group.channelSeed(identity: identity)
        } catch let FfiError.Group(msg) {
            // Retired v1 state refuses here and cannot be repaired.
            if msg.contains(LegacyChannelV1.rustRefusalMarker) {
                throw LegacyChannelError(kind: .channel)
            }
            throw ChannelsComposerError.ffi(msg)
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        } catch let FfiError.Codec(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
        // The PublisherGroup surface returns the 32-byte
        // channel seed directly — no separate
        // `channelSeedFromMlsExporterOutput` hop needed (the
        // wrapper bakes in the exporter label + length +
        // context per
        // `lakko_mls::publisher_group::CHANNEL_EXPORTER_LABEL`).
        let seed = seedBytes

        // Reuse the persisted per-channel signing seed when present (G-29
        // cross-relaunch continuity) so subscribers keep verifying against the
        // same Ed25519 key; a corrupt / wrong-length persisted seed falls back
        // to a fresh key rather than failing the open.
        let signer: ChannelSigningKey
        if let existingSigningSeed,
           let reused = try? ChannelSigningKey.fromSeed(seed: existingSigningSeed) {
            signer = reused
        } else {
            signer = ChannelSigningKey.generate()
        }
        return ChannelPublisherKeys(
            seed: seed,
            signingKeySeed: signer.toSeed()
        )
    }

    /// Compose a signed encrypted ChannelPost. Returns
    /// the wire bytes ready for
    /// `ChannelsClient.publishPost`.
    public static func compose(
        keys: ChannelPublisherKeys,
        channelIdHex: String,
        epochId: UInt32,
        messageCounter: UInt64,
        plaintext: Data
    ) throws -> Data {
        let channelId = try channelIdFromHex(channelIdHex)
        let signer: ChannelSigningKey
        do {
            signer = try ChannelSigningKey.fromSeed(seed: keys.signingKeySeed)
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
        do {
            return try LakkoCore.composeChannelPost(
                seed: keys.seed,
                channelId: channelId,
                epochId: epochId,
                signingKey: signer,
                messageCounter: messageCounter,
                plaintext: plaintext
            )
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
    }

    /// Decode a base64url-encoded ChannelPost wire and return
    /// the epoch it was sealed under — its clear-header
    /// `epoch_id` (ADR-0031). A catching-up subscriber reads
    /// this to pick the seed matching the post's epoch out of
    /// its per-channel `epoch -> seed` map; a post whose epoch
    /// has no seed is skipped, never force-decrypted (invariant
    /// #4).
    ///
    /// Reads only the clear header — no seed needed, and no
    /// AEAD authentication happens here (that is
    /// ``verifyAndDecrypt(seed:channelIdHex:postWireB64Url:)``).
    public static func postEpoch(postWireB64Url: String) throws -> UInt32 {
        guard let wire = Data(base64URLNoPad: postWireB64Url) else {
            throw ChannelsComposerError.badBase64(postWireB64Url)
        }
        do {
            let post = try LakkoCore.decodeChannelPost(wire: wire)
            return post.epochId()
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
    }

    /// Verify + decrypt a base64url-encoded ChannelPost
    /// (the form `ChannelsClient.fetchPosts` returns).
    /// Returns the recovered plaintext bytes.
    public static func verifyAndDecrypt(
        seed: Data,
        channelIdHex: String,
        postWireB64Url: String
    ) throws -> Data {
        let channelId = try channelIdFromHex(channelIdHex)
        guard let wire = Data(base64URLNoPad: postWireB64Url) else {
            throw ChannelsComposerError.badBase64(postWireB64Url)
        }
        let post: ChannelPost
        do {
            post = try LakkoCore.decodeChannelPost(wire: wire)
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
        do {
            return try LakkoCore.verifyAndDecryptChannelPost(
                seed: seed,
                channelId: channelId,
                post: post
            )
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
    }

    // MARK: - Subscriber-side (G4-3c)

    /// Fresh subscriber X25519 keypair, backed by
    /// `getrandom` (Security framework on iOS).
    public static func freshSubscriberKeys() -> ChannelSubscriberKeys {
        let kp = ChannelSubscriberKeypair.generate()
        return ChannelSubscriberKeys(
            secret: kp.secret(),
            // `public` is a Swift reserved word — the
            // UniFFI-generated method name is
            // backtick-escaped, so the call site must
            // backtick-escape too.
            publicKey: kp.`public`()
        )
    }

    /// Publisher-side: seal the channel's 32-byte content-
    /// key seed against a subscriber's X25519 public key.
    /// Returns the 80-byte wire bytes ready for
    /// `ChannelsClient.deliverEnvelope`.
    public static func sealSubscriberSeedEnvelope(
        subscriberPub: Data,
        channelIdHex: String,
        epochId: UInt32,
        seed: Data
    ) throws -> Data {
        let channelId = try channelIdFromHex(channelIdHex)
        do {
            return try LakkoCore.sealSubscriberSeedEnvelope(
                subscriberPub: subscriberPub,
                channelId: channelId,
                epochId: epochId,
                seed: seed
            )
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
    }

    /// Subscriber-side: open a seed envelope with the
    /// local keypair. Returns the recovered 32-byte
    /// content-key seed — hand it to ``verifyAndDecrypt``
    /// to decode posts on the channel for this epoch.
    public static func openSubscriberSeedEnvelope(
        keys: ChannelSubscriberKeys,
        channelIdHex: String,
        epochId: UInt32,
        wire: Data
    ) throws -> Data {
        let channelId = try channelIdFromHex(channelIdHex)
        let kp: ChannelSubscriberKeypair
        do {
            kp = try ChannelSubscriberKeypair.fromSecret(secret: keys.secret)
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
        do {
            return try LakkoCore.openSubscriberSeedEnvelope(
                keypair: kp,
                channelId: channelId,
                epochId: epochId,
                wire: wire
            )
        } catch let FfiError.Channels(msg) {
            throw ChannelsComposerError.ffi(msg)
        }
    }

    /// Wire length of a seed envelope —
    /// `eph_pub(32) + sealed_seed(32) + tag(16) = 80`.
    /// Exposed for UI sanity checks.
    public static var seedEnvelopeLen: UInt32 {
        LakkoCore.channelSeedEnvelopeLen()
    }

    // MARK: - Helpers

    /// 32 hex chars ↔ 16 bytes.
    public static func channelIdFromHex(_ hex: String) throws -> Data {
        guard hex.count == 32 else {
            throw ChannelsComposerError.badChannelIdHex(
                "channel id must be 32 hex chars (got \(hex.count))"
            )
        }
        var bytes = Data(capacity: 16)
        var index = hex.startIndex
        for _ in 0..<16 {
            let next = hex.index(index, offsetBy: 2)
            let pair = hex[index..<next]
            guard let byte = UInt8(pair, radix: 16) else {
                throw ChannelsComposerError.badChannelIdHex(
                    "invalid hex byte '\(pair)' in channel id"
                )
            }
            bytes.append(byte)
            index = next
        }
        return bytes
    }

    /// Hex-encode bytes (lowercase, no separators).
    public static func bytesToHex(_ data: Data) -> String {
        data.map { String(format: "%02x", $0) }.joined()
    }

    /// Generate 16 fresh bytes + hex-render for use as a
    /// new channel id. Uses `SecRandomCopyBytes` via the
    /// runtime, never falls back to a non-CSPRNG source.
    public static func freshChannelIdHex() -> String {
        var bytes = Data(count: 16)
        let result = bytes.withUnsafeMutableBytes {
            SecRandomCopyBytes(kSecRandomDefault, 16, $0.baseAddress!)
        }
        precondition(result == errSecSuccess,
                     "SecRandomCopyBytes failed: \(result)")
        return bytesToHex(bytes)
    }
}

// MARK: - base64url-no-pad codec

private extension Data {
    /// Decode a base64url-no-pad string into `Data`. Returns
    /// `nil` on any parse failure.
    init?(base64URLNoPad s: String) {
        // Re-add padding + canonicalise to standard base64 so
        // Apple's `Data(base64Encoded:)` accepts it.
        var std = s
            .replacingOccurrences(of: "-", with: "+")
            .replacingOccurrences(of: "_", with: "/")
        let pad = (4 - (std.count % 4)) % 4
        std.append(String(repeating: "=", count: pad))
        guard let data = Data(base64Encoded: std) else { return nil }
        self = data
    }
}
