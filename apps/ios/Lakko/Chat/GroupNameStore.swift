// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0051 §1 / §3 — the shared group name this device holds for one group:
/// the name and the setter's stamp, kept together because the stamp is what
/// orders a later rename against a growth re-send that carries the ORIGINAL
/// stamp verbatim (the ADR-0044 lesson: persist the stamp, re-emit it
/// unchanged).
public struct StoredGroupName: Equatable, Codable {
    /// The name; empty means "cleared" (the title falls back to the
    /// member-derived one) — kept as a row so its stamp still orders.
    public let name: String
    /// Setter-clock UNIX milliseconds at which the name was set.
    public let updatedAtMs: UInt64

    public init(name: String, updatedAtMs: UInt64) {
        self.name = name
        self.updatedAtMs = updatedAtMs
    }

    /// The name the title shows, or `nil` when it is empty / cleared.
    public var displayName: String? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }
}

/// ADR-0051 §3 — per-account map `groupIdHex -> { name, updatedAtMs }` in
/// `UserDefaults`, the ``KnownGroupsStore`` shape: a small non-secret
/// map (a group name is content the members already share, and this device
/// is a member), namespaced by account so two accounts on one device do not
/// collide. App-private — the notification extension only needs to
/// recognise the envelope, never to read the name.
public final class GroupNameStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    public func load(accountIdB64: String, groupIdHex: String) -> StoredGroupName? {
        guard let raw = map(accountIdB64: accountIdB64)[groupIdHex],
              let name = raw["name"] as? String,
              let stamp = raw["updatedAtMs"] as? NSNumber
        else { return nil }
        return StoredGroupName(name: name, updatedAtMs: stamp.uint64Value)
    }

    /// Persist `value` for the group, replacing whatever was stored. The
    /// caller decides whether the write is allowed (``GroupNameGate``); the
    /// store itself is a plain last-writer-wins map.
    public func record(accountIdB64: String, groupIdHex: String, value: StoredGroupName) {
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = [
            "name": value.name,
            "updatedAtMs": NSNumber(value: value.updatedAtMs),
        ]
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    /// Forget one group's name (the group was deleted locally).
    public func remove(accountIdB64: String, groupIdHex: String) {
        var current = map(accountIdB64: accountIdB64)
        current[groupIdHex] = nil
        defaults.set(current, forKey: Self.key(accountIdB64))
    }

    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.key(accountIdB64))
    }

    private func map(accountIdB64: String) -> [String: [String: Any]] {
        defaults.dictionary(forKey: Self.key(accountIdB64)) as? [String: [String: Any]] ?? [:]
    }

    static func key(_ accountIdB64: String) -> String {
        "group_name.\(accountIdB64)"
    }
}

/// ADR-0051 §2 — what the receive path does with one incoming group-name
/// envelope. The shape copies ``PushGroupRouting/SecretVerdict``: the two
/// accept arms are distinguished because the identical case is decided
/// BEFORE authority (a re-send from anyone changes nothing), and the three
/// reject arms are distinguished because the log has to say which gate
/// refused.
public enum GroupNameVerdict: Equatable {
    /// Byte-for-byte what is stored (name and stamp). Nothing to write.
    case acceptIdentical
    /// Authorised and strictly newer: store it and re-title.
    case accept
    /// A plain member of a role-table group. Dropped (invariant #4).
    case rejectNotAuthorized
    /// Authorised, but not newer than what is stored — a growth re-send of an
    /// older name, or two admins renaming at once with the other one winning.
    case rejectStale
    /// The roles or the roster could not be read, or the sender leaf is not in
    /// the roster. Cannot authorise a change: dropped, fail-closed.
    case rejectUnverifiable
}

/// ADR-0051 §2 — the pure receive-side decision for a group name, with the
/// same authority table the push-routing secret uses
/// (``PushGroupRouting/SecretAuthority``, read from the LIVE group by
/// ``PushGroupRouting/resolveSenderAuthority(group:senderLeaf:)``).
///
/// | Role table | Sender | Verdict |
/// |---|---|---|
/// | present | owner or admin | accept if newer |
/// | present | member | `rejectNotAuthorized` |
/// | present | unresolvable / table unreadable | `rejectUnverifiable` |
/// | absent (legacy group) | any member | accept if newer |
///
/// The gate runs before the order comparison; the identical case runs before
/// both. `isNewer` is the core's `groupNameIsNewer` (strictly greater, so a
/// re-send with the same stamp is never "newer"), injectable so the matrix is
/// testable without the FFI.
public enum GroupNameGate {

    public static func classify(
        incomingName: String,
        incomingUpdatedAtMs: UInt64,
        stored: StoredGroupName?,
        senderKey: Data?,
        authority: PushGroupRouting.SecretAuthority,
        isNewer: (_ incoming: UInt64, _ stored: UInt64) -> Bool = {
            groupNameIsNewer(incomingUpdatedAtMs: $0, storedUpdatedAtMs: $1)
        }
    ) -> GroupNameVerdict {
        // (1) Idempotent first — a re-observed identical name changes
        // nothing, so it needs no authority and no ordering. This is what
        // makes a growth re-send from an admin, or our own envelope echoed
        // back through a second device, harmless.
        if let stored, stored.name == incomingName, stored.updatedAtMs == incomingUpdatedAtMs {
            return .acceptIdentical
        }

        // (2) Authority — the receiver's check, never the sender's claim.
        switch authority {
        case .noRoleTable:
            // A legacy / open group has nobody to check against; ADR-0051 §2
            // keeps it open rather than leaving such a group nameless.
            break
        case .unreadable:
            return .rejectUnverifiable
        case .roleTable(let ownerKey, let admins):
            guard let senderKey, !senderKey.isEmpty else { return .rejectUnverifiable }
            if senderKey == ownerKey || admins.contains(senderKey) {
                break
            }
            return .rejectNotAuthorized
        }

        // (3) Nothing stored to order against — the first name.
        guard let stored else { return .accept }

        // (4) The order decides which authorised write survives. Strictly
        // greater, through the core, never re-derived here.
        return isNewer(incomingUpdatedAtMs, stored.updatedAtMs) ? .accept : .rejectStale
    }
}
