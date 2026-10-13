// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

// ADR-0074 section 3.5a rule 3 — the profile re-send after the account's keys
// moved, made durable.
//
// The grants every peer holds open the OLD mailbox only; the re-sent profile
// is what carries the grants for the new one, and nothing else does. So the
// re-send is owed on every device that holds the new keys (the rotating one
// and each kept one) from the moment the reach keys move until every
// conversation took the envelope:
//
//   * a marker is written when the keys move;
//   * each conversation that took the envelope is recorded in it, so a retry
//     sends only to the ones still missing;
//   * it is cleared only by a round in which no conversation failed;
//   * it is retried at each session start.
//
// The marker holds a public key and group ids, in `UserDefaults`; the account
// wipe clears it.

/// The marker and its rules.
public enum ProfileResendOwed {

    /// What is still owed, for the account key the re-send is for.
    public struct Record: Codable, Equatable {
        /// Standard base64 of the account key the keys moved TO. A marker
        /// written for an earlier key is a different re-send.
        public var accountKeyB64: String
        /// The conversations (group id hex) that took the envelope.
        public var reached: [String]

        public init(accountKeyB64: String, reached: [String] = []) {
            self.accountKeyB64 = accountKeyB64
            self.reached = reached
        }
    }

    /// The marker after the keys moved to `accountKeyB64`: a fresh one, unless
    /// one for that same key is already kept (a re-adoption must not send
    /// again to the conversations already reached).
    public static func marking(_ held: Record?, accountKeyB64: String) -> Record {
        if let held, held.accountKeyB64 == accountKeyB64 { return held }
        return Record(accountKeyB64: accountKeyB64)
    }

    /// The conversations a round still sends to: every current one the marker
    /// does not list as reached, in a stable order.
    public static func targets(groups: [String], record: Record) -> [String] {
        let reached = Set(record.reached)
        return groups.sorted().filter { !reached.contains($0) }
    }

    /// The marker after a round that sent to `attempted` and saw `failed` not
    /// land: `nil` (cleared) only when nothing failed.
    public static func after(_ record: Record, attempted: [String], failed: [String]) -> Record? {
        guard !failed.isEmpty else { return nil }
        var next = record
        let landed = Set(attempted).subtracting(failed)
        next.reached = Array(Set(record.reached).union(landed)).sorted()
        return next
    }

    /// The per-account marker store.
    public struct Store {
        static let prefix = "me.lakko.profile.resend-owed.v1."
        private let defaults: UserDefaults

        public init(defaults: UserDefaults = .standard) {
            self.defaults = defaults
        }

        public func load(accountIdB64: String) -> Record? {
            guard let data = defaults.data(forKey: Self.prefix + accountIdB64) else { return nil }
            return try? JSONDecoder().decode(Record.self, from: data)
        }

        public func put(_ record: Record?, accountIdB64: String) {
            guard let record, let data = try? JSONEncoder().encode(record) else {
                defaults.removeObject(forKey: Self.prefix + accountIdB64)
                return
            }
            defaults.set(data, forKey: Self.prefix + accountIdB64)
        }

        public func clear(accountIdB64: String) {
            defaults.removeObject(forKey: Self.prefix + accountIdB64)
        }
    }
}
