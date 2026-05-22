// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Owner-side composer orchestration for link-based call
/// invites (Phase 3c3-composer-helper).
///
/// Wraps the five-step "create a call link" pipeline into a
/// single async function so the UI doesn't have to know about
/// the call_id RNG, the one-time KeyPackage mint, the FFI
/// boundary, the discovery HTTP round-trip, or the URL
/// builder convention.
///
/// Sibling of `apps/web/src/calls/composer.ts` +
/// `apps/android/.../calls/CallInviteComposer.kt` — same
/// pipeline shape, platform-idiomatic surface.
public enum CallInviteComposer {

    // MARK: - Errors

    public enum Error: Swift.Error, LocalizedError {
        case wrongOwnerSigningSeedLength(expected: Int, got: Int)
        case wrongOwnerAccountIdLength(expected: Int, got: Int)
        case labelTooLong(maxBytes: Int, got: Int)
        case keyPackageEmpty

        public var errorDescription: String? {
            switch self {
            case .wrongOwnerSigningSeedLength(let expected, let got):
                return "ownerSigningSeed must be \(expected) bytes, got \(got)"
            case .wrongOwnerAccountIdLength(let expected, let got):
                return "ownerAccountId must be \(expected) bytes, got \(got)"
            case .labelTooLong(let maxBytes, let got):
                return "label is \(got) bytes (UTF-8), max is \(maxBytes)"
            case .keyPackageEmpty:
                return "identity.createKeyPackage() returned 0 bytes"
            }
        }
    }

    // MARK: - Output

    /// Result of ``composeAndUpload(...)``. The three primary
    /// fields are independent — UI typically surfaces ``url``
    /// in the share sheet, persists ``blobId`` for the future
    /// revoke flow, and keeps ``callId`` around for the in-
    /// call signaling layer.
    public struct Composed: Sendable, Equatable {
        /// Raw 32-byte content-addressed `blob_id`. Pass to
        /// ``DiscoveryClient/revokeCallInvite(blobId:signingSeed:)``
        /// when the owner cancels the link.
        public let blobId: Data
        /// The full `<origin>/c/<slug>` URL the owner
        /// shares — base64url-no-pad of ``blobId``.
        public let url: String
        /// The freshly-minted 16-byte `call_id` embedded in
        /// the capability. Stored client-side so a
        /// subsequent call-admission JWT mint binds to the
        /// same id.
        public let callId: Data
        /// Wire-format capability bytes the server now holds.
        /// Returned for the rare case where the UI wants to
        /// drive `CallInviteCapability.decodeAndVerify(...)`
        /// against the same bytes (e.g. to read back
        /// `expiryMs` for the landing-page rendering).
        public let capabilityBytes: Data
    }

    // MARK: - Pipeline

    /// End-to-end "create call link" pipeline.
    ///
    /// Five steps:
    ///   1. Generate a fresh 16-byte random `call_id` via
    ///      `SystemRandomNumberGenerator`.
    ///   2. Mint a one-time MLS `KeyPackage` via
    ///      `identity.createKeyPackage()` — the joiner
    ///      consumes it to bootstrap the call MLS group.
    ///   3. `composeCallInviteCapability(...)` via UniFFI —
    ///      wire bytes signed under the owner's Ed25519 key.
    ///   4. ``DiscoveryClient/uploadCallInvite(capabilityBytes:)``
    ///      — pushes the bytes to the server-side ledger.
    ///   5. ``CallInviteUrl/composePointerCallInviteUrl(blobId:origin:)``
    ///      — builds the `<origin>/c/<slug>` URL.
    ///
    /// The call must be driven from a background `Task`
    /// because the upload step solves a server-issued PoW
    /// challenge that can take a few seconds on slow devices.
    ///
    /// - Parameters:
    ///   - ownerSigningSeed: 32-byte Ed25519 signing seed.
    ///   - ownerAccountId:   16-byte UUIDv7 `account_id`.
    ///   - identity:         The owner's MLS identity handle
    ///                       (yields one-time KPs).
    ///   - discoveryClient:  Session-token-bound discovery
    ///                       client.
    ///   - media:            Audio-only vs. audio+video.
    ///   - expiryPreset:     1d / 7d / 30d / never.
    ///   - maxUsesPreset:    single / ten / unlimited.
    ///   - label:            UTF-8 free-text label, ≤ 64
    ///                       bytes.
    ///   - origin:           URL origin override (defaults
    ///                       to `https://lakko.me`).
    public static func composeAndUpload(
        ownerSigningSeed: Data,
        ownerAccountId: Data,
        identity: KeyPackageMinting,
        discoveryClient: DiscoveryClient,
        media: CallMediaPreset,
        expiryPreset: CallInviteExpiryPreset,
        maxUsesPreset: CallInviteMaxUsesPreset,
        label: String,
        origin: String = CallInviteUrl.defaultOrigin
    ) async throws -> Composed {
        // ---- 0. Local validation -----------------------------
        guard ownerSigningSeed.count == 32 else {
            throw Error.wrongOwnerSigningSeedLength(expected: 32, got: ownerSigningSeed.count)
        }
        guard ownerAccountId.count == 16 else {
            throw Error.wrongOwnerAccountIdLength(expected: 16, got: ownerAccountId.count)
        }
        let labelBytes = label.utf8.count
        guard labelBytes <= CallInviteUrl.labelMaxLen else {
            throw Error.labelTooLong(maxBytes: CallInviteUrl.labelMaxLen, got: labelBytes)
        }

        // ---- 1. call_id RNG ----------------------------------
        var callId = Data(count: 16)
        callId.withUnsafeMutableBytes { rawBuf in
            guard let base = rawBuf.baseAddress else { return }
            // SecRandomCopyBytes is the system CSPRNG; falling
            // back to arc4random_buf is fine on the rare
            // hardware where SecRandom is unavailable.
            let rc = SecRandomCopyBytes(kSecRandomDefault, 16, base)
            if rc != errSecSuccess {
                arc4random_buf(base, 16)
            }
        }

        // ---- 2. One-time KeyPackage mint ---------------------
        let oneTimeKeyPackage = try identity.createKeyPackage()
        guard !oneTimeKeyPackage.isEmpty else {
            throw Error.keyPackageEmpty
        }

        // ---- 3. Resolve presets to wire-level values ---------
        let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
        let expiryMs = expiryPreset.toMs(nowMs: nowMs)
        let maxUses = maxUsesPreset.toValue()
        let mediaFlags = FfiCallInviteMedia(
            audio: true, // every call carries audio
            video: media == .audioVideo
        )

        // ---- 4. composeCallInviteCapability via UniFFI -------
        let capabilityBytes = try composeCallInviteCapability(
            ownerSigningSeed: ownerSigningSeed,
            ownerAccountId: ownerAccountId,
            callId: callId,
            expiryMs: expiryMs,
            maxUses: maxUses,
            media: mediaFlags,
            oneTimeKeyPackage: oneTimeKeyPackage,
            label: label
        )

        // ---- 5. uploadCallInvite via HTTP --------------------
        let blobId = try await discoveryClient.uploadCallInvite(
            capabilityBytes: capabilityBytes
        )

        // ---- 6. URL build ------------------------------------
        let url = try CallInviteUrl.composePointerCallInviteUrl(
            blobId: blobId,
            origin: origin
        )

        return Composed(
            blobId: blobId,
            url: url,
            callId: callId,
            capabilityBytes: capabilityBytes
        )
    }
}

/// Narrow protocol the composer needs out of the MLS
/// identity — just the one-time KeyPackage minting surface.
/// Lets the composer be unit-tested against a fake KP
/// minter without instantiating a full MLS identity.
public protocol KeyPackageMinting {
    func createKeyPackage() throws -> Data
}

/// Retroactive conformance — UniFFI-generated `Identity`
/// already exposes a matching `createKeyPackage() throws ->
/// Data` method. The conformance is purely shape-driven; the
/// FFI class never needs to know about this protocol.
extension Identity: KeyPackageMinting {}
