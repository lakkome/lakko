// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import WebRTC

/// The ICE servers of a call: Lakko's own, and nobody else's. The twin of
/// `apps/web/src/calls/ice-servers.ts` and the Android `IceServers.kt`.
///
/// Until 2026-10-24 every call, one-to-one and group, started from Google's
/// public STUN servers (`stun.l.google.com`), which told Google each person's
/// network address and when they called. The rule now:
///
///   - the TURN entry `services/auth` mints with the call admission, as given;
///   - a STUN entry on the host and port of each of its `turn:` URIs, since
///     Lakko's coturn answers STUN there (measured on the test stack
///     2026-10-24: a binding request to edge2.lakko.me:3478 got a success
///     response). `turns:` URIs give none: STUN is UDP, and TLS is another
///     port;
///   - nothing else. Without a TURN entry (no TURN configured, or minting
///     failed) the list is empty: host candidates only. A group call still
///     connects then, since the SFU learns the person's outside address from
///     their own packets; a one-to-one call across two NATs may not, which is
///     what the TURN entry is for.
enum IceServers {
    /// One ICE server row as plain values, so the rule is testable without
    /// comparing `RTCIceServer` objects.
    struct Entry: Equatable, Sendable {
        let urls: [String]
        let username: String?
        let credential: String?

        init(urls: [String], username: String? = nil, credential: String? = nil) {
            self.urls = urls
            self.username = username
            self.credential = credential
        }

        /// The minted TURN credential as an ICE server row.
        init(turn: TurnCredential) {
            self.init(urls: turn.urls, username: turn.username, credential: turn.password)
        }
    }

    /// The STUN URLs on the hosts of `turn:` URIs: `stun:host:port`, the port
    /// 3478 when the URI names none, each host and port once. Pure.
    static func stunUrls(fromTurn urls: [String]) -> [String] {
        var out: [String] = []
        for url in urls {
            guard url.lowercased().hasPrefix("turn:") else { continue }
            let afterScheme = url.dropFirst("turn:".count)
            let rest = afterScheme.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false)
                .first.map(String.init) ?? ""
            guard let hostPort = parseHostPort(rest) else { continue }
            let stun = "stun:\(hostPort.host):\(hostPort.port)"
            if !out.contains(stun) { out.append(stun) }
        }
        return out
    }

    private static func parseHostPort(_ s: String) -> (host: String, port: Int)? {
        guard !s.isEmpty else { return nil }
        let host: String
        var portText: String?
        if s.hasPrefix("[") {
            guard let end = s.firstIndex(of: "]") else { return nil }
            host = String(s[...end])
            let after = s[s.index(after: end)...]
            if !after.isEmpty {
                guard after.hasPrefix(":") else { return nil }
                portText = String(after.dropFirst())
            }
        } else if let colon = s.lastIndex(of: ":") {
            host = String(s[..<colon])
            portText = String(s[s.index(after: colon)...])
        } else {
            host = s
        }
        guard !host.isEmpty, host != "[]" else { return nil }
        guard let portText else { return (host, 3478) }
        guard (1...5).contains(portText.count),
              portText.allSatisfy({ $0.isASCII && $0.isNumber }),
              let port = Int(portText),
              port > 0, port <= 65535
        else { return nil }
        return (host, port)
    }

    /// A call's ICE servers from the minted TURN entry, or none. Pure.
    static func entries(for turn: Entry?) -> [Entry] {
        guard let turn else { return [] }
        let stun = stunUrls(fromTurn: turn.urls)
        return stun.isEmpty ? [turn] : [Entry(urls: stun), turn]
    }

    /// The servers' URLs, for the call log: no username, no credential.
    static func line(_ entries: [Entry]) -> String {
        let urls = entries.flatMap(\.urls)
        return urls.isEmpty ? "none (host candidates only)" : urls.joined(separator: ", ")
    }

    /// The `RTCIceServer` rows WebRTC takes.
    static func rtcIceServers(_ entries: [Entry]) -> [RTCIceServer] {
        entries.map { entry in
            if let username = entry.username, let credential = entry.credential {
                return RTCIceServer(urlStrings: entry.urls, username: username, credential: credential)
            }
            return RTCIceServer(urlStrings: entry.urls)
        }
    }

    /// Resolve a call's ICE servers through `client` (or none without one),
    /// logging the URLs once. Fail-soft: no credential leaves host candidates
    /// only, never a refused call (an ICE availability choice, not an E2EE
    /// fallback; invariant #4 is untouched).
    @MainActor
    static func resolve(_ client: TurnCredentialClient?) async -> [RTCIceServer] {
        let turn = await client?.fetchEntryOrNil()
        let servers = entries(for: turn)
        NSLog("call ice servers: %@", line(servers))
        return rtcIceServers(servers)
    }
}
