// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// iOS-side helpers for the group join-ticket URL shape (ADR-0028, G-3).
///
/// Sibling of ``CallInviteUrl`` for the `GroupInviteCapability` ecosystem. The
/// `/g/<slug>` slug comes in two forms, discriminated purely by the decoded
/// byte length:
///
///   - **pointer** — the 32-byte content-addressed `blob_id` from
///     `DiscoveryClient.uploadGroupInvite(...)`. The production default: short
///     URL, the joiner fetches the capability from
///     `/discovery/group-invite/<blob_id>`. This is the OQ1 indirection that
///     adds the server-side anti-abuse ledger (consume dedup + soft max_uses).
///   - **inline** — the full `GroupInviteCapability` wire bytes (>= 108 bytes).
///     The censored-region fallback: no `/discovery/group-invite` round-trip.
///
/// The smallest legal inline capability is 108 bytes (a 1-byte group_id), well
/// above the 32-byte pointer, so a byte-length test cleanly discriminates.
public enum GroupInviteUrl {
    // MARK: - Constants

    /// Path prefix every group-invite URL starts with. Mirror of the web
    /// `GROUP_INVITE_URL_PREFIX = "/g/"`.
    public static let urlPrefix: String = "/g/"

    /// Default origin Lakko shares its invite links under. Stays in sync with
    /// the web + Android sides; a link generated on iOS works on any platform.
    public static let defaultOrigin: String = "https://lakko.me"

    /// Length of a content-addressed `blob_id` from
    /// `services/discovery/src/group_invite.rs` — SHA-256, 32 bytes.
    public static let blobIdLen: Int = 32

    /// Floor on the inline-bytes form. The real minimum is 108 bytes; we pin
    /// the discriminator floor at 100 — comfortably above the 32-byte pointer
    /// and below any real capability, so the byte-length test never
    /// misclassifies. Anything > 32 and < 100 is invalid input.
    public static let inlineMinLen: Int = 100

    // MARK: - URL builders

    /// Builder for the **pointer** URL form — the short
    /// `<origin>/g/<blob_id_b64url>` shape backed by
    /// `/discovery/group-invite/<blob_id>` lookup.
    ///
    /// - Throws: ``GroupInviteUrlError/wrongBlobIdLength`` when
    ///   `blobId.count != 32`.
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
    /// Reserved for the censored-region fallback; the pointer form is the
    /// production default.
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
        /// Short form — `blobId` is the 32-byte content-addressed id. Joiner
        /// fetches via `DiscoveryClient.fetchGroupInvite(blobId:)` ->
        /// `GroupInviteCapability.decodeAndVerify(...)`.
        case pointer(blobId: Data)
        /// Long form — the bytes are the wire-format `GroupInviteCapability`.
        /// Joiner runs `GroupInviteCapability.decodeAndVerify(...)` directly.
        case inline(capabilityBytes: Data)
    }

    /// Discriminated parse of a URL slug. The dispatch rule is a byte-length
    /// test: 32 => pointer (SHA-256), >= 100 => inline. Anything else is
    /// invalid input.
    public static func parseGroupInviteSlug(_ slug: String) throws -> ParsedGroupInvite {
        guard let bytes = base64UrlDecodeNoPad(slug) else {
            throw GroupInviteUrlError.invalidBase64Url
        }
        if bytes.count == blobIdLen {
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
