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
/// reverse index (``PushRoutingStore/lookupRoute(pushGidHex:)``), both of
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
/// One `true` per MUTED conversation, keyed by group id, and nothing else. It
/// is not the conversation list: an unmuted conversation has no row, so the
/// container learns nothing about it — not that it exists, not its name, not
/// when it was last active. A group id is already in this container (the
/// routing reverse index is keyed on one), so the mirror adds no identifier
/// that was not there.
///
/// ## What it is not
///
/// Not a source of truth. ``DeviceGroupSyncRepository/flags`` is, and it
/// publishes the whole snapshot on every change, which is why
/// ``replaceAll(mutedGroupIdsHex:defaults:)`` is the write the app makes: an
/// unmute on a sibling device has to REMOVE a row here, and a per-row write
/// would leave the old one behind and go on silencing a conversation the
/// person un-silenced.
public enum ConversationMuteMirror {

    /// Per-group key prefix in the App Group shared suite.
    private static let keyPrefix = "me.lakko.conversation-muted.v1."

    private static func key(_ groupIdHex: String) -> String {
        keyPrefix + groupIdHex.lowercased()
    }

    /// Has this device mirrored `groupIdHex` as muted?
    ///
    /// `false` for everything else, including every failure: see "Which way it
    /// fails" above.
    public static func isMuted(
        groupIdHex: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> Bool {
        guard let defaults else { return false }
        return defaults.bool(forKey: key(groupIdHex))
    }

    /// Replace the whole mirrored set. App-side only.
    ///
    /// `mutedGroupIdsHex` is every conversation currently muted; anything else
    /// with a row loses it. Writes only what changed, so the common case — the
    /// app re-publishing an unchanged flag snapshot — costs a read and no
    /// write.
    public static func replaceAll(
        mutedGroupIdsHex: Set<String>,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults else { return }
        let wanted = Set(mutedGroupIdsHex.map { key($0) })
        for (storageKey, _) in defaults.dictionaryRepresentation()
        where storageKey.hasPrefix(keyPrefix) && !wanted.contains(storageKey) {
            defaults.removeObject(forKey: storageKey)
        }
        for storageKey in wanted where defaults.bool(forKey: storageKey) == false {
            defaults.set(true, forKey: storageKey)
        }
    }

    /// Drop every mirrored row. Called on logout / forget-device, beside the
    /// other per-device push state: a row left behind would silence the next
    /// account's calls in a conversation it has never heard of.
    public static func clearAll(defaults: UserDefaults? = SharedStorage.sharedDefaults()) {
        guard let defaults else { return }
        for (storageKey, _) in defaults.dictionaryRepresentation()
        where storageKey.hasPrefix(keyPrefix) {
            defaults.removeObject(forKey: storageKey)
        }
    }
}
