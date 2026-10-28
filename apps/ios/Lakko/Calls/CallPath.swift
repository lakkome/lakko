// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which network path a call's media takes, as one log line. The twin of the
/// web `apps/web/src/calls/call-path.ts` and the Android `CallPath.kt`.
///
/// A delay spike that hits both directions at once (a burst of concealed
/// audio with nothing lost, measured on a one-to-one call on 2026-10-25) is
/// either the local network or a relay in between, and nothing in the logs
/// said which. This line does: the selected ICE candidate pair's local and
/// remote candidate types and protocols, the transport to the TURN server for
/// a relayed local candidate, and the pair's round-trip time.
///
///     call path: local=relay/udp/tls via=edge.lakko.me remote=host/udp rtt=41ms
///
/// No address or port of either side is printed. `via=` names the TURN
/// server, which is Lakko's own, by its host name, and only when it is a name.
/// The line is written when the selected pair is first known and when the
/// path changes, not on every statistics reading; a changed round-trip time
/// alone is not a new path.
enum CallPath {
    /// The selected pair of a statistics report, reduced to what the line prints.
    struct Reading: Equatable {
        /// `<type>/<protocol>[/<relayProtocol>]` of the local candidate.
        let local: String
        /// `<type>/<protocol>` of the remote candidate.
        let remote: String
        /// The TURN server's host name for a relayed local candidate.
        let via: String?
        /// The pair's current round-trip time in milliseconds, when reported.
        let rttMs: Int?
        /// What identifies the path for change detection (no round-trip time).
        let key: String
    }

    /// The statistics entry types the reading needs.
    static let entryTypes: Set<String> = ["transport", "candidate-pair", "local-candidate", "remote-candidate"]

    private static func string(_ entry: CallMediaStatEntry?, _ key: String) -> String? {
        guard let v = entry?.values[key] as? String, !v.isEmpty else { return nil }
        return v
    }

    private static func selectedPair(
        _ entries: [CallMediaStatEntry], byId: [String: CallMediaStatEntry]
    ) -> CallMediaStatEntry? {
        for entry in entries where entry.type == "transport" {
            if let id = string(entry, "selectedCandidatePairId"), let pair = byId[id] {
                return pair
            }
        }
        let pairs = entries.filter { $0.type == "candidate-pair" }
        let succeeded: (CallMediaStatEntry) -> Bool = { string($0, "state") == "succeeded" }
        let flag: (CallMediaStatEntry, String) -> Bool = { entry, key in
            (entry.values[key] as? NSNumber)?.boolValue ?? false
        }
        return pairs.first(where: { flag($0, "selected") })
            ?? pairs.first(where: { succeeded($0) && flag($0, "nominated") })
            ?? pairs.first(where: succeeded)
    }

    /// The host of a TURN URI (`turn:host:port?...`), or nil when it names an
    /// address rather than a host name, or does not parse.
    static func turnHostName(_ url: String?) -> String? {
        guard let url else { return nil }
        let lower = url.lowercased()
        let rest: Substring
        if lower.hasPrefix("turns:") {
            rest = lower.dropFirst("turns:".count)
        } else if lower.hasPrefix("turn:") {
            rest = lower.dropFirst("turn:".count)
        } else {
            return nil
        }
        let hostPort = rest.split(separator: "?", maxSplits: 1, omittingEmptySubsequences: false).first ?? ""
        if hostPort.hasPrefix("[") { return nil }
        let host = String(hostPort.split(separator: ":", omittingEmptySubsequences: false).first ?? "")
        if host.isEmpty || host.allSatisfy({ $0.isNumber || $0 == "." }) { return nil }
        let allowed = host.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "." || $0 == "-") }
        return allowed ? host : nil
    }

    /// The selected pair of a statistics report, or nil before there is one.
    static func read(_ entries: [CallMediaStatEntry]) -> Reading? {
        var byId: [String: CallMediaStatEntry] = [:]
        for entry in entries { byId[entry.id] = entry }
        guard let pair = selectedPair(entries, byId: byId),
              let localId = string(pair, "localCandidateId"), let local = byId[localId],
              let remoteId = string(pair, "remoteCandidateId"), let remote = byId[remoteId]
        else { return nil }
        let localType = string(local, "candidateType") ?? "?"
        let relayProtocol = localType == "relay" ? string(local, "relayProtocol") : nil
        let localText = "\(localType)/\(string(local, "protocol") ?? "?")"
            + (relayProtocol.map { "/\($0)" } ?? "")
        let remoteText = "\(string(remote, "candidateType") ?? "?")/\(string(remote, "protocol") ?? "?")"
        let via = localType == "relay" ? turnHostName(string(local, "url")) : nil
        let rttMs = (pair.values["currentRoundTripTime"] as? NSNumber).flatMap { n -> Int? in
            let seconds = n.doubleValue
            return seconds.isFinite && seconds >= 0 ? Int((seconds * 1000).rounded()) : nil
        }
        return Reading(
            local: localText, remote: remoteText, via: via, rttMs: rttMs,
            key: "\(pair.id)|\(localText)|\(via ?? "")|\(remoteText)")
    }

    /// The log line for a path.
    static func describe(_ path: Reading) -> String {
        "call path: local=\(path.local)" + (path.via.map { " via=\($0)" } ?? "")
            + " remote=\(path.remote) rtt=\(path.rttMs.map { "\($0)ms" } ?? "-")"
    }
}

/// One call's path log: a line when the path is first known or changes.
final class CallPathLog {
    private var lastKey: String?

    /// The line to log for this reading, or nil when the path is unchanged or
    /// not known yet.
    func note(_ entries: [CallMediaStatEntry]) -> String? {
        guard let path = CallPath.read(entries), path.key != lastKey else { return nil }
        lastKey = path.key
        return CallPath.describe(path)
    }
}
