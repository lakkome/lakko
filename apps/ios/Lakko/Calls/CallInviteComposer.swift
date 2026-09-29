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
        /// Raw 6-byte ledger `blob_id` the server minted, or `nil`
        /// for the inline link a TRANSPORT failure of the upload
        /// falls back to (no ledger row, nothing to revoke). Pass
        /// a present id to
        /// ``DiscoveryClient/revokeCallInvite(blobId:signingSeed:)``
        /// when the owner cancels the link.
        public let blobId: Data?
        /// The full `<origin>/c/<slug>#<secret>` URL the owner
        /// shares — base64url-no-pad of ``blobId`` (or of the
        /// sealed link bytes for the inline form), the link
        /// secret after `#`.
        public let url: String
        /// The 16-byte `call_id` embedded in the capability —
        /// the live call's, so a person who follows the link
        /// arrives in THAT call.
        public let callId: Data
        /// The PLAINTEXT capability wire (the server holds only its
        /// sealed form).
        /// Returned for the rare case where the UI wants to
        /// drive `CallInviteCapability.decodeAndVerify(...)`
        /// against the same bytes (e.g. to read back
        /// `expiryMs` for the landing-page rendering).
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
    ///   4. ``DiscoveryClient/uploadCallInvite(capabilityBytes:)``
    ///      then ``CallInviteUrl/composePointerCallInviteUrl(blobId:origin:)``.
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
        origin: String = CallInviteUrl.defaultOrigin,
        linkGrant: (_ linkId: Data, _ expiresAtMs: UInt64) async throws -> FfiReachGrant
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
        // ADR-0058 section 6 — a `/c/` link carries a `link` grant scoped to
        // it, registered by the caller BEFORE the link exists.
        let linkId = generateLinkId()
        let grant = try await linkGrant(linkId, expiryMs)
        let mint = try composeCallInviteCapability(
            ownerSigningSeed: ownerSigningSeed,
            ownerAccountId: ownerAccountId,
            callId: callId,
            expiryMs: expiryMs,
            maxUses: CallInviteValues.maxUses,
            media: FfiCallInviteMedia(
                audio: CallInviteValues.audio, video: CallInviteValues.video),
            oneTimeKeyPackage: oneTimeKeyPackage,
            label: CallInviteValues.label,
            linkId: linkId,
            linkGrant: grant
        )
        // The plaintext capability, for the caller that reads it back.
        let capabilityBytes = try CallInviteCapability
            .openLink(sealed: mint.sealed, linkSecret: mint.linkSecret).wire()

        // ---- 3. uploadCallInvite via HTTP --------------------
        // ADR-0028 amendment 2026-09-28 — the ledger gets the SEALED blob and
        // the clear fields only. Only a TRANSPORT failure (a censored or
        // offline network) falls back to the inline form, which carries the
        // sealed blob in the URL; a refusal from the server surfaces.
        let (blobId, base) = try await pointerOrInlineBase(
            sealed: mint.sealed,
            origin: origin,
            upload: {
                try await discoveryClient.uploadCallInvite(
                    mint: mint,
                    expiryMs: expiryMs,
                    maxUses: CallInviteValues.maxUses
                )
            })

        // ---- 4. URL build ------------------------------------
        // The secret rides the fragment: `<origin>/c/<id>#<secret>`.
        let url = InviteLinkSecret.link(base: base, secret: mint.linkSecret)

        return Composed(
            blobId: blobId,
            url: url,
            callId: callId,
            capabilityBytes: capabilityBytes
        )
    }
}

extension CallInviteComposer {
    /// Upload the sealed link and build the pointer base
    /// `<origin>/c/<id>`; on a TRANSPORT failure of the upload
    /// only, the inline base `<origin>/c/<b64url sealed>` with no
    /// ledger id (web and Android do the same). Any other upload
    /// error is rethrown. Split out so the rule is host-testable.
    static func pointerOrInlineBase(
        sealed: Data,
        origin: String,
        upload: () async throws -> Data
    ) async throws -> (blobId: Data?, base: String) {
        do {
            let blobId = try await upload()
            return (blobId, try CallInviteUrl.composePointerCallInviteUrl(blobId: blobId, origin: origin))
        } catch let error where uploadFallsBackInline(error) {
            NSLog("call invite upload failed (network); sharing the inline link")
            let base = (try? CallInviteUrl.composeInlineCallInviteUrl(capabilityBytes: sealed, origin: origin))
                ?? CallInviteUrl.composeCallInviteUrl(sealed, origin: origin)
            return (nil, base)
        }
    }

    /// Whether an upload error is a transport failure (the one case that
    /// falls back to the inline link).
    static func uploadFallsBackInline(_ error: Swift.Error) -> Bool {
        if let discovery = error as? DiscoveryClientError, case .network = discovery { return true }
        return error is URLError
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
