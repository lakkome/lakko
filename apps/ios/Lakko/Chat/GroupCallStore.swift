// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Per-account map `groupIdHex -> OngoingGroupCall` in `UserDefaults`, the
/// ``ReadWatermarkStore`` / ``GroupNameStore`` shape.
///
/// Why durable: a group call is announced by journal rows, and the DS poll
/// resumes past rows already applied. A record held only in memory was lost on
/// relaunch, so the device showed no Join banner for a call still running and
/// its Call button started a second call beside it.
///
/// Why this cannot resurrect a dead call: a member counts only while its
/// newest join / heartbeat is fresh (``GroupCallReducer/memberFreshMs``), so a
/// reloaded record whose participants stopped renewing is not live, and
/// records past ``GroupCallReducer/maxLiveAgeMs`` are dropped on load.
///
/// Advisory state only (see ``OngoingGroupCall``): a call id, the media flags,
/// the start time and the members' signature keys with last-seen times. No key
/// material.
public final class GroupCallStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// The stored records, minus any older than the max live age at `nowMs`.
    /// An undecodable blob reads as no records.
    func load(accountIdB64: String, nowMs: Int64) -> [String: OngoingGroupCall] {
        guard let data = defaults.data(forKey: Self.key(accountIdB64)),
              let decoded = try? JSONDecoder().decode([String: OngoingGroupCall].self, from: data)
        else { return [:] }
        return Self.withinMaxAge(decoded, nowMs: nowMs)
    }

    /// Replace the account's stored records.
    func save(accountIdB64: String, records: [String: OngoingGroupCall]) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard !records.isEmpty else {
            clear(accountIdB64: accountIdB64)
            return
        }
        guard let data = try? JSONEncoder().encode(records) else { return }
        defaults.set(data, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    static func withinMaxAge(
        _ records: [String: OngoingGroupCall], nowMs: Int64
    ) -> [String: OngoingGroupCall] {
        records.filter { nowMs &- $0.value.startedAtMs <= GroupCallReducer.maxLiveAgeMs }
    }

    static func key(_ accountIdB64: String) -> String {
        "group_calls.\(accountIdB64)"
    }
}
