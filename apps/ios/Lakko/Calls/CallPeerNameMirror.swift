// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// The conversation name a background process may show for a call
/// (`docs/ux/calls.md` section 2.1: "Caller identity in the wake, resolved
/// locally").
///
/// ## Why a mirror exists at all
///
/// The wake payload carries no caller — it cannot, invariant #5 — so a name has
/// to come from this device. The app resolves names through
/// ``ProfileDirectory`` + ``ChatRepository/conversationTitle(groupIdHex:)``,
/// and neither is reachable from the notification-service extension: the
/// directory is app-target-only (it writes profiles, which C-59 forbids the NSE
/// from doing) and its store is app-private `UserDefaults`.
///
/// So the app WRITES a minimal `group -> display name` mirror into the App
/// Group suite and the NSE only ever READS it. The same asymmetry, the same
/// suite and the same plaintext posture as the ADR-0042 `push_gid` reverse
/// index (``PushRoutingStore/lookupRoute(pushGidHex:)``), which is already read
/// by the NSE from this suite.
///
/// ## What it costs
///
/// A contact display name sits in the App Group container in plaintext, exactly
/// as the routing reverse index does. It is on-device only, inside the app's
/// own sandbox, never sent anywhere and never part of a backup of the account —
/// and it buys the one thing a ringing phone must have: a name on the screen
/// before anything is open. A group id, which the mirror is keyed on, is
/// already in that container.
///
/// ## What it is not
///
/// Not a source of truth and never consulted by the foreground app, which
/// resolves names properly. A missing or stale row costs the lock screen a
/// name, nothing else: the NSE falls back to the unnamed wording, and the app
/// corrects the system call screen with a call update as soon as it has
/// decrypted the offer.
/// The one name both processes use for the call a notification is about.
///
/// The notification-service extension stamps the call id onto the alert it
/// raises for an incoming call, and the app reads it back to take that alert
/// down once the call has been answered or ended. Two processes, one key, so it
/// is stated once — in the file both targets compile.
public enum LakkoCallNotification {
    /// `userInfo` key carrying the lowercase-hex 16-byte call id.
    public static let callIdUserInfoKey = "lakko_call_id"
    /// `UNNotificationContent.categoryIdentifier` of a call alert, so the app's
    /// notification delegate can tell one from a message.
    public static let callCategoryIdentifier = "LAKKO_CALL"
}

public enum CallPeerNameMirror {

    /// Per-group key prefix in the App Group shared suite.
    private static let keyPrefix = "me.lakko.call-peer.v1."

    private static func key(_ groupIdHex: String) -> String {
        keyPrefix + groupIdHex.lowercased()
    }

    /// The mirrored display name for a conversation, or `nil` when this device
    /// has never mirrored one (a fresh install, an un-provisioned App Group, a
    /// group whose name the app has not resolved yet).
    public static func name(
        groupIdHex: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> String? {
        guard let defaults else { return nil }
        let value = defaults.string(forKey: key(groupIdHex))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value, !value.isEmpty else { return nil }
        return value
    }

    /// Mirror one conversation's display name. App-side only.
    ///
    /// A no-op when the value has not changed, so the common case — the app
    /// re-resolving the same names on every sweep — costs a read and no write.
    /// A `nil` or blank name REMOVES the row rather than storing an empty
    /// string: an empty name on the lock screen would read as a bug, and the
    /// absent row makes the reader fall back to the unnamed wording it already
    /// handles.
    public static func mirror(
        groupIdHex: String,
        name: String?,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults else { return }
        let storageKey = key(groupIdHex)
        let trimmed = name?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !trimmed.isEmpty else {
            if defaults.object(forKey: storageKey) != nil {
                defaults.removeObject(forKey: storageKey)
            }
            return
        }
        if defaults.string(forKey: storageKey) == trimmed { return }
        defaults.set(trimmed, forKey: storageKey)
    }

    /// Drop every mirrored name. Called on logout / forget-device, next to the
    /// other per-device push state: a name left behind would let the next
    /// account's lock screen show the previous one's contact.
    public static func clearAll(defaults: UserDefaults? = SharedStorage.sharedDefaults()) {
        guard let defaults else { return }
        for (storageKey, _) in defaults.dictionaryRepresentation()
        where storageKey.hasPrefix(keyPrefix) {
            defaults.removeObject(forKey: storageKey)
        }
    }
}
