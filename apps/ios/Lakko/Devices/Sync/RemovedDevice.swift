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

    /// The copy of the removed-device screen, rendered in the interface
    /// language on every read (web `REMOVED_DEVICE_COPY`).
    ///
    /// UX 18.6: the two bodies have their own keys, each naming the account
    /// (`{name}`). The progress word and the button reuse the reset screen's
    /// (`devices.signed_out.removing` / `.sign_in`). `devices.removed.sign_out`
    /// stays for the keys-mismatch screen, the one state that still waits
    /// for a tap.
    enum Copy {
        static let bodyRemovingKey = "devices.removed.body_removing"
        static let bodyDoneKey = "devices.removed.body_done"

        static var title: String { Localization.text("devices.removed.title") }
        static var signOut: String { Localization.text("devices.removed.sign_out") }
        /// `account` is `@name`; a guest account reads "this account".
        static func bodyRemoving(account: String) -> String {
            Localization.text(bodyRemovingKey, texts: ["name": name(account)])
        }
        static func bodyDone(account: String) -> String {
            Localization.text(bodyDoneKey, texts: ["name": name(account)])
        }
        private static func name(_ account: String) -> String {
            account.isEmpty ? Localization.text("devices.signed_out.this_account") : account
        }
        static var removing: String { Localization.text("devices.signed_out.removing") }
        static var signIn: String { Localization.text("devices.signed_out.sign_in") }
    }

    /// Which variant of the full-screen state a sign-out shows.
    static func variant(cause: DeviceSignOutCause, state: KeysSignOutState) -> RemovedDeviceScreen.Variant {
        switch cause {
        case .keysReset: return .keysReset(state)
        case .deviceRemoved: return .removed(state)
        }
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

    /// Whether a device that is behind may drop its DeviceGroup state and
    /// rejoin through a sibling (ADR-0081 decision 4): only on a group known
    /// to be active — never on a removed device (it is not behind, it is
    /// out), and not on a handle whose state could not be read.
    static func rejoinAllowed(_ active: Bool?) -> Bool {
        active == true
    }
}
