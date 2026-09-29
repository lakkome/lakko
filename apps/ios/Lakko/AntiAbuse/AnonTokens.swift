// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0054 S3 - anonymous tokens, the client half.
//
// A token is drawn from `services/auth` with the session (see
// ``AnonTokenPool`` / ``AuthAnonTokenIssuer``) and spent, one per anonymous
// action, at discovery, the DS or push-gw in the `x-lakko-anon-token` header.
// The spending request carries nothing else that names the account: no
// `Authorization`, no cookie, and it travels on its own connection pool
// (``AnonymousHTTP``). UX copy: `docs/ux/anti-abuse.md` section 3.

/// The rationed actions a token pays for. The raw values are the wire context
/// names; `AnonTokenContextTests` pins them against `anonTokenContexts()`.
public enum AnonTokenContext: String, CaseIterable, Sendable {
    /// `POST /discovery/oprf` - every username or channel lookup, and the
    /// OPRF step of a username / channel-name registration.
    case discoveryLookup = "discovery-lookup"
    /// `POST /discovery/channel/register`.
    case discoveryRegister = "discovery-register"
    /// The three ledger consumes (`/discovery/{invite,call-invite,group-invite}/{id}/consume`).
    case inviteConsume = "invite-consume"
    /// `POST /ds/group/{hex}/knock` (a probe counts as one knock).
    case dsKnock = "ds-knock"
    /// push-gw register, unregister, group subscribe and group unsubscribe.
    case pushRegister = "push-register"
    /// `POST /v1/channels/{id}/subscribers` (ADR-0059 section 4).
    case channelSubscribe = "channel-subscribe"
    /// `GET /v1/directory/search` (ADR-0059 section 5).
    case directorySearch = "directory-search"

    /// Tokens asked for per issuance (ADR-0054 S5): a lookup is the common
    /// action; the others are rare, so a small batch leaves little unspent
    /// bearer value lying on the device.
    public var batchSize: Int {
        switch self {
        case .discoveryLookup: return 8
        case .pushRegister: return 4
        case .inviteConsume: return 2
        case .dsKnock: return 2
        case .discoveryRegister: return 1
        case .channelSubscribe: return 2
        case .directorySearch: return 1
        }
    }

    /// A background refill starts when the context holds fewer than this:
    /// below 2 for lookups (so a search rarely waits), empty for the rest.
    public var lowWater: Int {
        switch self {
        case .discoveryLookup: return 2
        default: return 1
        }
    }
}

/// Why no token could be spent, or why a token route refused the one that was.
///
/// None of these means a lost session: the client never signs out on them.
public enum AnonTokenUnavailable: Error, Equatable, LocalizedError {
    /// No session to draw tokens with (signed out, or before sign-in).
    case noSession
    /// The server has anonymous tokens switched off (`503 anon-tokens-disabled`,
    /// from the issuer or from the token route).
    case disabled
    /// The account's allowance for this action is used up for the window
    /// (`429 anon-token-quota-exhausted`).
    case quotaExhausted(retryAfterSecs: Int?)
    /// No token arrived within the bounded wait of an action.
    case timedOut
    /// A token route refused twice (`401 anon-token-*`), or refused with
    /// `anon-token-required` (a client bug).
    case refused(code: String)
    /// Issuance failed for another reason (network, a non-2xx, a malformed
    /// response).
    case issuanceFailed(String)
    /// The issuer's proof did not verify under the key from the key route:
    /// the whole batch was discarded.
    case verificationFailed(String)
    /// `400 epoch-not-issuable` from the issuer. Internal to the pool, which
    /// rebuilds the request once for `currentEpoch`.
    case epochNotIssuable(currentEpoch: UInt64)

    public var errorDescription: String? {
        switch self {
        case .quotaExhausted:
            return AnonTokenCopy.allowanceUsed
        default:
            return AnonTokenCopy.networkFailure
        }
    }

    /// The token refusal inside `error`, looking through the repository
    /// wrappers the screens see (`ChatError.wrapped`, `ContactsError.wrapped`).
    public static func find(in error: Error) -> AnonTokenUnavailable? {
        if let direct = error as? AnonTokenUnavailable { return direct }
        if let chat = error as? ChatError, case .wrapped(_, let underlying) = chat {
            return find(in: underlying)
        }
        if let contacts = error as? ContactsError, case .wrapped(let underlying) = contacts {
            return find(in: underlying)
        }
        return nil
    }

    /// Whether `error` is, or wraps, a used-up daily allowance.
    public static func isQuotaExhausted(_ error: Error) -> Bool {
        if case .quotaExhausted = find(in: error) { return true }
        return false
    }
}

/// The copy for token outcomes, verbatim from `docs/ux/anti-abuse.md` section 3.
public enum AnonTokenCopy {
    /// Search (a username or channel lookup) - allowance used up.
    public static let searchAllowanceUsed =
        "You have looked up a lot of people today. Try again tomorrow."
    /// Invite link, group link, channel name - allowance used up.
    public static let allowanceUsed =
        "You have used today's allowance for this. Try again tomorrow."
    /// Tokens off, no token in time, or a refused token: the generic network
    /// copy the action already had.
    public static let networkFailure = "No connection. Check your network and try again."

    /// The search copy for `error` when it is a used-up allowance, else `nil`.
    public static func searchMessage(for error: Error) -> String? {
        AnonTokenUnavailable.isQuotaExhausted(error) ? searchAllowanceUsed : nil
    }

    /// The link / channel-name copy for `error` when it is a token outcome,
    /// else `nil`.
    public static func linkMessage(for error: Error) -> String? {
        guard let token = AnonTokenUnavailable.find(in: error) else { return nil }
        if case .quotaExhausted = token { return allowanceUsed }
        return networkFailure
    }
}

/// Where a token comes from. ``AnonTokenPool`` in production; a fake in tests.
public protocol AnonTokenSource: AnyObject, Sendable {
    /// One token for `context`, waiting (bounded) for a refill when none is at
    /// hand. The token is removed from the pool whether or not the caller then
    /// spends it successfully.
    func take(_ context: AnonTokenContext) async throws -> AnonToken
}

/// Sending one request on a token route.
public enum AnonTokenRoute {

    /// The request header name (`x-lakko-anon-token`), from the core.
    public static let headerName: String = anonTokenHeaderName()

    /// The `401` codes that mean "this token, not the next one": drop it, take
    /// another and retry once.
    public static let retryOnceCodes: Set<String> = [
        "anon-token-spent",
        "anon-token-invalid",
        "anon-token-expired",
        "anon-token-wrong-context",
        "anon-token-malformed",
    ]

    /// Send `perform` with one token of `context`; on a retryable token refusal
    /// take a second token and send once more.
    ///
    /// `perform` receives the header VALUE and must put it under
    /// ``headerName`` on a request that carries no `Authorization` and no
    /// cookie, sent on an anonymous session. Returns the final response
    /// whatever its status, except for the token outcomes, which throw
    /// ``AnonTokenUnavailable``: `503 anon-tokens-disabled` -> `.disabled`, a
    /// second retryable refusal or `anon-token-required` -> `.refused`.
    public static func send(
        context: AnonTokenContext,
        tokens: AnonTokenSource,
        perform: (String) async throws -> (Data, HTTPURLResponse)
    ) async throws -> (Data, HTTPURLResponse) {
        var retried = false
        while true {
            let token = try await tokens.take(context)
            let (data, http) = try await perform(token.headerValue)
            let code = errorCode(in: data)
            if http.statusCode == 503, code == "anon-tokens-disabled" {
                throw AnonTokenUnavailable.disabled
            }
            if http.statusCode == 401, let code, code.hasPrefix("anon-token") {
                if retryOnceCodes.contains(code), !retried {
                    NSLog("[anon-token] %@ refused %@, retrying once with another token",
                          context.rawValue, code)
                    retried = true
                    continue
                }
                throw AnonTokenUnavailable.refused(code: code)
            }
            return (data, http)
        }
    }

    /// The `error` (or `code`) string of a JSON error body, or `nil`.
    public static func errorCode(in data: Data) -> String? {
        guard !data.isEmpty,
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        if let code = object["error"] as? String { return code }
        return object["code"] as? String
    }
}

/// The connection pool token routes travel on: its own ephemeral
/// `URLSession`, so an anonymous request never shares a connection (or a
/// cookie jar, or a cache) with the requests that carry the session token.
public enum AnonymousHTTP {
    /// The process-wide anonymous session.
    public static let session: URLSession = {
        let config = URLSessionConfiguration.ephemeral
        config.httpCookieAcceptPolicy = .never
        config.httpShouldSetCookies = false
        config.httpCookieStorage = nil
        config.urlCache = nil
        config.urlCredentialStorage = nil
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        return URLSession(configuration: config)
    }()

    /// The anonymous session a client should use when its caller passed
    /// `session`: production callers keep the default `.shared` and get
    /// ``session``; a test that injected a stubbed session keeps it for both.
    public static func session(pairedWith session: URLSession) -> URLSession {
        session === URLSession.shared ? Self.session : session
    }
}

/// A token taken earlier, handed out first; later takes go to `fallback`.
///
/// For logout: the push unregister must spend a token after the session (and
/// with it the pool) is gone, so the caller takes one beforehand.
public final class PrepaidAnonToken: AnonTokenSource, @unchecked Sendable {
    private let lock = NSLock()
    private var token: AnonToken?
    private let fallback: AnonTokenSource

    public init(_ token: AnonToken, then fallback: AnonTokenSource) {
        self.token = token
        self.fallback = fallback
    }

    public func take(_ context: AnonTokenContext) async throws -> AnonToken {
        if let prepaid = takePrepaid(context) { return prepaid }
        return try await fallback.take(context)
    }

    private func takePrepaid(_ context: AnonTokenContext) -> AnonToken? {
        lock.withLock {
            guard token?.context == context.rawValue else { return nil }
            defer { token = nil }
            return token
        }
    }
}
