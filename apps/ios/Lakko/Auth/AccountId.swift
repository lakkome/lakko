// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Account-id helpers.
///
/// Mirror of:
///
///   * `apps/web/src/auth/account-id.ts` (1.17d3)
///   * `apps/android/.../auth/AccountId.kt` (1.15d2)
///
/// The Lakko auth service identifies accounts by an opaque 16-byte
/// UUIDv7 the client picks at signup time. UUIDv7 (RFC 9562 §5.7)
/// embeds a 48-bit Unix-millisecond timestamp followed by 74 bits of
/// randomness, so the resulting bytes:
///
///   * sort lexicographically by creation time (useful in DB indices
///     server-side);
///   * cannot be guessed (74 bits of entropy is well above what an
///     attacker can brute-force);
///   * are leak-free — the timestamp is visible, but reveals only
///     "user signed up around then", not which device / network /
///     locale.
///
/// The auth service treats `account_id` as opaque bytes; this is
/// purely a client-side convention.

/// Raw length of an `account_id` in bytes (UUIDv7 is always 16).
public let accountIdLength: Int = 16

/// Generate a fresh UUIDv7 as raw bytes.
///
/// Layout (RFC 9562 §5.7):
///
/// ```text
///   0               1               2               3
///   0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
///  +---------------+---------------+---------------+---------------+
///  |                           unix_ts_ms                          |
///  +---------------+---------------+-------+-------+---------------+
///  |          unix_ts_ms           |  ver  |       rand_a          |
///  +-------+-----------------------+-------+-----------------------+
///  |var|                        rand_b                             |
///  +-------+-------------------------------+-----------------------+
///  |                            rand_b                             |
///  +---------------------------------------------------------------+
/// ```
///
///   * `ver` = `0b0111` (UUIDv7) at byte 6 high nibble
///   * `var` = `0b10` (RFC variant) at byte 8 top two bits
///
/// Random bits come from `SecRandomCopyBytes`, Apple's standard
/// CSPRNG (backed by the kernel `corecrypto` entropy pool — same
/// source `arc4random_buf` reads from). At signup we call this once
/// per account lifetime, so the per-call syscall cost is irrelevant.
public func generateAccountId() -> Data {
    var bytes = [UInt8](repeating: 0, count: accountIdLength)
    let status = bytes.withUnsafeMutableBytes { buf -> Int32 in
        guard let base = buf.baseAddress else { return errSecParam }
        return SecRandomCopyBytes(kSecRandomDefault, accountIdLength, base)
    }
    // `SecRandomCopyBytes` failing means the OS entropy pool is
    // unreachable — which on iOS means the device is in an
    // unrecoverable boot-time state. `precondition` so we crash loudly
    // instead of papering over a broken CSPRNG with a chosen-id
    // collision risk.
    precondition(status == errSecSuccess, "SecRandomCopyBytes failed: \(status)")

    let nowMs = UInt64(Date().timeIntervalSince1970 * 1000)
    bytes[0] = UInt8((nowMs >> 40) & 0xFF)
    bytes[1] = UInt8((nowMs >> 32) & 0xFF)
    bytes[2] = UInt8((nowMs >> 24) & 0xFF)
    bytes[3] = UInt8((nowMs >> 16) & 0xFF)
    bytes[4] = UInt8((nowMs >> 8) & 0xFF)
    bytes[5] = UInt8(nowMs & 0xFF)

    // Version 7 in the high nibble of byte 6 (low nibble stays random).
    bytes[6] = (bytes[6] & 0x0F) | 0x70
    // RFC variant `10xxxxxx` in the top two bits of byte 8.
    bytes[8] = (bytes[8] & 0x3F) | 0x80

    return Data(bytes)
}

/// Base64-encode raw bytes (standard alphabet, with padding) — the
/// shape every binary field on the auth wire uses.
///
/// `Data.base64EncodedString()` emits single-line output by default,
/// matching the server's `serde_with::base64::Base64<Standard, Padded>`
/// shape and the JVM `Base64.getEncoder()` output the Android side
/// produces.
public func base64Encode(_ bytes: Data) -> String {
    bytes.base64EncodedString()
}

/// Decode a base64 string back to bytes. Returns `nil` on invalid
/// input — the caller has already validated wire correctness or
/// trusts the source. The Kotlin side throws on invalid input; we
/// surface `nil` here so callers can branch with `guard let` rather
/// than wrapping every call in a `do { try } catch`.
public func base64Decode(_ b64: String) -> Data? {
    Data(base64Encoded: b64)
}

/// Base64url-encode raw bytes WITHOUT padding (RFC 4648 §5).
///
/// Used by URL path segments — axum rejects the standard-alphabet
/// `+` and `/`, so the discovery service's
/// `GET /discovery/identity/{account_id}` path expects this shape.
///
/// Swift's `Foundation` does not ship a base64url encoder, so we
/// transform the standard-alphabet output by replacing `+` → `-`,
/// `/` → `_`, and stripping `=` padding — the canonical RFC 4648 §5
/// translation.
public func base64UrlEncodeNoPad(_ bytes: Data) -> String {
    bytes.base64EncodedString()
        .replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_")
        .replacingOccurrences(of: "=", with: "")
}

/// Reverse of ``base64UrlEncodeNoPad(_:)``. The input may be padded
/// or unpadded — we re-pad to a multiple of four before handing off
/// to `Data(base64Encoded:)`, which insists on padded input.
public func base64UrlDecodeNoPad(_ slug: String) -> Data? {
    var s = slug
        .replacingOccurrences(of: "-", with: "+")
        .replacingOccurrences(of: "_", with: "/")
    let remainder = s.count % 4
    if remainder > 0 {
        s.append(String(repeating: "=", count: 4 - remainder))
    }
    return Data(base64Encoded: s)
}
