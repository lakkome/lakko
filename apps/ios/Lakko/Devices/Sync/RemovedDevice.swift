// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// "This device was removed" (docs/ux/second-device.md section 3.1).
///
/// Another device of the account removed this one: once this device applies
/// that commit its DeviceGroup is inactive (`DeviceGroup.isActive()` is
/// `false`), frozen for good, and nothing new decrypts. The sync runtime
/// (``DeviceGroupSyncRepository``) then stops polling, persists nothing more,
/// and the app root shows one full-screen state instead of the app
/// (``RemovedDeviceScreen``).
///
/// The decisions live here, apart from the repository, so they can be tested
/// without a Delivery Service. Mirror of
/// `apps/web/src/devices/sync/removed-device.ts`.
enum RemovedDevice {

    /// The copy of the removed-device screen, exactly as the UX spec words it.
    enum Copy {
        static let title = "This device was removed"
        static let body =
            "It was removed from your account on another device. New messages no "
            + "longer reach it. Sign out, then sign in again if you want to use it."
        static let signOut = "Sign out"
    }

    /// What the poll loop does after applying a batch (or after loading a
    /// handle).
    enum PollStep: Equatable {
        /// This device was removed: stop for good this session.
        case stopRemoved
        /// Go on polling.
        case `continue`
    }

    /// Whether the DeviceGroup handle reports this device still in the
    /// account's DeviceGroup; `nil` when the read itself fails (a poisoned
    /// lock, a released handle). A failed read is not proof of removal.
    static func readActive(_ read: () throws -> Bool) -> Bool? {
        try? read()
    }

    /// Only an explicit `false` is a removal: the screen must never show on a
    /// read that failed.
    static func isRemoved(_ active: Bool?) -> Bool {
        active == false
    }

    /// Stop for good once the device is removed, otherwise go on.
    static func pollStepAfterBatch(_ active: Bool?) -> PollStep {
        isRemoved(active) ? .stopRemoved : .continue
    }

    /// Whether the ADR-0057 fork repair may run on this handle. The repair
    /// re-founds the account's DeviceGroup, so it runs only on a group known
    /// to be active: never on a removed device (it is not forked, it is out),
    /// and not on a handle whose state could not be read.
    static func forkRepairAllowed(_ active: Bool?) -> Bool {
        active == true
    }
}
