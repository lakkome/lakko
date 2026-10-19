// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// iOS-side helpers for the group join-ticket URL shape (ADR-0028, G-3).
///
/// Sibling of ``CallInviteUrl`` for the `GroupInviteCapability` ecosystem. The
/// `/g/<slug>` slug is one form:
///
///   - **pointer** — the 6-byte ledger id the discovery service mints at
///     `DiscoveryClient.uploadGroupInvite(...)` (8 base64url characters,
///     ADR-0028 amendment of 2026-09-27). The production default: the joiner
///     fetches the capability from `/discovery/group-invite/<blob_id>`. This is
///     the OQ1 indirection that adds the server-side anti-abuse ledger (consume
///     dedup + soft max_uses).
///
/// Anything else is refused, fail-closed: the retired 32-byte (43-character)
/// content-addressed id of the old link shape, and the inline form that
/// carried the sealed blob in the path (removed 2026-10-19: a failed upload
/// mints no link; an inline URL opens as a dead link).
public enum GroupInviteUrl {
    // MARK: - Constants

    /// Path prefix every group-invite URL starts with. Mirror of the web
    /// `GROUP_INVITE_URL_PREFIX = "/g/"`.
    public static let urlPrefix: String = "/g/"

    /// Default origin Lakko shares its invite links under: the web origin of
    /// the stack this build talks to (`ApiEndpoints.webOrigin`).
    public static var defaultOrigin: String { ApiEndpoints.webOrigin }

    /// Length of a ledger `blob_id` (6 bytes), read from the core.
    public static var blobIdLen: Int { Int(inviteBlobIdLen()) }

    /// Length of a ledger `blob_id` in the URL (8 base64url characters).
    public static var blobIdUrlLen: Int { Int(inviteBlobIdUrlLen()) }

    // MARK: - URL builders

    /// Builder for the **pointer** URL form — the short
    /// `<origin>/g/<blob_id_b64url>` shape backed by
    /// `/discovery/group-invite/<blob_id>` lookup.
    ///
    /// - Throws: ``GroupInviteUrlError/wrongBlobIdLength`` when `blobId` is not
    ///   ``blobIdLen`` bytes.
    public static func composePointerGroupInviteUrl(
        blobId: Data,
        origin: String = defaultOrigin
    ) throws -> String {
        guard blobId.count == blobIdLen else {
            throw GroupInviteUrlError.wrongBlobIdLength(expected: blobIdLen, got: blobId.count)
        }
        return "\(origin)\(urlPrefix)\(base64UrlEncodeNoPad(blobId))"
    }

    // MARK: - URL parsing

    /// Parse result for a `/g/<slug>` segment: always the ledger id.
    public enum ParsedGroupInvite: Equatable {
        /// `blobId` is the 6-byte ledger id. Joiner fetches via
        /// `DiscoveryClient.fetchGroupInvite(blobId:)` and opens it.
        case pointer(blobId: Data)
    }

    /// Parse a URL slug: exactly 8 characters decoding to 6 bytes. Anything
    /// else, an inline sealed blob included, is invalid input.
    public static func parseGroupInviteSlug(_ slug: String) throws -> ParsedGroupInvite {
        guard isBase64Url(slug), let bytes = base64UrlDecodeNoPad(slug) else {
            throw GroupInviteUrlError.invalidBase64Url
        }
        guard bytes.count == blobIdLen, slug.count == blobIdUrlLen else {
            throw GroupInviteUrlError.slugWrongLength(blobIdLen: blobIdLen, got: bytes.count)
        }
        return .pointer(blobId: bytes)
    }
}

/// `true` when `value` is non-empty and uses only the base64url alphabet with
/// no padding (RFC 4648 section 5) - the only shape a Lakko link segment ever
/// has. The shared decoder is lenient (it also takes `+`, `/` and `=`), so the
/// link parsers check the alphabet first and a mangled link is refused rather
/// than read.
func isBase64Url(_ value: String) -> Bool {
    !value.isEmpty && value.unicodeScalars.allSatisfy { scalar in
        switch scalar {
        case "A"..."Z", "a"..."z", "0"..."9", "-", "_": return true
        default: return false
        }
    }
}

// MARK: - Errors

public enum GroupInviteUrlError: Error, Equatable, LocalizedError {
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
            return "group-invite slug decoded to \(got) bytes, not a \(blobIdLen)-byte ledger id"
        }
    }
}
