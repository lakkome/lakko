// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Which conversations a background process must not ring for
/// (`docs/ux/calls.md` section 2.1: "a muted chat never rings").
///
/// ## Why a mirror exists at all
///
/// Mute is a cross-device thread flag, kept by ``DeviceGroupSyncRepository``
/// and persisted inside the account's own encrypted sync blob. Neither is
/// reachable from the notification-service extension: the repository is
/// main-actor app state, and its blob is opened with material C-59 forbids
/// that process from touching.
///
/// So the app WRITES the muted set into the App Group suite and the extension
/// only ever READS it — the same asymmetry, the same suite and the same
/// plaintext posture as ``CallPeerNameMirror`` and the ADR-0042 `push_gid`
/// reverse index (``PushRoutingStore/lookupRoute(accountIdB64:pushGidHex:)``), both of
/// which the extension already reads from this container.
///
/// ## Which way it fails
///
/// **Absent means ring.** A missing suite, a fresh install, an unprovisioned
/// App Group, a group the app has never mirrored: every one of them answers
/// "not muted", so the call rings. The other direction was never available —
/// failing to silence is an unwanted ring, failing to ring is a call the
/// person never finds out about, and a file that is not there yet is a
/// completely ordinary state for both of the processes involved.
///
/// ## What it carries
///
/// One `true` per MUTED conversation, keyed by account and group id, and nothing else. It
/// is not the conversation list: an unmuted conversation has no row, so the
/// container learns nothing about it — not that it exists, not its name, not
/// when it was last active. A group id is already in this container (the
/// routing reverse index is keyed on one), so the mirror adds no identifier
/// that was not there.
///
/// ## Per account (2026-09-30)
///
/// One row per `(account, muted group)`: `me.lakko.conversation-muted.v2.
/// <account>.<group>`. The set used to be device-wide and was wiped on every
/// logout, so a second account signed in on the same phone could never read the
/// first one's mutes; it is now keyed by the account the flag belongs to, the
/// same way ``AcceptedContactsStore`` keys the contact list. Each reader names
/// the account it is ringing for (the push route carries it; the app passes the
/// signed-in account). No account: nothing is muted and nothing is written.
/// Logout keeps the rows for the next sign-in of the same account; forget-device
/// removes that account's rows through ``clear(accountIdB64:defaults:)``.
///
/// ## What it is not
///
/// Not a source of truth. ``DeviceGroupSyncRepository/flags`` is, and it
/// publishes the whole snapshot on every change, which is why
/// ``replaceAll(accountIdB64:mutedGroupIdsHex:defaults:)`` is the write the app
/// makes: an unmute on a sibling device has to REMOVE a row here, and a per-row
/// write would leave the old one behind and go on silencing a conversation the
/// person un-silenced.
public enum ConversationMuteMirror {

    /// Key prefix in the App Group shared suite; the account and the group
    /// follow.
    private static let keyPrefix = "me.lakko.conversation-muted.v2."
    /// The device-wide prefix this mirror used before it was per account.
    /// Never read; removed by ``clear(accountIdB64:defaults:)``.
    static let legacyKeyPrefix = "me.lakko.conversation-muted.v1."

    private static func accountPrefix(_ accountIdB64: String) -> String {
        keyPrefix + accountIdB64 + "."
    }

    private static func key(_ accountIdB64: String, _ groupIdHex: String) -> String {
        accountPrefix(accountIdB64) + groupIdHex.lowercased()
    }

    /// Has this device mirrored `groupIdHex` as muted for `accountIdB64`?
    ///
    /// `false` for everything else, including every failure and a `nil`
    /// account: see "Which way it fails" above.
    public static func isMuted(
        accountIdB64: String?,
        groupIdHex: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> Bool {
        guard let defaults, let accountIdB64 else { return false }
        return defaults.bool(forKey: key(accountIdB64, groupIdHex))
    }

    /// Replace one account's whole mirrored set. App-side only.
    ///
    /// `mutedGroupIdsHex` is every conversation of `accountIdB64` currently
    /// muted; any other row of that account loses it. Other accounts' rows are
    /// untouched. Writes only what changed, so the common case — the app
    /// re-publishing an unchanged flag snapshot — costs a read and no write.
    /// A `nil` account writes nothing.
    public static func replaceAll(
        accountIdB64: String?,
        mutedGroupIdsHex: Set<String>,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults, let accountIdB64 else { return }
        let prefix = accountPrefix(accountIdB64)
        let wanted = Set(mutedGroupIdsHex.map { key(accountIdB64, $0) })
        for (storageKey, _) in defaults.dictionaryRepresentation()
        where storageKey.hasPrefix(prefix) && !wanted.contains(storageKey) {
            defaults.removeObject(forKey: storageKey)
        }
        for storageKey in wanted where defaults.bool(forKey: storageKey) == false {
            defaults.set(true, forKey: storageKey)
        }
    }

    /// Forget-device: drop one account's rows, and the old device-wide rows
    /// nothing reads any more. Other accounts' rows stay.
    public static func clear(
        accountIdB64: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults else { return }
        let prefix = accountPrefix(accountIdB64)
        for (storageKey, _) in defaults.dictionaryRepresentation()
        where storageKey.hasPrefix(prefix) || storageKey.hasPrefix(legacyKeyPrefix) {
            defaults.removeObject(forKey: storageKey)
        }
    }
}
