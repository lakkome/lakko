// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0047 §4 — the client half of the GIF proxy (`services/gif-proxy`).
/// Mirror of `apps/web/src/media/gif-client.ts`.
///
/// Everything the picker shows and everything a pick sends goes through
/// Lakko's own proxy. The service's paths start with `/gifs/`, so against the
/// production base `https://api.lakko.me/gifs` a search is
/// `https://api.lakko.me/gifs/gifs/search?q=&offset=`; against a local
/// `lakko-svc-gif-proxy` it is `http://localhost:8091/gifs/search`. An item's
/// relative `preview_url` / `full_url` (`/gifs/media/{id}/{variant}`) is
/// resolved against the same base — and ONLY when it is exactly that relative
/// proxy shape, so the client never follows a response somewhere else.
///
/// Every route needs `Authorization: Bearer <session token>`. Requests go
/// through ``AntiCensorshipTransport/perform(_:session:config:)`` like every
/// other Lakko REST client.
///
/// Failure mapping, per UX spec §5 states:
///
///   * 503 `gif-provider-not-configured` -> ``GifFailure/notConfigured`` (the tab hides)
///   * 429 `rate-limited` + `Retry-After`  -> ``GifFailure/rateLimited(retryAfterSecs:)``
///   * 401                                  -> ``GifFailure/unauthorized``
///   * 502 / any other status / network     -> ``GifFailure/unavailable`` (Retry)
public final class GifClient: @unchecked Sendable {

    /// Local `lakko-svc-gif-proxy` dev bind; the simulator reaches the Mac.
    /// The fallback when neither `LakkoGifProxyUrl` nor the API base URL is
    /// set (see ``configuredBaseURL``).
    public static let defaultBaseURL = ApiService.gifs.localDevBaseURL
    /// The production base, behind nginx's `/gifs` location on `api.lakko.me`.
    public static let productionBaseURL = "https://api.lakko.me/gifs"

    /// UX spec §5 — about 350 ms after typing stops.
    public static let searchDebounceMs = 350
    /// Mirrors `services/gif-proxy` `MAX_QUERY_CHARS`.
    public static let maxQueryChars = 100
    /// Mirrors the proxy's full-rendition cap.
    public static let maxFullBytes = 8 * 1024 * 1024
    /// Mirrors the proxy's preview cap.
    public static let maxPreviewBytes = 2 * 1024 * 1024
    /// ADR-0047 §1 — attachment dimensions are bounded to this.
    public static let maxDimension = 4096
    /// Content types the proxy may answer media with (ADR-0047 §4).
    public static let mediaTypes: Set<String> = ["image/gif", "image/webp", "video/mp4"]

    /// The Info.plist key a deployment sets to point the client at its proxy.
    public static let infoPlistKey = "LakkoGifProxyUrl"

    /// The configured base: the `LakkoGifProxyUrl` Info.plist value when
    /// non-empty (a per-feature override), else `<LakkoApiBaseUrl>/gifs` from
    /// ``ApiEndpoints``, else the dev default (the same default-to-localhost
    /// posture every client shares).
    public static var configuredBaseURL: String {
        let raw = (Bundle.main.object(forInfoDictionaryKey: infoPlistKey) as? String)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return raw.isEmpty ? ApiEndpoints.baseURL(for: .gifs) : raw
    }

    public typealias Perform = (URLRequest) async throws -> (Data, URLResponse)

    public let baseURL: String
    private let sessionToken: () -> String?
    private let perform: Perform

    public init(
        baseURL: String = GifClient.configuredBaseURL,
        session: URLSession = .shared,
        sessionToken: @escaping () -> String?,
        perform: Perform? = nil
    ) {
        var base = baseURL
        while base.hasSuffix("/") { base.removeLast() }
        self.baseURL = base
        self.sessionToken = sessionToken
        self.perform = perform ?? { request in
            try await AntiCensorshipTransport.perform(request, session: session, config: .current)
        }
    }

    // MARK: - URL building

    /// The list URL for `query` (trimmed and capped; empty means Trending) at
    /// `offset`.
    public func listURL(query: String, offset: Int) -> URL? {
        let q = String(query.trimmingCharacters(in: .whitespacesAndNewlines).prefix(Self.maxQueryChars))
        let off = offset > 0 ? offset : 0
        guard var components = URLComponents(string: baseURL) else { return nil }
        let basePath = components.path
        // `URLQueryItem` leaves `+` (and other sub-delimiters) unescaped, which
        // a server reads as a space or separator; percent-encode everything
        // outside the unreserved set instead.
        if q.isEmpty {
            components.path = basePath + "/gifs/trending"
            components.percentEncodedQuery = "offset=\(off)"
        } else {
            components.path = basePath + "/gifs/search"
            components.percentEncodedQuery = "q=\(Self.formEncode(q))&offset=\(off)"
        }
        return components.url
    }

    private static let unreserved = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~"
    )

    static func formEncode(_ value: String) -> String {
        value.addingPercentEncoding(withAllowedCharacters: unreserved) ?? ""
    }

    /// `baseURL` + `relative` when `relative` is exactly a proxy media path;
    /// `nil` otherwise (absolute URLs, other hosts, traversal, query strings).
    public func mediaURL(relative: Any?) -> URL? {
        guard let relative = relative as? String, Self.isProxyMediaPath(relative) else { return nil }
        return URL(string: baseURL + relative)
    }

    static func isProxyMediaPath(_ path: String) -> Bool {
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        // "", "gifs", "media", id, variant
        guard path.hasPrefix("/"), parts.count == 5, parts[0].isEmpty,
              parts[1] == "gifs", parts[2] == "media",
              parts[4] == "preview" || parts[4] == "full" else {
            return false
        }
        let id = parts[3]
        return (1...64).contains(id.count) && id.unicodeScalars.allSatisfy { s in
            s.isASCII && (CharacterSet.alphanumerics.contains(s))
        }
    }

    // MARK: - Parsing

    /// Parse a list response body. Items that do not match the contract are
    /// dropped individually; a body that is not a list at all is `nil`.
    public func parsePage(_ data: Data) -> GifPage? {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let rawItems = root["items"] as? [Any] else {
            return nil
        }
        var items: [GifItem] = []
        for case let r as [String: Any] in rawItems {
            guard let id = r["id"] as? String,
                  let preview = mediaURL(relative: r["preview_url"]),
                  let full = mediaURL(relative: r["full_url"]),
                  let width = Self.dimension(r["width"]),
                  let height = Self.dimension(r["height"]) else {
                continue
            }
            items.append(GifItem(id: id, previewURL: preview, fullURL: full, width: width, height: height))
        }
        var nextOffset: Int?
        if let n = root["next_offset"] as? NSNumber, !(n === kCFBooleanTrue || n === kCFBooleanFalse) {
            let v = n.doubleValue
            if v == v.rounded(), v > 0, v <= Double(Int32.max) { nextOffset = Int(v) }
        }
        return GifPage(items: items, nextOffset: nextOffset)
    }

    private static func dimension(_ value: Any?) -> Int? {
        guard let n = value as? NSNumber, !(n === kCFBooleanTrue || n === kCFBooleanFalse) else { return nil }
        let v = n.doubleValue
        guard v == v.rounded(), v >= 1, v <= Double(maxDimension) else { return nil }
        return Int(v)
    }

    /// Map a non-2xx response to the failure the UI shows. `body` is only read
    /// for 503: only the proxy's own body counts as "not configured" — a 503
    /// from nginx in front of a stopped service is an outage, and hiding the
    /// tab for an outage would be wrong.
    public static func failure(status: Int, retryAfter: String?, body: Data) -> GifFailure {
        switch status {
        case 503:
            let code = (try? JSONSerialization.jsonObject(with: body) as? [String: Any])?["error"] as? String
            return code == "gif-provider-not-configured" ? .notConfigured : .unavailable
        case 401:
            return .unauthorized
        case 429:
            let secs = retryAfter.flatMap { Double($0.trimmingCharacters(in: .whitespaces)) }
            if let secs, secs.isFinite, secs > 0 {
                return .rateLimited(retryAfterSecs: Int(secs.rounded(.up)))
            }
            return .rateLimited(retryAfterSecs: 1)
        default:
            return .unavailable
        }
    }

    // MARK: - Requests

    private func authorizedRequest(_ url: URL) -> URLRequest? {
        guard let token = sessionToken(), !token.isEmpty else { return nil }
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.cachePolicy = .reloadIgnoringLocalCacheData
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return request
    }

    public func fetchPage(query: String, offset: Int) async -> GifResult<GifPage> {
        guard let url = listURL(query: query, offset: offset) else { return .failure(.unavailable) }
        guard let request = authorizedRequest(url) else { return .failure(.unauthorized) }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await perform(request)
        } catch {
            return .failure(.unavailable)
        }
        guard let http = response as? HTTPURLResponse else { return .failure(.unavailable) }
        guard (200..<300).contains(http.statusCode) else {
            return .failure(Self.failure(
                status: http.statusCode,
                retryAfter: http.value(forHTTPHeaderField: "Retry-After"),
                body: data
            ))
        }
        guard let page = parsePage(data) else { return .failure(.unavailable) }
        return .ok(page)
    }

    /// Fetch one rendition through the proxy. Refuses a URL that is not this
    /// proxy's media path, a response whose type is not an allowed media type,
    /// or a body over `maxBytes`, so nothing outside the contract is ever
    /// decoded or encrypted into a message.
    public func fetchMedia(_ url: URL, maxBytes: Int) async -> GifResult<GifMedia> {
        guard url.absoluteString.hasPrefix(baseURL + "/gifs/media/"),
              Self.isProxyMediaPath(String(url.absoluteString.dropFirst(baseURL.count))) else {
            return .failure(.unavailable)
        }
        guard let request = authorizedRequest(url) else { return .failure(.unauthorized) }
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await perform(request)
        } catch {
            return .failure(.unavailable)
        }
        guard let http = response as? HTTPURLResponse else { return .failure(.unavailable) }
        guard (200..<300).contains(http.statusCode) else {
            return .failure(Self.failure(
                status: http.statusCode,
                retryAfter: http.value(forHTTPHeaderField: "Retry-After"),
                body: data
            ))
        }
        let type = (http.value(forHTTPHeaderField: "Content-Type") ?? "")
            .split(separator: ";").first.map { $0.trimmingCharacters(in: .whitespaces).lowercased() } ?? ""
        guard Self.mediaTypes.contains(type), !data.isEmpty, data.count <= maxBytes else {
            return .failure(.unavailable)
        }
        return .ok(GifMedia(bytes: data, mime: type))
    }
}

public struct GifItem: Equatable, Identifiable, Hashable {
    public let id: String
    public let previewURL: URL
    public let fullURL: URL
    public let width: Int
    public let height: Int
}

public struct GifPage: Equatable {
    public let items: [GifItem]
    public let nextOffset: Int?
}

public struct GifMedia: Equatable {
    public let bytes: Data
    /// The response's type, parameters stripped; always in ``GifClient/mediaTypes``.
    public let mime: String
}

public enum GifFailure: Equatable, Error {
    case notConfigured
    case rateLimited(retryAfterSecs: Int)
    case unauthorized
    case unavailable
}

public enum GifResult<T: Equatable>: Equatable {
    case ok(T)
    case failure(GifFailure)
}

/// UX spec §5 copy.
public enum GifCopy {
    public static let privacyNote =
        "GIF search goes through Lakko's server, which sees what you search for. Sent GIFs are encrypted like any file."
    public static let noResults = "No GIFs found"
    public static let unavailable = "GIFs are unavailable right now"
    public static let rateLimited = "Too many searches \u{2014} try again in a moment"
    public static let attribution = "Powered by GIPHY"
}

/// Trailing-edge debounce: ``schedule(_:)`` runs the LAST scheduled closure
/// once `delayMs` pass with no newer call. ``flush()`` runs a pending call now
/// (Return in the search field); ``cancel()`` drops it (the picker closed).
///
/// The timer is injected so the debounce is testable without waiting.
@MainActor
public final class Debouncer {

    public typealias Timer = (_ delayMs: Int, _ fire: @escaping @MainActor () -> Void) -> () -> Void

    nonisolated public static let dispatchTimer: Timer = { delayMs, fire in
        let item = DispatchWorkItem { MainActor.assumeIsolated { fire() } }
        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(delayMs), execute: item)
        return { item.cancel() }
    }

    private let delayMs: Int
    private let timer: Timer
    private var cancelPending: (() -> Void)?
    private var pending: (@MainActor () -> Void)?
    private var generation = 0

    public init(delayMs: Int, timer: @escaping Timer = Debouncer.dispatchTimer) {
        self.delayMs = delayMs
        self.timer = timer
    }

    public func schedule(_ fn: @escaping @MainActor () -> Void) {
        cancelPending?()
        pending = fn
        generation += 1
        let mine = generation
        cancelPending = timer(delayMs) { [weak self] in
            guard let self, self.generation == mine else { return }
            self.cancelPending = nil
            let run = self.pending
            self.pending = nil
            run?()
        }
    }

    public func flush() {
        cancelPending?()
        cancelPending = nil
        generation += 1
        let run = pending
        pending = nil
        run?()
    }

    public func cancel() {
        cancelPending?()
        cancelPending = nil
        generation += 1
        pending = nil
    }
}
