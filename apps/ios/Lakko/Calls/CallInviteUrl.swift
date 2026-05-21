// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

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
/// Two surfaces:
///
///   - URL plumbing — ``composePointerCallInviteUrl(blobId:)``
///     for the production short form (`/c/<blob_id_b64url>`),
///     ``composeInlineCallInviteUrl(capabilityBytes:)`` for
///     the censored-region fallback (the bytes are encoded
///     into the URL itself, no `/discovery/call-invite`
///     round-trip needed), and ``parseCallInviteSlug(_:)`` for
///     the joiner-side dispatcher.
///   - Form-side enums — ``CallInviteExpiryPreset``,
///     ``CallInviteMaxUsesPreset``, ``CallMediaPreset`` —
///     translate UI dropdown choices into the wire-level
///     `composeCallInviteCapability` arguments.
public enum CallInviteUrl {
    // MARK: - Constants

    /// Path prefix every call-invite URL starts with. Mirror
    /// of the web `CALL_INVITE_URL_PREFIX = "/c/"`.
    public static let urlPrefix: String = "/c/"

    /// Default origin Lakko shares its invite links under.
    /// Stays in sync with the web + Android sides; a link
    /// generated on iOS works when consumed on any platform.
    public static let defaultOrigin: String = "https://lakko.me"

    /// Hard cap on the human-visible label, in bytes (UTF-8).
    /// Mirrors `lakko_contacts::invite::MAX_LABEL_LEN` (64) —
    /// the call-invite codec re-uses the contact-invite
    /// ceiling.
    public static let labelMaxLen: Int = 64

    /// Length of a content-addressed `blob_id` from
    /// `services/discovery/src/call_invite.rs` — SHA-256,
    /// 32 bytes.
    public static let blobIdLen: Int = 32

    /// Lower bound on the inline-bytes form. The 3c3 wire
    /// format with a 256-byte one-time KP tops out around 500
    /// bytes; we pin the floor at 400 to leave headroom for
    /// KP formats that might shrink slightly. Anything < 400
    /// and ≠ 32 is invalid input.
    public static let inlineMinLen: Int = 400

    // MARK: - URL builders

    /// Auto-pick the URL form by argument shape — pointer
    /// (32 bytes) when the caller has uploaded the capability
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
    ///   `blobId.count != 32`.
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
        /// Short form — `blobId` is the 32-byte content-
        /// addressed id. Joiner fetches via
        /// `DiscoveryClient.fetchCallInvite(blobId:)` →
        /// `CallInviteCapability.decodeAndVerify(...)`.
        case pointer(blobId: Data)
        /// Long form — the bytes are the wire-format
        /// `CallInviteCapability`. Joiner runs
        /// `CallInviteCapability.decodeAndVerify(...)` directly.
        case inline(capabilityBytes: Data)
    }

    /// Discriminated parse of a URL slug. The dispatch rule
    /// is a clean byte-length test: 32 ⇒ pointer (SHA-256),
    /// ≥ 400 ⇒ inline (3c3 wire-format minimum). Anything
    /// else is invalid input.
    public static func parseCallInviteSlug(_ slug: String) throws -> ParsedCallInvite {
        guard let bytes = base64UrlDecodeNoPad(slug) else {
            throw CallInviteUrlError.invalidBase64Url
        }
        if bytes.count == blobIdLen {
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

// MARK: - Form-side enums

/// Discrete UI choices the "Create call link" form exposes
/// for the `expiry_ms` field. Same recipes as the contact-
/// invite form — keeps the UX consistent across the two
/// invite kinds.
public enum CallInviteExpiryPreset {
    case oneDay
    case sevenDays
    case thirtyDays
    case never

    /// Translate into the UNIX-millis argument
    /// `composeCallInviteCapability` expects. `.never`
    /// resolves to `0` — the `CALL_NEVER_EXPIRES` sentinel —
    /// and the UI MUST surface a warning before allowing it.
    public func toMs(nowMs: UInt64) -> UInt64 {
        switch self {
        case .oneDay:
            return nowMs + 1 * 86_400_000
        case .sevenDays:
            return nowMs + 7 * 86_400_000
        case .thirtyDays:
            return nowMs + 30 * 86_400_000
        case .never:
            return 0
        }
    }
}

/// Discrete UI choices for `max_uses`.
public enum CallInviteMaxUsesPreset {
    case single
    case ten
    case unlimited

    /// Translate into the `max_uses` u32 the wire format
    /// carries. `.single` resolves to `0` — the
    /// `CALL_SINGLE_USE` sentinel. `.unlimited` saturates at
    /// `u32::MAX`.
    public func toValue() -> UInt32 {
        switch self {
        case .single:
            return 0
        case .ten:
            return 10
        case .unlimited:
            return UInt32.max
        }
    }
}

/// Discrete UI choices the "Create call link" form exposes
/// for the media kind — surfaces the same `audio-only` vs
/// `audio-video` toggle the `CallInviteMedia` wire flags
/// carry.
public enum CallMediaPreset {
    case audio
    case audioVideo
}

// MARK: - Human-visible renderers

/// Convert a wire `max_uses` u32 back into a human-visible
/// rendering for the landing page.
public func describeCallInviteMaxUses(_ maxUses: UInt32) -> String {
    if maxUses == 0 { return "single-use" }
    if maxUses == UInt32.max { return "unlimited" }
    return "up to \(maxUses) uses"
}

/// Render an `expiry_ms` (UNIX millis) for the landing page —
/// `"never expires"` for the `CALL_NEVER_EXPIRES` sentinel,
/// otherwise a relative "in 6 days" / "expired 3 hours ago"
/// string.
public func describeCallInviteExpiry(expiryMs: UInt64, nowMs: UInt64) -> String {
    if expiryMs == 0 { return "never expires" }
    // Signed delta so we can render "ago" for past expiries.
    let exp = Int64(bitPattern: expiryMs)
    let now = Int64(bitPattern: nowMs)
    let deltaMs = exp - now
    let futureLabel = deltaMs >= 0 ? "in" : "expired"
    let suffix = deltaMs >= 0 ? "" : " ago"
    let absMs = abs(deltaMs)
    let day: Int64 = 86_400_000
    let hour: Int64 = 3_600_000
    let minute: Int64 = 60_000
    if absMs >= day {
        let days = (absMs + day / 2) / day
        return "\(futureLabel) \(days) day\(days == 1 ? "" : "s")\(suffix)"
    }
    if absMs >= hour {
        let hours = (absMs + hour / 2) / hour
        return "\(futureLabel) \(hours) hour\(hours == 1 ? "" : "s")\(suffix)"
    }
    let mins = max(1, (absMs + minute / 2) / minute)
    return "\(futureLabel) \(mins) minute\(mins == 1 ? "" : "s")\(suffix)"
}

// Base64URL helpers (`base64UrlEncodeNoPad` /
// `base64UrlDecodeNoPad`) live in `Lakko/Auth/AccountId.swift`
// at the package module surface — they're imported implicitly
// since both files are in the same target.
