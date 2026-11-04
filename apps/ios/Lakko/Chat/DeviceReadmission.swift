// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0050 section 2.4 / 2.6 — the re-admission arm, and its mirror.
///
/// iOS mirror of `apps/web/src/chat/device-readmission.ts`.
///
/// A person's device that has published a KeyPackage but holds no leaf in a
/// chat the person is in should be added: a device restored by three words
/// with no other device online, a device whose pairing fan-out was cut short,
/// a contact's new phone. Any member may propose it (the core's second
/// self-service arm), so every client re-checks, on a bounded schedule, the
/// device list of each account in each chat against that chat's roster.
///
/// The mirror: a leaf of THIS account whose device no longer has a directory
/// row (`forgetDevice` deletes its row before wiping) is retired, by this
/// account's own devices only — a plain member may not remove another
/// account's leaf.
///
/// What the planner cannot know is why a device is absent from a roster: never
/// added, or removed by an admin. A device this client SAW removed from the
/// chat is remembered per chat and never re-admitted by it; a removal that
/// happened before this client joined is invisible to it, and the device would
/// be re-admitted — the arm's known limit, recorded in the ADR.
public enum DeviceReadmission {

    /// How often one chat is re-checked.
    public static let intervalMs: Int64 = 10 * 60_000

    /// A device row from the directory, as fetched.
    public struct DirectoryDevice: Equatable {
        public let deviceKeyB64: String
        public let keyPackage: Data

        public init(deviceKeyB64: String, keyPackage: Data) {
            self.deviceKeyB64 = deviceKeyB64
            self.keyPackage = keyPackage
        }
    }

    /// A roster leaf with the person it resolves to.
    public struct RosterLeafKeys: Equatable {
        public let leafKeyB64: String
        public let personKeyB64: String

        public init(leafKeyB64: String, personKeyB64: String) {
            self.leafKeyB64 = leafKeyB64
            self.personKeyB64 = personKeyB64
        }
    }

    /// One account in the chat with its directory list; `devices == nil` when
    /// the fetch failed — nothing is decided for that account.
    public struct AccountDevices: Equatable {
        public let personKeyB64: String
        public let devices: [DirectoryDevice]?

        public init(personKeyB64: String, devices: [DirectoryDevice]?) {
            self.personKeyB64 = personKeyB64
            self.devices = devices
        }
    }

    public struct Plan: Equatable {
        /// KeyPackages to propose an Add for.
        public let admit: [DirectoryDevice]
        /// Own-account leaf keys to propose a Remove for.
        public let retire: [String]
    }

    /// - Parameter suppressAdmit: the person keys whose directory devices are
    ///   NOT proposed for an Add by this arm: an account whose published key
    ///   could not be confirmed as the roster's person key. After a reset the
    ///   directory lists the NEW identity's devices under the same account id;
    ///   those are added by the reset repair (ADR-0073 R.7), which checks each
    ///   KeyPackage's account key, never by this arm. The retire half is
    ///   untouched: it is about this account's own leaves.
    ///
    /// - Parameter ownDeviceGroupRoster: ADR-0074 section 2.3 — the leaf keys
    ///   (base64) of the account's own DeviceGroup, or `nil` when it could
    ///   not be read. A device of the OWN account is proposed for an Add only
    ///   when it is in that roster: the DeviceGroup is the account's own,
    ///   MLS-authenticated record of its devices, and a removed device is not
    ///   in it, so one that publishes itself again is not put back. A roster
    ///   that could not be read admits no own device (the next check reads it
    ///   again). The per-chat `removed` memory stays as defence in depth, and
    ///   stays the only memory for a contact's devices. The retire half is
    ///   untouched.
    public static func plan(
        roster: [RosterLeafKeys],
        ownPersonKeyB64: String,
        accounts: [AccountDevices],
        removed: Set<String>,
        ownDeviceGroupRoster: Set<String>?,
        suppressAdmit: Set<String> = []
    ) -> Plan {
        let rosterKeys = Set(roster.map { $0.leafKeyB64 })
        var admit: [DirectoryDevice] = []
        var retire: [String] = []
        var seen = Set<String>()
        for account in accounts {
            guard let devices = account.devices else { continue }
            let listed = Set(devices.map { $0.deviceKeyB64 })
            let suppressed = suppressAdmit.contains(account.personKeyB64)
            let own = account.personKeyB64 == ownPersonKeyB64
            for device in devices where !suppressed {
                if rosterKeys.contains(device.deviceKeyB64) { continue }
                if own, !ownDeviceAdmissible(
                    deviceKeyB64: device.deviceKeyB64, ownDeviceGroupRoster: ownDeviceGroupRoster) {
                    continue
                }
                if removed.contains(device.deviceKeyB64) { continue }
                if seen.contains(device.deviceKeyB64) { continue }
                seen.insert(device.deviceKeyB64)
                admit.append(device)
            }
            guard account.personKeyB64 == ownPersonKeyB64 else { continue }
            for leaf in roster where leaf.personKeyB64 == ownPersonKeyB64 {
                if listed.contains(leaf.leafKeyB64) { continue }
                retire.append(leaf.leafKeyB64)
            }
        }
        return Plan(admit: admit, retire: retire)
    }

    /// ADR-0074 section 2.3 — whether a device of the OWN account may be
    /// added to a conversation: only when its key is a leaf of the account's
    /// DeviceGroup. `nil` (the roster could not be read) admits nothing.
    /// Every "add my own device" path asks this: the re-admission arm, the
    /// pairing fan-out and a new conversation's first commit.
    public static func ownDeviceAdmissible(
        deviceKeyB64: String,
        ownDeviceGroupRoster: Set<String>?
    ) -> Bool {
        ownDeviceGroupRoster?.contains(deviceKeyB64) ?? false
    }
}

/// The per-chat throttle and the removed-device memory for
/// ``DeviceReadmission``.
@MainActor
public enum DeviceReadmissionMemory {

    private static var lastCheckedAtMs: [String: Int64] = [:]

    /// Whether `groupIdHex` is due for a check at `nowMs`; stamps it when so.
    public static func takeSlot(groupIdHex: String, nowMs: Int64) -> Bool {
        if let last = lastCheckedAtMs[groupIdHex], nowMs - last < DeviceReadmission.intervalMs {
            return false
        }
        lastCheckedAtMs[groupIdHex] = nowMs
        return true
    }

    /// Tests and sign-out.
    public static func clearSlots() {
        lastCheckedAtMs.removeAll()
    }

    private static let keyPrefix = "me.lakko.readmission.removed.v1."

    private static func load(
        accountIdB64: String, defaults: UserDefaults
    ) -> [String: [String]] {
        (defaults.dictionary(forKey: keyPrefix + accountIdB64) as? [String: [String]]) ?? [:]
    }

    /// Remember the leaves that left `groupIdHex` between two roster reads. A
    /// torn read on either side records nothing.
    public static func recordRemoved(
        accountIdB64: String,
        groupIdHex: String,
        before: [String]?,
        after: [String]?,
        defaults: UserDefaults = .standard
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        guard let before, let after else { return }
        let now = Set(after)
        let gone = before.filter { !now.contains($0) }
        guard !gone.isEmpty else { return }
        var all = load(accountIdB64: accountIdB64, defaults: defaults)
        var current = Set(all[groupIdHex] ?? [])
        current.formUnion(gone)
        all[groupIdHex] = Array(current).sorted()
        defaults.set(all, forKey: keyPrefix + accountIdB64)
    }

    public static func removed(
        accountIdB64: String,
        groupIdHex: String,
        defaults: UserDefaults = .standard
    ) -> Set<String> {
        Set(load(accountIdB64: accountIdB64, defaults: defaults)[groupIdHex] ?? [])
    }

    /// Forget what left one group this person is no longer in (ADR-0087
    /// section 9): a device added back later is not a device someone removed.
    public static func forgetGroup(
        accountIdB64: String, groupIdHex: String, defaults: UserDefaults = .standard
    ) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var all = load(accountIdB64: accountIdB64, defaults: defaults)
        guard all.removeValue(forKey: groupIdHex) != nil else { return }
        defaults.set(all, forKey: keyPrefix + accountIdB64)
    }

    public static func clearForAccount(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: keyPrefix + accountIdB64)
        clearSlots()
    }
}
