// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// Owner-side composer orchestration for link-based call
/// invites (Phase 3c3-composer-helper).
///
/// Wraps the "make a link for this call" pipeline into a single
/// async function so the UI doesn't have to know about the
/// one-time KeyPackage mint, the FFI boundary, the discovery
/// HTTP round-trip, or the URL builder convention.
///
/// ## What `docs/ux/call-links.md` changed here
///
/// The `call_id` is now an ARGUMENT, not something this mints.
/// A link is made from inside a call that is already running,
/// and it has to lead into THAT call rather than a new one, so
/// the id comes from the live call. The four fields the form
/// used to ask about are gone from the signature too: they are
/// stated once, in ``CallInviteValues``.
///
/// Sibling of `apps/web/src/calls/composer.ts` +
/// `apps/android/.../calls/CallInviteComposer.kt` — same
/// pipeline shape, platform-idiomatic surface.
public enum CallInviteComposer {

    // MARK: - Errors

    public enum Error: Swift.Error, LocalizedError {
        case wrongOwnerSigningSeedLength(expected: Int, got: Int)
        case wrongOwnerAccountIdLength(expected: Int, got: Int)
        case wrongCallIdLength(expected: Int, got: Int)
        case keyPackageEmpty

        public var errorDescription: String? {
            switch self {
            case .wrongOwnerSigningSeedLength(let expected, let got):
                return "ownerSigningSeed must be \(expected) bytes, got \(got)"
            case .wrongOwnerAccountIdLength(let expected, let got):
                return "ownerAccountId must be \(expected) bytes, got \(got)"
            case .wrongCallIdLength(let expected, let got):
                return "callId must be \(expected) bytes, got \(got)"
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
        /// Raw 6-byte ledger `blob_id` the server minted, or `nil` when the
        /// upload failed on transport and the link is the inline fallback
        /// (no ledger row, nothing to revoke). Pass to
        /// ``DiscoveryClient/revokeCallInvite(blobId:signingSeed:)``
        /// when the owner cancels the link.
        public let blobId: Data?
        /// The full `<origin>/c/<id>#<secret>` URL the owner shares
        /// (sealed-links amendment of 2026-09-28): the capability is sealed
        /// under the 12-byte secret in the fragment, which never leaves this
        /// device except inside this string.
        public let url: String
        /// The 16-byte `call_id` embedded in the capability —
        /// the live call's, so a person who follows the link
        /// arrives in THAT call.
        public let callId: Data
        /// The PLAINTEXT capability wire (what a joiner tag is computed
        /// over). The server holds only the sealed blob.
        public let capabilityBytes: Data
    }

    // MARK: - Pipeline

    /// End-to-end "make a link for this call" pipeline.
    ///
    /// Four steps:
    ///   1. Validate the owner's keys and the live call's id.
    ///   2. Mint a one-time MLS `KeyPackage` via
    ///      `identity.createKeyPackage()`.
    ///   3. `composeCallInviteCapability(...)` via UniFFI —
    ///      wire bytes signed under the owner's Ed25519 key,
    ///      carrying the ``CallInviteValues`` every link does.
    ///   4. ``DiscoveryClient/uploadCallInvite(mint:expiryMs:maxUses:)``
    ///      then the pointer link `<origin>/c/<id>#<secret>`; on a
    ///      TRANSPORT failure only, the inline link
    ///      `<origin>/c/<sealed>#<secret>`.
    ///
    /// The call must be driven from a background `Task`: the
    /// upload solves a server-issued anti-spam challenge that
    /// can take a few seconds on slow devices. That cost is why
    /// a link is made here, when someone asks for one, and not
    /// when the call starts.
    ///
    /// - Parameters:
    ///   - callId:           16-byte id of the call the link
    ///                       must lead into.
    ///   - ownerSigningSeed: 32-byte Ed25519 signing seed of
    ///                       the call's owner — the device that
    ///                       founded it. ADR-0032 elects the
    ///                       committer from the owner the
    ///                       capability names, so a link signed
    ///                       by anyone else points nowhere.
    ///   - ownerAccountId:   16-byte UUIDv7 `account_id`.
    ///   - identity:         The owner's MLS identity handle
    ///                       (yields one-time KPs).
    ///   - discoveryClient:  Session-token-bound discovery
    ///                       client.
    ///   - nowMs:            Clock for the expiry window.
    ///   - origin:           URL origin override (defaults
    ///                       to the build's web origin).
    public static func composeAndUpload(
        callId: Data,
        ownerSigningSeed: Data,
        ownerAccountId: Data,
        identity: KeyPackageMinting,
        discoveryClient: DiscoveryClient,
        nowMs: UInt64 = UInt64(Date().timeIntervalSince1970 * 1000),
        origin: String = CallInviteUrl.defaultOrigin
    ) async throws -> Composed {
        // ---- 0. Local validation -----------------------------
        guard ownerSigningSeed.count == 32 else {
            throw Error.wrongOwnerSigningSeedLength(expected: 32, got: ownerSigningSeed.count)
        }
        guard ownerAccountId.count == 16 else {
            throw Error.wrongOwnerAccountIdLength(expected: 16, got: ownerAccountId.count)
        }
        guard callId.count == CallInviteValues.callIdLen else {
            throw Error.wrongCallIdLength(
                expected: CallInviteValues.callIdLen, got: callId.count)
        }

        // ---- 1. One-time KeyPackage mint ---------------------
        let oneTimeKeyPackage = try identity.createKeyPackage()
        guard !oneTimeKeyPackage.isEmpty else {
            throw Error.keyPackageEmpty
        }

        // ---- 2. The values every link carries ----------------
        // Fixed, not asked (`docs/ux/call-links.md` section 4).
        let expiryMs = CallInviteValues.expiryMs(nowMs: nowMs)
        let mint = try composeCallInviteCapability(
            ownerSigningSeed: ownerSigningSeed,
            ownerAccountId: ownerAccountId,
            callId: callId,
            expiryMs: expiryMs,
            maxUses: CallInviteValues.maxUses,
            media: FfiCallInviteMedia(
                audio: CallInviteValues.audio, video: CallInviteValues.video),
            oneTimeKeyPackage: oneTimeKeyPackage,
            label: CallInviteValues.label
        )
        // The plaintext wire, for the joiner-tag and anything that reads the
        // capability back; the seal is what goes to the server.
        let capabilityBytes = try CallInviteCapability.openLink(
            sealed: mint.sealed, linkSecret: mint.linkSecret
        ).wire()

        // ---- 3. uploadCallInvite via HTTP --------------------
        // Only a TRANSPORT failure falls back to the inline link; a refusal
        // from the server surfaces.
        let blobId: Data?
        do {
            blobId = try await discoveryClient.uploadCallInvite(
                mint: mint,
                expiryMs: expiryMs,
                maxUses: CallInviteValues.maxUses
            )
        } catch let error where InviteLink.uploadFallsBackInline(error) {
            NSLog("call invite upload failed (network); sharing the inline link")
            blobId = nil
        }

        // ---- 4. URL build ------------------------------------
        let url: String
        if let blobId {
            url = InviteLink.pointer(
                origin: origin, prefix: CallInviteUrl.urlPrefix,
                blobId: blobId, secret: mint.linkSecret)
        } else {
            url = InviteLink.inline(
                origin: origin, prefix: CallInviteUrl.urlPrefix,
                sealed: mint.sealed, secret: mint.linkSecret)
        }

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
