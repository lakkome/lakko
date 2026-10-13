// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0073 section 3.3 — the ONE token provider.
//
// Every REST client of the app (auth, discovery, storage, the GIF proxy, the
// call JWT and TURN mints, the anonymous-token issuer, WebAuthn, push,
// channels, transparency, the DS) sends its request through
// `AntiCensorshipTransport.perform`, the seam that also routes through the
// obfuscated tunnel when armed. That seam hands every request carrying a
// bearer this session issued to `SessionTokenProvider.shared.perform`, which
// swaps in a fresh access token (refreshing when 120 s or less remain, one
// refresh in flight at a time), and on a `401` applies one rule: a
// `key-generation-superseded` body means the account's keys were reset on
// another device (section 3.5); any other `401` on a bearer route earns one
// refresh and one retry.
//
// The pure rules below (`SessionTokenRule`) are what the tests pin; the
// provider is the state around them.

/// The rules of ADR-0073 section 3.3, free of state.
public enum SessionTokenRule {

    /// Refresh when this much or less of the access token's life remains.
    /// Chosen, not measured (ADR-0073 section 3.3).
    public static let refreshAheadMs: Int64 = 120_000

    /// What to do before presenting the access token.
    public enum Decision: Equatable {
        /// More than ``refreshAheadMs`` remain: present it as it is.
        case useCurrent
        /// It is expiring or expired and the refresh credential is live:
        /// refresh first.
        case refreshFirst
        /// It is expiring or expired and there is no usable refresh
        /// credential: present it anyway (the server decides); nothing can
        /// renew it.
        case noRefreshCredential
    }

    /// The decision for `pair` at `nowMs`.
    public static func decide(pair: SessionPair, nowMs: Int64) -> Decision {
        if pair.sessionExpiresAtMs - nowMs > refreshAheadMs {
            return .useCurrent
        }
        return pair.refreshUsable(nowMs: nowMs) ? .refreshFirst : .noRefreshCredential
    }

    /// What a `401` means, by its body (ADR-0073 sections 3.2, 3.4, 4.1).
    public enum UnauthorizedMeaning: Equatable {
        /// `{"error":"key-generation-superseded"}`: the keys were reset
        /// elsewhere. The device signs out and wipes.
        case keyGenerationSuperseded
        /// `{"error":"refresh-invalid"}`: the refresh credential is bad,
        /// expired or unknown. Sign in again; nothing is wiped.
        case refreshInvalid
        /// `{"error":"device-revoked"}` (ADR-0074 section 2): this device was
        /// removed from the account on another device. The device signs out
        /// and wipes everything, by itself. Read before the generation, as
        /// the server checks it first.
        case deviceRevoked
        /// A plain `401` (an expired access token, wrong credentials, a
        /// refused code, a logged-out session): no body code, or another one.
        case other
    }

    /// Classify a `401` by its JSON body. Anything that is not
    /// `{"error": <one of the two codes>}` is ``UnauthorizedMeaning/other``.
    public static func classify401(body: Data) -> UnauthorizedMeaning {
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let code = object["error"] as? String else {
            return .other
        }
        switch code {
        case "key-generation-superseded": return .keyGenerationSuperseded
        case "refresh-invalid": return .refreshInvalid
        case "device-revoked": return .deviceRevoked
        default: return .other
        }
    }

    /// Whether a `device-revoked` answer to `requestURL` is proof that this
    /// device was removed: only when the request went to `services/auth`
    /// (ADR-0074 section 2.3; discovery's `403 device-revoked` and a DS `401`
    /// are not proof).
    public static func revocationIsProof(requestURL: URL?, authBaseURL: String) -> Bool {
        guard let requestURL, !authBaseURL.isEmpty else { return false }
        let text = requestURL.absoluteString
        return text == authBaseURL || text.hasPrefix(authBaseURL + "/")
    }

    /// What the caller of a bearer route does after a `401`.
    public enum AfterBearer401: Equatable {
        /// Sign out and wipe (section 3.5). The request is not retried.
        case signOutSuperseded
        /// This device was removed (ADR-0074): sign out and wipe everything.
        /// The request is not retried.
        case signOutRevoked
        /// Refresh once, then retry the request once.
        case refreshAndRetry
        /// Report the `401` as it is.
        case report
    }

    /// The retry rule: a superseded generation is never retried; any other
    /// `401` earns exactly one refresh-and-retry; a second `401` is reported.
    public static func afterBearer401(
        meaning: UnauthorizedMeaning,
        alreadyRetried: Bool
    ) -> AfterBearer401 {
        switch meaning {
        case .keyGenerationSuperseded:
            return .signOutSuperseded
        case .deviceRevoked:
            return .signOutRevoked
        case .refreshInvalid, .other:
            return alreadyRetried ? .report : .refreshAndRetry
        }
    }
}

/// Thrown by the provider's transport hook when a request would have gone
/// out under a session the server already refused as superseded. The
/// request is never sent; the app is signing out.
public struct SessionSupersededError: Error, Equatable {
    public init() {}
}

/// Thrown by the provider's transport hook when a request would have gone
/// out under a session the server already refused as device-revoked
/// (ADR-0074). The request is never sent; the device is wiping.
public struct SessionDeviceRevokedError: Error, Equatable {
    public init() {}
}

/// How one refresh ended.
public enum SessionRefreshOutcome: Equatable {
    /// A new pair was installed.
    case refreshed(SessionPair)
    /// `401 key-generation-superseded`: the provider is now superseded.
    case superseded
    /// `401 device-revoked`: this device was removed; the provider is now
    /// revoked.
    case revoked
    /// `401 refresh-invalid`: the credential is dead; sign in again.
    case invalid
    /// No refresh credential, or no account signed in.
    case noCredential
    /// Offline or any other failure; the current pair stays.
    case failed(String)
}

/// The session's token provider: holds the pair, refreshes it ahead of
/// expiry, and applies the `401` rules on every bearer request.
///
/// Thread-safe (`NSLock`), because the transport seam is reached from any
/// actor. The three callbacks are invoked off the lock, on no particular
/// actor; the repository hops to the main actor itself.
public final class SessionTokenProvider: @unchecked Sendable {

    /// The process-wide provider every REST client's request passes through.
    public static let shared = SessionTokenProvider()

    /// `POST /auth/session/refresh` with the refresh credential: the new pair,
    /// or a thrown ``AuthClientError`` (`401` with the body code).
    public typealias Refresher = @Sendable (_ refreshToken: String) async throws -> SessionPair

    private let lock = NSLock()
    private var accountIdB64: String?
    private var pair: SessionPair?
    /// Every access token installed for the current account, so a request
    /// still carrying an earlier one is recognised as this session's and
    /// gets the current token instead.
    private var knownAccessTokens: Set<String> = []
    /// `true` once the server answered `key-generation-superseded`; every
    /// later access is refused without a network call until ``clear()``.
    private var superseded = false
    /// `true` once the server answered `device-revoked` (ADR-0074); every
    /// later access is refused without a network call until ``clear()``.
    private var revoked = false
    private var inFlight: Task<SessionRefreshOutcome, Never>?

    /// Set by the repository. Absent means a refresh cannot run.
    public var refresher: Refresher? {
        get { lock.lock(); defer { lock.unlock() }; return _refresher }
        set { lock.lock(); _refresher = newValue; lock.unlock() }
    }
    private var _refresher: Refresher?

    /// The base URL of `services/auth`, the one service whose
    /// `device-revoked` is proof of a removal. Injectable for tests.
    public var authBaseURL: @Sendable () -> String = { ApiEndpoints.baseURL(for: .auth) }

    /// The clock, injectable for tests.
    public var nowMs: @Sendable () -> Int64 = { Int64(Date().timeIntervalSince1970 * 1000) }

    /// A refresh installed a new pair for the account.
    public var onPairRefreshed: (@Sendable (_ pair: SessionPair, _ accountIdB64: String) -> Void)?
    /// The account's keys were reset elsewhere (ADR-0073 section 3.5). Fired
    /// at most once per installed session.
    public var onSuperseded: (@Sendable (_ accountIdB64: String) -> Void)?
    /// This device was removed from the account on another device (ADR-0074
    /// section 2.3). Fired at most once per installed session.
    public var onDeviceRevoked: (@Sendable (_ accountIdB64: String) -> Void)?
    /// The refresh credential was refused as invalid: sign in again, nothing
    /// is wiped.
    public var onRefreshInvalid: (@Sendable (_ accountIdB64: String) -> Void)?

    public init() {}

    // MARK: - State

    /// Install the session pair for `accountIdB64`. A different account
    /// drops the previous account's tokens; the same account keeps every
    /// earlier access token as known.
    public func install(_ newPair: SessionPair, accountIdB64 newAccount: String) {
        lock.lock()
        if accountIdB64 != newAccount {
            knownAccessTokens = []
        }
        accountIdB64 = newAccount
        pair = newPair
        superseded = false
        revoked = false
        if !newPair.sessionToken.isEmpty {
            knownAccessTokens.insert(newPair.sessionToken)
        }
        lock.unlock()
    }

    /// Forget the session (logout, wipe, an account switch).
    public func clear() {
        lock.lock()
        accountIdB64 = nil
        pair = nil
        knownAccessTokens = []
        superseded = false
        revoked = false
        inFlight?.cancel()
        inFlight = nil
        lock.unlock()
    }

    /// The installed pair, or `nil` when signed out.
    public var current: SessionPair? {
        lock.lock()
        defer { lock.unlock() }
        return pair
    }

    /// The account the pair belongs to, or `nil`.
    public var account: String? {
        lock.lock()
        defer { lock.unlock() }
        return accountIdB64
    }

    /// Whether the server refused this session as superseded.
    public var isSuperseded: Bool {
        lock.lock()
        defer { lock.unlock() }
        return superseded
    }

    /// Whether the server refused this session as device-revoked.
    public var isDeviceRevoked: Bool {
        lock.lock()
        defer { lock.unlock() }
        return revoked
    }

    /// Whether `token` is an access token this session issued (current or
    /// earlier).
    public func isKnownAccessToken(_ token: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return knownAccessTokens.contains(token)
    }

    // MARK: - Access

    /// What a bearer request may use.
    public enum AccessOutcome: Equatable {
        /// Present this token.
        case token(String)
        /// The session is superseded: do not send.
        case superseded
        /// This device was removed: do not send.
        case revoked
        /// The refresh credential was refused as invalid.
        case invalid
        /// No session is installed.
        case signedOut
    }

    /// The access token to present now: the current one when more than
    /// ``SessionTokenRule/refreshAheadMs`` remain, else the one a refresh
    /// yields. A refresh that fails for a transport reason keeps the current
    /// token (the request then fails as it does offline).
    public func accessToken() async -> AccessOutcome {
        lock.lock()
        let snapshot = pair
        let wasSuperseded = superseded
        let wasRevoked = revoked
        lock.unlock()
        if wasRevoked { return .revoked }
        if wasSuperseded { return .superseded }
        guard let snapshot else { return .signedOut }
        switch SessionTokenRule.decide(pair: snapshot, nowMs: nowMs()) {
        case .useCurrent, .noRefreshCredential:
            return .token(snapshot.sessionToken)
        case .refreshFirst:
            switch await refresh() {
            case .refreshed(let fresh): return .token(fresh.sessionToken)
            case .superseded: return .superseded
            case .revoked: return .revoked
            case .invalid: return .invalid
            case .noCredential, .failed: return .token(snapshot.sessionToken)
            }
        }
    }

    /// Run one refresh now (the silent restore refreshes instead of reusing
    /// the persisted access token; a `401` on a bearer route refreshes once).
    /// Concurrent callers share the refresh in flight.
    @discardableResult
    public func refresh() async -> SessionRefreshOutcome {
        lock.lock()
        if revoked {
            lock.unlock()
            return .revoked
        }
        if superseded {
            lock.unlock()
            return .superseded
        }
        guard let snapshot = pair, let account = accountIdB64, let refresher = _refresher else {
            lock.unlock()
            return .noCredential
        }
        guard snapshot.refreshUsable(nowMs: nowMs()) else {
            lock.unlock()
            return .noCredential
        }
        if let running = inFlight {
            lock.unlock()
            return await running.value
        }
        let task = Task<SessionRefreshOutcome, Never> { [weak self] in
            guard let self else { return .failed("provider gone") }
            return await self.runRefresh(
                refreshToken: snapshot.refreshToken, deviceIdB64: snapshot.deviceIdB64,
                account: account, refresher: refresher)
        }
        inFlight = task
        lock.unlock()
        return await task.value
    }

    private func runRefresh(
        refreshToken: String,
        deviceIdB64: String?,
        account: String,
        refresher: Refresher
    ) async -> SessionRefreshOutcome {
        let outcome: SessionRefreshOutcome
        do {
            // A refresh keeps the device (ADR-0074 section 0.4), so the
            // client-side note of it moves to the new pair.
            let fresh = try await refresher(refreshToken).naming(deviceIdB64: deviceIdB64)
            outcome = .refreshed(fresh)
        } catch AuthClientError.http(let status, _, let body) where status == 401 {
            switch SessionTokenRule.classify401(body: Data(body.utf8)) {
            case .deviceRevoked: outcome = .revoked
            case .keyGenerationSuperseded: outcome = .superseded
            case .refreshInvalid, .other: outcome = .invalid
            }
        } catch {
            outcome = .failed(String(describing: type(of: error)))
        }
        var fire: (() -> Void)?
        lock.lock()
        inFlight = nil
        // The session may have been cleared or replaced while the refresh
        // was out; only the account it was made for takes the result.
        if accountIdB64 == account {
            switch outcome {
            case .refreshed(let fresh):
                pair = fresh
                knownAccessTokens.insert(fresh.sessionToken)
                let callback = onPairRefreshed
                fire = { callback?(fresh, account) }
            case .superseded:
                if !superseded {
                    superseded = true
                    let callback = onSuperseded
                    fire = { callback?(account) }
                }
            case .revoked:
                if !revoked {
                    revoked = true
                    let callback = onDeviceRevoked
                    fire = { callback?(account) }
                }
            case .invalid:
                let callback = onRefreshInvalid
                fire = { callback?(account) }
            case .noCredential, .failed:
                break
            }
        }
        lock.unlock()
        fire?()
        return outcome
    }

    /// A bearer route of auth or discovery answered `key-generation-superseded`
    /// directly (ADR-0073 sections 3.2 and 4.1): mark the session and fire the
    /// callback once.
    public func noteSuperseded() {
        var fire: (() -> Void)?
        lock.lock()
        if !superseded, let account = accountIdB64 {
            superseded = true
            let callback = onSuperseded
            fire = { callback?(account) }
        }
        lock.unlock()
        fire?()
    }

    /// A bearer route of auth answered `device-revoked` directly (ADR-0074
    /// section 2.3): mark the session and fire the callback once. Only
    /// auth's `401` is proof; discovery's `403 device-revoked` is not, and a
    /// caller that meets it runs ``refresh()`` and lets auth answer.
    public func noteDeviceRevoked() {
        var fire: (() -> Void)?
        lock.lock()
        if !revoked, let account = accountIdB64 {
            revoked = true
            let callback = onDeviceRevoked
            fire = { callback?(account) }
        }
        lock.unlock()
        fire?()
    }

    // MARK: - The transport hook

    /// Send `request` through `send`, applying the token rules when it
    /// carries an access token of this session. A request with no bearer, or
    /// with a bearer this session never issued (a call JWT, a test token), is
    /// sent as it is.
    public func perform(
        _ request: URLRequest,
        send: (URLRequest) async throws -> (Data, URLResponse)
    ) async throws -> (Data, URLResponse) {
        guard let bearer = Self.bearer(of: request), isKnownAccessToken(bearer) else {
            return try await send(request)
        }
        var first = request
        switch await accessToken() {
        case .token(let token):
            Self.setBearer(token, on: &first)
        case .superseded:
            throw SessionSupersededError()
        case .revoked:
            throw SessionDeviceRevokedError()
        case .invalid, .signedOut:
            break
        }
        let (data, response) = try await send(first)
        guard let http = response as? HTTPURLResponse, http.statusCode == 401 else {
            return (data, response)
        }
        let meaning = SessionTokenRule.classify401(body: data)
        switch SessionTokenRule.afterBearer401(meaning: meaning, alreadyRetried: false) {
        case .signOutSuperseded:
            noteSuperseded()
            return (data, response)
        case .signOutRevoked:
            // Only auth's answer is proof of a removal (ADR-0074 section
            // 2.3). The same body from any other service triggers nothing by
            // itself: auth is asked, and its answer to the refresh decides.
            if SessionTokenRule.revocationIsProof(requestURL: first.url, authBaseURL: authBaseURL()) {
                noteDeviceRevoked()
            } else {
                _ = await refresh()
            }
            return (data, response)
        case .report:
            return (data, response)
        case .refreshAndRetry:
            switch await refresh() {
            case .refreshed(let fresh):
                var retry = request
                Self.setBearer(fresh.sessionToken, on: &retry)
                return try await send(retry)
            case .superseded, .revoked:
                return (data, response)
            case .invalid, .noCredential, .failed:
                return (data, response)
            }
        }
    }

    static func bearer(of request: URLRequest) -> String? {
        guard let value = request.value(forHTTPHeaderField: "Authorization") else { return nil }
        let prefix = "Bearer "
        guard value.hasPrefix(prefix) else { return nil }
        let token = String(value.dropFirst(prefix.count))
        return token.isEmpty ? nil : token
    }

    static func setBearer(_ token: String, on request: inout URLRequest) {
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
    }
}
