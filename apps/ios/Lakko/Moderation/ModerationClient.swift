// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0099 section 2 — the client half of `services/moderation`.
///
/// Three routes, all anonymous: a report carries nothing that names the
/// person who sent it (UX `reporting-and-moderation.md` section 0), so no
/// request here has an `Authorization` header, a cookie or the session's
/// connection pool (``AnonymousHTTP``). The send spends one anonymous token
/// of context `report`. Every request goes through
/// ``AntiCensorshipTransport/perform(_:session:config:)``.
///
/// The service's paths start with `/moderation/`, so against the production
/// base `https://api.lakko.me/moderation` a send is
/// `https://api.lakko.me/moderation/moderation/report`, and against the test
/// stack `https://test.lakko.me/native/moderation/moderation/report`.
public final class ModerationClient: @unchecked Sendable {

    /// The Info.plist key carrying the pinned root key (base64url, 32 bytes).
    public static let rootKeyInfoPlistKey = "LakkoModerationRootKey"

    /// The pinned offline root key, or `nil` when the build carries none
    /// (reporting is then unavailable).
    public static var pinnedRootKey: Data? {
        rootKey(fromInfoValue: Bundle.main.object(forInfoDictionaryKey: rootKeyInfoPlistKey) as? String)
    }

    /// Parse a configured root key: base64url (padded or not) of exactly 32
    /// bytes, else `nil`.
    static func rootKey(fromInfoValue raw: String?) -> Data? {
        guard let raw = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !raw.isEmpty,
              let key = base64UrlDecodeNoPad(raw), key.count == 32 else { return nil }
        return key
    }

    public typealias Perform = (URLRequest) async throws -> (Data, URLResponse)

    public let baseURL: String
    private let tokens: AnonTokenSource
    private let perform: Perform

    public init(
        baseURL: String = ApiEndpoints.baseURL(for: .moderation),
        tokens: AnonTokenSource = AnonTokenPool.shared,
        perform: Perform? = nil
    ) {
        var base = baseURL
        while base.hasSuffix("/") { base.removeLast() }
        self.baseURL = base
        self.tokens = tokens
        self.perform = perform ?? { request in
            try await AntiCensorshipTransport.perform(
                request, session: AnonymousHTTP.session, config: .current)
        }
    }

    /// What fetching the moderator list gave.
    public enum ListResult: Equatable {
        case list(Data)
        /// The service says reporting is off (`503 reporting-unavailable`).
        case unavailable
        /// Network failure or any other answer.
        case failed
    }

    /// What sending a report gave (UX 2.3).
    public enum SendResult: Equatable {
        case sent(receiptId: String)
        /// The day's allowance is used up, or the service is rate limiting.
        case limited
        /// Reporting, or anonymous tokens, are off on this server.
        case unavailable
        /// No network, or any answer that is none of the above.
        case failed
    }

    /// A report's status, from its receipt (UX 2.3, Your reports).
    public enum Status: String, Equatable, Codable {
        case received, action, none
    }

    /// `GET /moderation/moderators`: the raw signed list.
    public func fetchModeratorList() async -> ListResult {
        guard let url = URL(string: baseURL + "/moderation/moderators") else { return .failed }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        guard let (data, response) = try? await perform(request),
              let http = response as? HTTPURLResponse else { return .failed }
        return Self.listResult(status: http.statusCode, body: data)
    }

    static func listResult(status: Int, body: Data) -> ListResult {
        if (200..<300).contains(status), !body.isEmpty { return .list(body) }
        if status == 503, AnonTokenRoute.errorCode(in: body) == "reporting-unavailable" { return .unavailable }
        return .failed
    }

    /// `POST /moderation/report` with the sealed report as the raw body.
    public func send(report: Data) async -> SendResult {
        guard let url = URL(string: baseURL + "/moderation/report") else { return .failed }
        do {
            let (data, http) = try await AnonTokenRoute.send(context: .report, tokens: tokens) { header in
                var request = URLRequest(url: url)
                request.httpMethod = "POST"
                request.setValue("application/octet-stream", forHTTPHeaderField: "Content-Type")
                request.setValue(header, forHTTPHeaderField: AnonTokenRoute.headerName)
                request.httpBody = report
                let (data, response) = try await self.perform(request)
                guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
                return (data, http)
            }
            return Self.sendResult(status: http.statusCode, body: data)
        } catch let token as AnonTokenUnavailable {
            return Self.sendResult(tokenOutcome: token)
        } catch {
            return .failed
        }
    }

    static func sendResult(status: Int, body: Data) -> SendResult {
        switch status {
        case 201, 200:
            guard let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
                  let id = object["receipt_id"] as? String, !id.isEmpty else { return .failed }
            return .sent(receiptId: id)
        case 429:
            return .limited
        case 503:
            let code = AnonTokenRoute.errorCode(in: body)
            return code == "reporting-unavailable" || code == "anon-tokens-disabled" ? .unavailable : .failed
        default:
            return .failed
        }
    }

    static func sendResult(tokenOutcome: AnonTokenUnavailable) -> SendResult {
        switch tokenOutcome {
        case .quotaExhausted: return .limited
        case .disabled: return .unavailable
        default: return .failed
        }
    }

    /// `GET /moderation/report/{receipt_id}`: the status, or `nil` when it
    /// could not be had (unknown receipt, no network).
    public func status(receiptId: String) async -> Status? {
        guard Self.isReceiptId(receiptId),
              let url = URL(string: baseURL + "/moderation/report/" + receiptId) else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        guard let (data, response) = try? await perform(request),
              let http = response as? HTTPURLResponse, http.statusCode == 200,
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let raw = object["status"] as? String else { return nil }
        return Status(rawValue: raw)
    }

    /// A receipt id is 22 base64url characters; anything else is never put in
    /// a URL.
    static func isReceiptId(_ id: String) -> Bool {
        id.count == 22 && id.unicodeScalars.allSatisfy {
            $0.isASCII && (CharacterSet.alphanumerics.contains($0) || $0 == "-" || $0 == "_")
        }
    }
}

/// ADR-0099 section 2 — the moderator list this device accepts: verified
/// under the pinned root key, and never older than the newest version this
/// device has seen (a list with a lower version is treated as unavailable,
/// so a server cannot roll back to a list holding a revoked moderator).
public enum ModeratorListGate {
    static let highestKey = "lakko.moderation.list-version.v1"

    /// Whether a list of `version` may be used when `highest` was seen.
    public static func accepts(version: UInt64, highestSeen: UInt64?) -> Bool {
        guard let highestSeen else { return true }
        return version >= highestSeen
    }

    /// The verified version of `list`, recorded as the new highest when it is
    /// one, or `nil` when it does not verify or is older than one seen here.
    public static func admit(
        _ list: Data, rootKey: Data, defaults: UserDefaults = .standard,
        version verify: (Data, Data) throws -> UInt64 = { try moderatorListVersion(moderatorList: $0, rootKey: $1) }
    ) -> UInt64? {
        guard let version = try? verify(list, rootKey) else { return nil }
        let highest = (defaults.object(forKey: highestKey) as? NSNumber)?.uint64Value
        guard accepts(version: version, highestSeen: highest) else { return nil }
        if highest.map({ version > $0 }) ?? true {
            defaults.set(NSNumber(value: version), forKey: highestKey)
        }
        return version
    }

    /// The list a report, an appeal or a standing proof is sealed to or
    /// checked against: fetched, then admitted (verified under `rootKey`,
    /// not older than this device's floor, the floor raised on a newer one).
    /// No root key, or a list that is not admitted, is `.unavailable`, so
    /// nothing is ever sealed to a list that may still name a removed
    /// moderator. Written once so the report sheet and the appeal cannot
    /// drift apart.
    public static func admittedList(
        rootKey: Data?,
        fetch: () async -> ModerationClient.ListResult,
        admitting: (Data, Data) -> UInt64? = { ModeratorListGate.admit($0, rootKey: $1) }
    ) async -> ModerationClient.ListResult {
        guard let rootKey else { return .unavailable }
        let fetched = await fetch()
        guard case .list(let list) = fetched else { return fetched }
        return admitting(list, rootKey) != nil ? .list(list) : .unavailable
    }
}
