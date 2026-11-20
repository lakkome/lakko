// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0099 step 3 / UX `reporting-and-moderation.md` section 4 — the owner's
// side of a suspended account: what auth's `403 account-suspended` carries,
// which requests this device still sends while suspended, the reason text
// and the two routes the suspended screen calls. Everything here is free of
// state except ``SuspensionStore`` and ``SuspensionClient``; the state lives
// in ``SuspensionCenter``.

/// The body code auth answers a suspended account with.
public let accountSuspendedCode = "account-suspended"

/// What a `403 account-suspended` from a route that authenticated the person
/// carries (a landing route, or `POST /auth/session/refresh`): the account,
/// the `s1` restricted session and an ordinary `r2` refresh credential.
///
/// The pair kept for a suspended account is ``pair``: the `s1` stands in the
/// access-token slot and lives as long as one, so the token provider serves
/// and renews it exactly as it serves an access token, and the `r2` keeps
/// asking auth whether the suspension is over.
public struct SuspendedLanding: Equatable, Sendable {
    /// The 16-byte account id.
    public let accountId: Data
    /// The `s1` in the access slot, the `r2`, their expiries, the generation.
    public let pair: SessionPair
    /// The fresh-auth grant the password and passkey sign-ins add, so a
    /// suspended account can still delete itself; `nil` elsewhere.
    public let freshGrant: FreshAuthGrant?

    /// The account id in the standard base64 every store on this device uses.
    public var accountIdB64: String { base64Encode(accountId) }

    /// Parse a `403` body. `nil` for a body that is not a full
    /// `account-suspended` landing (a bare `{"error":"account-suspended"}`
    /// from a bearer route carries no credentials and parses as `nil`).
    public static func parse(_ body: Data) -> SuspendedLanding? {
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              object["error"] as? String == accountSuspendedCode,
              let idText = object["account_id_b64"] as? String,
              let accountId = base64UrlDecodeNoPad(idText), accountId.count == accountIdLength,
              let restricted = object["restricted_token"] as? String, !restricted.isEmpty,
              let restrictedExpires = (object["restricted_expires_at_ms"] as? NSNumber)?.int64Value,
              let refresh = object["refresh_token"] as? String, !refresh.isEmpty,
              let refreshExpires = (object["refresh_expires_at_ms"] as? NSNumber)?.int64Value,
              let generation = (object["key_generation"] as? NSNumber)?.uint64Value
        else { return nil }
        let pair = SessionPair(
            sessionToken: restricted,
            sessionExpiresAtMs: restrictedExpires,
            refreshToken: refresh,
            refreshExpiresAtMs: refreshExpires,
            keyGeneration: generation)
        let grant = FreshAuthGrant.from(
            grant: object["fresh_grant"] as? String,
            expiresAtMs: (object["fresh_grant_expires_at_ms"] as? NSNumber)?.int64Value)
        return SuspendedLanding(accountId: accountId, pair: pair, freshGrant: grant)
    }

    /// Whether a JSON body names `account-suspended`, with or without
    /// credentials.
    public static func isSuspendedBody(_ body: Data) -> Bool {
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any] else { return false }
        return object["error"] as? String == accountSuspendedCode
    }
}

/// Thrown by the auth and passkey clients for a `403 account-suspended`.
///
/// Distinct from ``AuthClientError`` so it is never read as a refused
/// password: a landing route that throws it authenticated the person.
public struct AccountSuspendedError: Error, Equatable {
    /// The credentials, when the route carried them (a landing route or the
    /// refresh); `nil` from a bearer route.
    public let landing: SuspendedLanding?
    /// The OPAQUE export key the password sign-in computed before its
    /// finish was refused, so the device key is written as on a normal
    /// sign-in. `nil` on every other route.
    public var exportKey: Data?

    public init(landing: SuspendedLanding?, exportKey: Data? = nil) {
        self.landing = landing
        self.exportKey = exportKey
    }

    /// The error for a non-2xx answer, or `nil` when it is not a `403
    /// account-suspended`.
    public static func from(status: Int, body: Data) -> AccountSuspendedError? {
        guard status == 403, SuspendedLanding.isSuspendedBody(body) else { return nil }
        return AccountSuspendedError(landing: SuspendedLanding.parse(body))
    }
}

/// Thrown by the token provider instead of sending a request this device
/// does not send while its account is suspended (UX section 4: clients
/// honour the suspension; the Delivery Service does not know accounts).
public struct SessionSuspendedError: Error, Equatable {
    public init() {}
}

/// Which requests leave this device while its account is suspended.
public enum SuspendedRequestRule {

    /// The auth routes that take the `s1` as their bearer (ADR-0099 step 3):
    /// the suspension read, the appeal, account deletion and its passkey
    /// ceremony, logout, and token issuance (auth issues the `blob-read`
    /// context only, so files in chats still open; every other context
    /// answers `account-suspended`).
    static let restrictedAuthPaths: Set<String> = [
        "/account/suspension",
        "/account/suspension/appeal",
        "/account",
        "/auth/session/logout",
        "/auth/action/start",
        "/auth/action/finish",
        "/auth/tokens",
    ]

    /// A route outside auth that takes the `s1` as its bearer.
    public struct BearerRoute: Equatable, Sendable {
        public let base: String
        public let method: String
        public let path: String

        public init(base: String, method: String, path: String) {
            self.base = base
            self.method = method
            self.path = path
        }
    }

    /// The account deletion's two clean-ups outside auth, which discovery and
    /// storage accept with the `s1`: the directory entry and the backup.
    public static var liveBearerRoutes: [BearerRoute] {
        [
            BearerRoute(base: ApiEndpoints.baseURL(for: .discovery), method: "DELETE", path: "/discovery/account"),
            BearerRoute(base: ApiEndpoints.baseURL(for: .storage), method: "DELETE", path: "/backup"),
        ]
    }

    /// The one write a suspended device still sends to the write bases:
    /// buying the blob read pass (a POST that writes nothing), so files open.
    static let allowedWritePaths: Set<String> = ["/blob/pass/read"]

    /// Whether `request` may be sent while suspended.
    ///
    /// * A request carrying this session's bearer goes only to the auth
    ///   routes that accept the `s1` (every other service refuses it, and
    ///   nothing else is the account's to do while suspended).
    /// * A write to the Delivery Service, the channels service or the blob
    ///   store never goes: no message, commit, post, knock or upload leaves a
    ///   suspended device. Reads go, so the chats can still be read.
    /// * Everything else (anonymous routes, reads) goes.
    public static func allows(
        method: String,
        url: URL?,
        carriesSessionBearer: Bool,
        authBase: String,
        writeBases: [String],
        bearerRoutes: [BearerRoute] = []
    ) -> Bool {
        guard let url else { return true }
        let text = url.absoluteString
        if carriesSessionBearer {
            for route in bearerRoutes where route.method.uppercased() == method.uppercased() {
                if let path = remainder(of: text, under: route.base), bare(path) == route.path { return true }
            }
            guard let path = remainder(of: text, under: authBase) else { return false }
            return restrictedAuthPaths.contains(bare(path))
        }
        if method.uppercased() != "GET" && method.uppercased() != "HEAD" {
            for base in writeBases {
                guard let path = remainder(of: text, under: base) else { continue }
                let bare = path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? path
                return allowedWritePaths.contains(bare)
            }
        }
        return true
    }

    /// The live service bases whose writes are withheld.
    public static var liveWriteBases: [String] {
        [ApiEndpoints.baseURL(for: .ds), ApiEndpoints.baseURL(for: .channels), ApiEndpoints.baseURL(for: .storage)]
    }

    /// `path` without its query.
    static func bare(_ path: String) -> String {
        path.split(separator: "?", maxSplits: 1).first.map(String.init) ?? path
    }

    /// `text` with `base` taken off its start, or `nil` when it is not under
    /// `base` (`base` itself reads as the empty path).
    static func remainder(of text: String, under base: String) -> String? {
        guard !base.isEmpty else { return nil }
        if text == base { return "" }
        guard text.hasPrefix(base + "/") else { return nil }
        return String(text.dropFirst(base.count))
    }
}

/// When suspended mode asks auth again (UX section 4: the screen goes away
/// at the next refresh once the suspension ended).
public enum SuspensionRefreshRule {
    /// Every 10 minutes while in suspended mode. Chosen, not measured.
    public static let intervalMs: Int64 = 600_000

    /// Whether a refresh is due at `nowMs` after one at `lastMs`.
    public static func due(lastMs: Int64?, nowMs: Int64) -> Bool {
        guard let lastMs else { return true }
        return nowMs - lastMs >= intervalMs || nowMs < lastMs
    }
}

/// `GET /account/suspension`, decoded.
public struct SuspensionDetails: Equatable, Sendable {
    public enum Appeal: String, Equatable, Sendable {
        case none, sent, declined
    }

    /// The reason byte.
    public let reason: UInt8
    /// The kebab reason name (`spam`, `child-safety`, ... or `unknown`).
    public let reasonName: String
    /// The end, or `nil` for a suspension with no end date.
    public let untilMs: Int64?
    public let appeal: Appeal

    public init(reason: UInt8, reasonName: String, untilMs: Int64?, appeal: Appeal) {
        self.reason = reason
        self.reasonName = reasonName
        self.untilMs = untilMs
        self.appeal = appeal
    }

    /// The same details with the appeal state replaced.
    public func with(appeal: Appeal) -> SuspensionDetails {
        SuspensionDetails(reason: reason, reasonName: reasonName, untilMs: untilMs, appeal: appeal)
    }

    /// What the route answered.
    public enum Answer: Equatable {
        case suspended(SuspensionDetails)
        case notSuspended
    }

    /// Decode the route's JSON, or `nil` for a body that is not its shape.
    public static func decode(_ body: Data) -> Answer? {
        guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let suspended = object["suspended"] as? Bool else { return nil }
        guard suspended else { return .notSuspended }
        let reason = (object["reason"] as? NSNumber)?.uint8Value ?? 0
        let name = (object["reason_name"] as? String) ?? "unknown"
        let until = (object["until_ms"] as? NSNumber)?.int64Value
        let appeal = Appeal(rawValue: (object["appeal"] as? String) ?? "none") ?? .none
        return .suspended(SuspensionDetails(reason: reason, reasonName: name, untilMs: until, appeal: appeal))
    }
}

/// The reason text: the report sheet's catalog keys (`report.reason.*`).
public enum SuspensionReasonText {
    /// The catalog key for a kebab reason name; an unknown name reads as
    /// `report.reason.other`.
    public static func catalogKey(reasonName: String) -> String {
        let known = ["spam", "harassment", "hate", "violence", "sexual", "child-safety", "impersonation", "other"]
        guard known.contains(reasonName) else { return "report.reason.other" }
        return "report.reason." + reasonName.replacingOccurrences(of: "-", with: "_")
    }
}

/// The per-account record that this account is in suspended mode: a flag
/// only (the `s1` and `r2` are kept as the session pair, in the Keychain).
/// Survives a restart, so the suspended screen comes back at launch.
public struct SuspensionStore {
    private let defaults: UserDefaults
    static let prefix = "lakko.suspension.v1."

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func isSuspended(accountIdB64: String) -> Bool {
        defaults.bool(forKey: Self.prefix + accountIdB64)
    }

    public func set(_ suspended: Bool, accountIdB64: String) {
        if suspended {
            defaults.set(true, forKey: Self.prefix + accountIdB64)
        } else {
            defaults.removeObject(forKey: Self.prefix + accountIdB64)
        }
    }
}

/// The two routes the suspended screen calls with the `s1`, and the appeal
/// seal.
public struct SuspensionClient: Sendable {

    public typealias Perform = @Sendable (URLRequest) async throws -> (Data, URLResponse)

    public let baseURL: String
    private let perform: Perform

    public init(baseURL: String = ApiEndpoints.baseURL(for: .auth), perform: Perform? = nil) {
        var base = baseURL
        while base.hasSuffix("/") { base.removeLast() }
        self.baseURL = base
        self.perform = perform ?? { request in
            try await AntiCensorshipTransport.perform(request, session: .shared, config: .current)
        }
    }

    /// What reading the suspension gave.
    public enum StatusResult: Equatable {
        case answer(SuspensionDetails.Answer)
        case failed
    }

    /// `GET /account/suspension` with the `s1`.
    public func status(restrictedToken: String) async -> StatusResult {
        guard !restrictedToken.isEmpty, let url = URL(string: baseURL + "/account/suspension") else { return .failed }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("Bearer \(restrictedToken)", forHTTPHeaderField: "Authorization")
        guard let (data, response) = try? await perform(request),
              let http = response as? HTTPURLResponse, http.statusCode == 200,
              let answer = SuspensionDetails.decode(data) else { return .failed }
        return .answer(answer)
    }

    /// What sending the appeal gave.
    public enum AppealResult: Equatable {
        /// `204`, or `409 appeal-already-sent`: an appeal is on file.
        case sent
        /// `409 not-suspended`: the suspension is over.
        case notSuspended
        case failed
    }

    /// `POST /account/suspension/appeal` with the sealed appeal.
    public func appeal(sealed: Data, restrictedToken: String) async -> AppealResult {
        guard !restrictedToken.isEmpty,
              let url = URL(string: baseURL + "/account/suspension/appeal") else { return .failed }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(restrictedToken)", forHTTPHeaderField: "Authorization")
        request.httpBody = try? JSONSerialization.data(withJSONObject: ["sealed_b64": sealed.base64EncodedString()])
        guard let (data, response) = try? await perform(request),
              let http = response as? HTTPURLResponse else { return .failed }
        return Self.appealResult(status: http.statusCode, body: data)
    }

    static func appealResult(status: Int, body: Data) -> AppealResult {
        if (200..<300).contains(status) { return .sent }
        if status == 409 {
            switch AnonTokenRoute.errorCode(in: body) {
            case "appeal-already-sent": return .sent
            case "not-suspended": return .notSuspended
            default: return .failed
            }
        }
        return .failed
    }
}

/// The appeal's limits and its seal (UX section 4).
public enum SuspensionAppeal {
    /// The field's limit. The core counts Unicode scalars, so the field and
    /// its counter count them too: what the field lets through is what the
    /// seal accepts.
    public static let maxCharacters = 1000

    /// The length the counter shows.
    public static func length(_ text: String) -> Int {
        text.unicodeScalars.count
    }

    /// Whether `text` can be sent: not empty once trimmed, within the limit.
    public static func sendable(_ text: String) -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        return !trimmed.isEmpty && trimmed.unicodeScalars.count <= maxCharacters
    }

    /// `text` cut to the limit at a character boundary, for the field.
    public static func clamp(_ text: String) -> String {
        guard text.unicodeScalars.count > maxCharacters else { return text }
        var kept = ""
        var used = 0
        for character in text {
            let cost = character.unicodeScalars.count
            if used + cost > maxCharacters { break }
            kept.append(character)
            used += cost
        }
        return kept
    }
}

/// The info blocks a suspended account is not offered: the ones that send
/// (UX section 4: calls and adding contacts are not offered). The shared
/// rule decides the blocks; this takes the sending ones out on top of it.
public enum SuspendedInfo {
    static let sending: Set<InfoSection> = [.call, .addMember, .inviteLink, .addContact, .invitePeople]

    /// `sections` without the sending blocks while `suspended`.
    public static func withoutSending(_ sections: [InfoSection], suspended: Bool) -> [InfoSection] {
        suspended ? sections.filter { !sending.contains($0) } : sections
    }
}
