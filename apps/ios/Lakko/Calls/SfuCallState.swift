// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// What the SFU knows about a call id (`services/sfu` call lifetime).
enum SfuCallState: String, Equatable {
    case unknown, live, closing, ended
}

/// Asking the SFU whether a call is still there (`docs/ux/calls.md` section
/// 4.3).
///
/// A group call's record lives in the chat's MLS group and a member whose app
/// died publishes nothing, so after a crash the call it was alone in stays
/// "in progress" on every other phone for the freshness window, and whoever
/// taps Join lands in an empty room. The one party that knows a room is empty
/// is the SFU: the call ends there five seconds after the last socket goes,
/// and `GET /v1/calls/{call_id}` (`Authorization: Bearer <call jwt>`) says
/// so. A phone showing the
/// call as joinable asks every ``intervalMs`` and, on `ended`, closes the
/// chat's record itself by publishing the End signal — so the banner goes on
/// every device, not only its own. Shared word-for-word with web and Android.
enum SfuCallStateProbe {
    /// How often a device offering Join asks the SFU. Chosen: a stale banner
    /// lives at most this long past the SFU's own five-second grace.
    static let intervalMs: Int64 = 10_000

    /// The state route for `callIdHex`, on the HTTP origin behind the SFU's
    /// WebSocket base (`wss://host/sfu` -> `https://host/sfu`). The token
    /// never rides the URL (invariant #5: a request line in a log must not
    /// name who asked about which call); ``request(sfuBaseUrl:callIdHex:jwt:)``
    /// carries it in a header.
    static func url(sfuBaseUrl: String, callIdHex: String) -> URL? {
        var base = sfuBaseUrl
        if base.lowercased().hasPrefix("wss:") {
            base = "https:" + base.dropFirst(4)
        } else if base.lowercased().hasPrefix("ws:") {
            base = "http:" + base.dropFirst(3)
        }
        while base.hasSuffix("/") { base.removeLast() }
        return URL(string: "\(base)/v1/calls/\(callIdHex)")
    }

    /// The probe request: the same call-admission JWT the socket uses, bound
    /// to this call id, as a bearer token.
    static func request(sfuBaseUrl: String, callIdHex: String, jwt: String) -> URLRequest? {
        guard let url = url(sfuBaseUrl: sfuBaseUrl, callIdHex: callIdHex) else { return nil }
        var request = URLRequest(url: url)
        request.setValue("Bearer \(jwt)", forHTTPHeaderField: "Authorization")
        request.timeoutInterval = 5
        return request
    }

    /// The route's answer, or `nil` for anything that is not one of the four
    /// words (a proxy error page, an old server) — the caller then changes
    /// nothing, since a probe that cannot be read must never end a live call.
    static func parse(_ data: Data) -> SfuCallState? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let word = object["state"] as? String
        else { return nil }
        return SfuCallState(rawValue: word)
    }

    /// One probe over `URLSession`. Never throws; `nil` on any failure.
    static func fetch(sfuBaseUrl: String, callIdHex: String, jwt: String) async -> SfuCallState? {
        guard let request = request(sfuBaseUrl: sfuBaseUrl, callIdHex: callIdHex, jwt: jwt) else {
            return nil
        }
        guard let (data, response) = try? await URLSession.shared.data(for: request),
              let http = response as? HTTPURLResponse, http.statusCode == 200
        else { return nil }
        return parse(data)
    }
}
