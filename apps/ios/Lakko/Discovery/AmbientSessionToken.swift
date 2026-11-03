// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The signed-in session's bearer token, readable from any thread, for one
/// purpose only: keying `services/discovery`'s rate limiter.
///
/// Discovery charges `/discovery/oprf` and `/discovery/lookup`
/// to the **account** when the request carries a
/// valid session token and to the source **IP** otherwise, so everyone behind
/// one NAT shares a single anonymous bucket. Most `DiscoveryClient()` call
/// sites are constructed without a token, so without this holder a signed-in
/// user's lookups were charged to that shared bucket.
///
/// ``DiscoveryClient`` attaches this token to those three rate-limited reads
/// when the client itself carries none. It is never used for a protected
/// write: those still require the token passed to the client explicitly, so
/// an anonymous client cannot write as whichever account happens to be signed
/// in. The server reads the token on those three routes only to pick the
/// bucket; an expired token falls back to the IP bucket, never to a refusal.
///
/// ``AuthRepository`` keeps it in step with `identitySecrets` (set on every
/// sign-in / restore, cleared on logout). `nil` before sign-in, so signup and
/// recovery lookups stay anonymous as they must.
public final class AmbientSessionToken: @unchecked Sendable {

    /// The process-wide instance.
    public static let shared = AmbientSessionToken()

    private let lock = NSLock()
    private var token: String?

    /// A fresh holder (tests); production uses ``shared``.
    public init() {}

    /// The current token, or `nil` when signed out.
    public var current: String? {
        lock.lock()
        defer { lock.unlock() }
        return token
    }

    /// Replace the token. An empty string clears it.
    public func set(_ newToken: String?) {
        lock.lock()
        defer { lock.unlock() }
        token = (newToken?.isEmpty ?? true) ? nil : newToken
    }
}
