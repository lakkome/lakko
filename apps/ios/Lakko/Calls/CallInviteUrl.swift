// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// iOS-side helpers for the link-based call-invite URL shape
/// (Phase 3c3-url-helpers).
///
/// Sibling of `composePointerInviteUrl` (the contact-invite
/// URL helper that lives on `ContactsRepository`) for the
/// `CallInviteCapability` ecosystem. The URL prefix is
/// `/c/` (vs. `/i/` for contact invites), so the iOS
/// universal-link dispatcher in `LakkoApp` can branch on the
/// path segment without parsing the slug.
///
/// URL plumbing — ``composePointerCallInviteUrl(blobId:)``
/// for the production short form (`/c/<blob_id_b64url>`),
/// ``composeInlineCallInviteUrl(capabilityBytes:)`` for the
/// censored-region fallback (the bytes are encoded into the URL
/// itself, no `/discovery/call-invite` round-trip needed), and
/// ``parseCallInviteSlug(_:)`` for the joiner-side dispatcher.
///
/// The form-side enums that used to live here are gone with the
/// form (`docs/ux/call-links.md` section 6): a link carries one
/// fixed set of values now, and they are stated once in
/// ``CallInviteValues``.
public enum CallInviteUrl {
    // MARK: - Constants

    /// Path prefix every call-invite URL starts with. Mirror
    /// of the web `CALL_INVITE_URL_PREFIX = "/c/"`.
    public static let urlPrefix: String = "/c/"

    /// Default origin Lakko shares its invite links under: the
    /// web origin of the stack this build talks to
    /// (`ApiEndpoints.webOrigin`), so a link made against the
    /// test stack opens on the test stack.
    public static var defaultOrigin: String { ApiEndpoints.webOrigin }

    /// Length of a ledger `blob_id`: 6 random bytes the
    /// discovery service mints at upload (ADR-0028 amendment
    /// of 2026-09-27), read from the core.
    public static var blobIdLen: Int { Int(inviteBlobIdLen()) }

    /// Length of a ledger `blob_id` in the URL (8 base64url
    /// characters).
    public static var blobIdUrlLen: Int { Int(inviteBlobIdUrlLen()) }

    /// Lower bound on the inline-bytes form. The 3c3 wire
    /// format with a 256-byte one-time KP tops out around 500
    /// bytes; we pin the floor at 400 to leave headroom for
    /// KP formats that might shrink slightly. Anything < 400
    /// that is not a 6-byte pointer is invalid input - the
    /// retired 32-byte content-addressed id included.
    public static let inlineMinLen: Int = 400

    // MARK: - URL builders

    /// Auto-pick the URL form by argument shape — pointer
    /// (6 bytes) when the caller has uploaded the capability
    /// and wants the short form, inline-bytes (≥ 400 bytes)
    /// when the caller is staying server-free.
    ///
    /// Both produce a `<origin>/c/<base64url>` shape; the
    /// joiner side discriminates via
    /// ``parseCallInviteSlug(_:)`` on the byte length.
    public static func composeCallInviteUrl(
        _ bytes: Data,
        origin: String = defaultOrigin
    ) -> String {
        return "\(origin)\(urlPrefix)\(base64UrlEncodeNoPad(bytes))"
    }

    /// Builder for the **pointer** URL form — the short
    /// `<origin>/c/<blob_id_b64url>` shape backed by
    /// `/discovery/call-invite/<blob_id>` lookup.
    ///
    /// - Throws: ``CallInviteUrlError/wrongBlobIdLength`` when
    ///   `blobId` is not ``blobIdLen`` bytes.
    public static func composePointerCallInviteUrl(
        blobId: Data,
        origin: String = defaultOrigin
    ) throws -> String {
        guard blobId.count == blobIdLen else {
            throw CallInviteUrlError.wrongBlobIdLength(
                expected: blobIdLen,
                got: blobId.count
            )
        }
        return composeCallInviteUrl(blobId, origin: origin)
    }

    /// Builder for the **inline-bytes** URL form — the long
    /// shape that carries the capability inside the URL
    /// itself, no server round-trip on open. Reserved for the
    /// censored-region fallback; the pointer form is the
    /// production default.
    ///
    /// - Throws: ``CallInviteUrlError/inlineTooSmall`` when
    ///   `capabilityBytes.count < 400`.
    public static func composeInlineCallInviteUrl(
        capabilityBytes: Data,
        origin: String = defaultOrigin
    ) throws -> String {
        guard capabilityBytes.count >= inlineMinLen else {
            throw CallInviteUrlError.inlineTooSmall(
                minimum: inlineMinLen,
                got: capabilityBytes.count
            )
        }
        return composeCallInviteUrl(capabilityBytes, origin: origin)
    }

    // MARK: - URL parsing

    /// Discriminated parse result for a `/c/<slug>` segment.
    public enum ParsedCallInvite: Equatable {
        /// Short form — `blobId` is the 6-byte ledger id. Joiner fetches via
        /// `DiscoveryClient.fetchCallInvite(blobId:)` →
        /// `CallInviteCapability.decodeAndVerify(...)`.
        case pointer(blobId: Data)
        /// Long form — the bytes are the wire-format
        /// `CallInviteCapability`. Joiner runs
        /// `CallInviteCapability.decodeAndVerify(...)` directly.
        case inline(capabilityBytes: Data)
    }

    /// Discriminated parse of a URL slug. The dispatch rule
    /// is a clean byte-length test: 6 bytes (exactly 8
    /// characters) ⇒ pointer, ≥ 400 ⇒ inline (3c3 wire-format
    /// minimum). Anything else is invalid input.
    public static func parseCallInviteSlug(_ slug: String) throws -> ParsedCallInvite {
        guard isBase64Url(slug), let bytes = base64UrlDecodeNoPad(slug) else {
            throw CallInviteUrlError.invalidBase64Url
        }
        if bytes.count == blobIdLen, slug.count == blobIdUrlLen {
            return .pointer(blobId: bytes)
        }
        if bytes.count >= inlineMinLen {
            return .inline(capabilityBytes: bytes)
        }
        throw CallInviteUrlError.slugWrongLength(
            blobIdLen: blobIdLen,
            inlineMinLen: inlineMinLen,
            got: bytes.count
        )
    }
}

// MARK: - Errors

public enum CallInviteUrlError: Error, Equatable, LocalizedError {
    case wrongBlobIdLength(expected: Int, got: Int)
    case inlineTooSmall(minimum: Int, got: Int)
    case invalidBase64Url
    case slugWrongLength(blobIdLen: Int, inlineMinLen: Int, got: Int)

    public var errorDescription: String? {
        switch self {
        case .wrongBlobIdLength(let expected, let got):
            return "blobId must be \(expected) bytes, got \(got)"
        case .inlineTooSmall(let minimum, let got):
            return "capabilityBytes must be ≥ \(minimum) bytes, got \(got)"
        case .invalidBase64Url:
            return "slug is not valid base64url-no-pad"
        case .slugWrongLength(let blobIdLen, let inlineMinLen, let got):
            return "call-invite slug decoded to \(got) bytes — neither a \(blobIdLen)-byte pointer nor a ≥ \(inlineMinLen)-byte inline capability"
        }
    }
}

// Base64URL helpers (`base64UrlEncodeNoPad` /
// `base64UrlDecodeNoPad`) live in `Lakko/Auth/AccountId.swift`
// at the package module surface — they're imported implicitly
// since both files are in the same target.
