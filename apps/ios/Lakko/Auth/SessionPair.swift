// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The session pair every session-landing route of `services/auth` answers
/// with and the whole of what `POST /auth/session/refresh` returns
/// (ADR-0065 section 3): a short-lived **access token** (the bearer for
/// every service), a **refresh credential** only the refresh route accepts,
/// and the **key generation** both were issued under.
///
/// Both tokens are opaque strings to the client. The generation is what a
/// device compares with the one the directory publishes
/// (``KeyResetLanding``) and what a `401 key-generation-superseded` is about.
public struct SessionPair: Equatable, Codable, Sendable {
    /// The `v2` access token.
    public let sessionToken: String
    /// Absolute unix-millisecond expiry of ``sessionToken``.
    public let sessionExpiresAtMs: Int64
    /// The `r1` refresh credential. Empty for a session that cannot renew
    /// (a test fixture); the provider then never refreshes it.
    public let refreshToken: String
    /// Absolute unix-millisecond expiry of ``refreshToken``.
    public let refreshExpiresAtMs: Int64
    /// The key generation the pair was issued under (1 = never reset).
    public let keyGeneration: UInt64

    public init(
        sessionToken: String,
        sessionExpiresAtMs: Int64,
        refreshToken: String,
        refreshExpiresAtMs: Int64,
        keyGeneration: UInt64
    ) {
        self.sessionToken = sessionToken
        self.sessionExpiresAtMs = sessionExpiresAtMs
        self.refreshToken = refreshToken
        self.refreshExpiresAtMs = refreshExpiresAtMs
        self.keyGeneration = keyGeneration
    }

    /// A pair with no refresh credential: the access token alone, as the
    /// pre-ADR-0065 triple carried. The provider never refreshes it; it
    /// exists for fixtures and for the keys-only pairing outcome.
    public static func accessOnly(sessionToken: String, sessionExpiresAtMs: Int64) -> SessionPair {
        SessionPair(
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs,
            refreshToken: "",
            refreshExpiresAtMs: 0,
            keyGeneration: 1
        )
    }

    /// Whether the refresh credential can still be presented at `nowMs`.
    public func refreshUsable(nowMs: Int64) -> Bool {
        !refreshToken.isEmpty && refreshExpiresAtMs > nowMs
    }

    enum CodingKeys: String, CodingKey {
        case sessionToken = "session_token"
        case sessionExpiresAtMs = "session_expires_at_ms"
        case refreshToken = "refresh_token"
        case refreshExpiresAtMs = "refresh_expires_at_ms"
        case keyGeneration = "key_generation"
    }
}

/// A wire response that carries the session pair flattened into its own
/// object (every login / signup / recovery finish, the device redeem, the
/// passkey finish, the TOTP recover, the key-reset finish).
protocol SessionPairCarrying {
    var sessionToken: String { get }
    var sessionExpiresAtMs: Int64 { get }
    var refreshToken: String { get }
    var refreshExpiresAtMs: Int64 { get }
    var keyGeneration: UInt64 { get }
}

extension SessionPairCarrying {
    /// The pair as one value.
    var sessionPair: SessionPair {
        SessionPair(
            sessionToken: sessionToken,
            sessionExpiresAtMs: sessionExpiresAtMs,
            refreshToken: refreshToken,
            refreshExpiresAtMs: refreshExpiresAtMs,
            keyGeneration: keyGeneration
        )
    }
}
