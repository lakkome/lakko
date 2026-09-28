// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// iOS-side helpers for the group join-ticket URL shape (ADR-0028, G-3).
///
/// Sibling of ``CallInviteUrl`` for the `GroupInviteCapability` ecosystem. The
/// `/g/<slug>` slug comes in two forms, discriminated purely by the decoded
/// byte length:
///
///   - **pointer** — the 6-byte ledger id the discovery service mints at
///     `DiscoveryClient.uploadGroupInvite(...)` (8 base64url characters,
///     ADR-0028 amendment of 2026-09-27). The production default: the joiner
///     fetches the capability from `/discovery/group-invite/<blob_id>`. This is
///     the OQ1 indirection that adds the server-side anti-abuse ledger (consume
///     dedup + soft max_uses).
///   - **inline** — the full `GroupInviteCapability` wire bytes (>= 108 bytes).
///     The fallback when the upload fails on a transport error: no
///     `/discovery/group-invite` round-trip.
///
/// Anything else is refused, fail-closed - in particular the retired 32-byte
/// (43-character) content-addressed id of the old link shape, which falls in
/// the gap between the two forms.
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

    /// Floor on the inline-bytes form. The real minimum is 108 bytes; the
    /// discriminator floor is 100 — above the 6-byte pointer and below any real
    /// capability, so the byte-length test never misclassifies. Anything
    /// between the two is invalid input.
    public static let inlineMinLen: Int = 100

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

    /// Builder for the **inline-bytes** URL form — the long shape that carries
    /// the capability inside the URL itself, no server round-trip on open.
    /// Used only when the upload fails on a transport error; the pointer form
    /// is the production default.
    ///
    /// - Throws: ``GroupInviteUrlError/inlineTooSmall`` when
    ///   `capabilityBytes.count < 100`.
    public static func composeInlineGroupInviteUrl(
        capabilityBytes: Data,
        origin: String = defaultOrigin
    ) throws -> String {
        guard capabilityBytes.count >= inlineMinLen else {
            throw GroupInviteUrlError.inlineTooSmall(minimum: inlineMinLen, got: capabilityBytes.count)
        }
        return "\(origin)\(urlPrefix)\(base64UrlEncodeNoPad(capabilityBytes))"
    }

    // MARK: - URL parsing

    /// Discriminated parse result for a `/g/<slug>` segment.
    public enum ParsedGroupInvite: Equatable {
        /// Short form — `blobId` is the 6-byte ledger id. Joiner fetches via
        /// `DiscoveryClient.fetchGroupInvite(blobId:)` ->
        /// `GroupInviteCapability.decodeAndVerify(...)`.
        case pointer(blobId: Data)
        /// Long form — the bytes are the wire-format `GroupInviteCapability`.
        /// Joiner runs `GroupInviteCapability.decodeAndVerify(...)` directly.
        case inline(capabilityBytes: Data)
    }

    /// Discriminated parse of a URL slug. The dispatch rule is a byte-length
    /// test: 6 bytes (exactly 8 characters) => pointer, >= 100 => inline.
    /// Anything else is invalid input.
    public static func parseGroupInviteSlug(_ slug: String) throws -> ParsedGroupInvite {
        guard isBase64Url(slug), let bytes = base64UrlDecodeNoPad(slug) else {
            throw GroupInviteUrlError.invalidBase64Url
        }
        if bytes.count == blobIdLen, slug.count == blobIdUrlLen {
            return .pointer(blobId: bytes)
        }
        if bytes.count >= inlineMinLen {
            return .inline(capabilityBytes: bytes)
        }
        throw GroupInviteUrlError.slugWrongLength(
            blobIdLen: blobIdLen,
            inlineMinLen: inlineMinLen,
            got: bytes.count
        )
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
    case inlineTooSmall(minimum: Int, got: Int)
    case invalidBase64Url
    case slugWrongLength(blobIdLen: Int, inlineMinLen: Int, got: Int)

    public var errorDescription: String? {
        switch self {
        case .wrongBlobIdLength(let expected, let got):
            return "blobId must be \(expected) bytes, got \(got)"
        case .inlineTooSmall(let minimum, let got):
            return "capabilityBytes must be >= \(minimum) bytes, got \(got)"
        case .invalidBase64Url:
            return "slug is not valid base64url-no-pad"
        case .slugWrongLength(let blobIdLen, let inlineMinLen, let got):
            return "group-invite slug decoded to \(got) bytes — neither a \(blobIdLen)-byte pointer nor a >= \(inlineMinLen)-byte inline capability"
        }
    }
}
