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
/// for the link (`/c/<blob_id_b64url>`) and
/// ``parseCallInviteSlug(_:)`` for the joiner-side dispatcher.
/// The inline form that carried the capability in the URL was
/// removed on 2026-10-19: a failed upload mints no link, and an
/// inline slug is refused as malformed (a dead link).
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

    // MARK: - URL builders

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
        return "\(origin)\(urlPrefix)\(base64UrlEncodeNoPad(blobId))"
    }

    // MARK: - URL parsing

    /// Parse result for a `/c/<slug>` segment: always the ledger id.
    public enum ParsedCallInvite: Equatable {
        /// `blobId` is the 6-byte ledger id. Joiner fetches via
        /// `DiscoveryClient.fetchCallInvite(blobId:)` and opens it.
        case pointer(blobId: Data)
    }

    /// Parse a URL slug: exactly 8 characters decoding to 6
    /// bytes. Anything else, the retired 32-byte id and an
    /// inline sealed blob included, is invalid input.
    public static func parseCallInviteSlug(_ slug: String) throws -> ParsedCallInvite {
        guard isBase64Url(slug), let bytes = base64UrlDecodeNoPad(slug) else {
            throw CallInviteUrlError.invalidBase64Url
        }
        guard bytes.count == blobIdLen, slug.count == blobIdUrlLen else {
            throw CallInviteUrlError.slugWrongLength(blobIdLen: blobIdLen, got: bytes.count)
        }
        return .pointer(blobId: bytes)
    }
}

// MARK: - Errors

public enum CallInviteUrlError: Error, Equatable, LocalizedError {
    case wrongBlobIdLength(expected: Int, got: Int)
    case invalidBase64Url
    case slugWrongLength(blobIdLen: Int, got: Int)

    public var errorDescription: String? {
        switch self {
        case .wrongBlobIdLength(let expected, let got):
            return "blobId must be \(expected) bytes, got \(got)"
        case .invalidBase64Url:
            return "slug is not valid base64url-no-pad"
        case .slugWrongLength(let blobIdLen, let got):
            return "call-invite slug decoded to \(got) bytes, not a \(blobIdLen)-byte ledger id"
        }
    }
}

// Base64URL helpers (`base64UrlEncodeNoPad` /
// `base64UrlDecodeNoPad`) live in `Lakko/Auth/AccountId.swift`
// at the package module surface — they're imported implicitly
// since both files are in the same target.
