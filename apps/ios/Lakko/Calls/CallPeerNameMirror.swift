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

/// ## Per account (2026-09-30)
///
/// One row per `(account, group)`: `me.lakko.call-peer.v2.<account>.<group>`.
/// The mirror used to be device-wide and was wiped on every logout; it is now
/// keyed by the account the conversation belongs to, the same way
/// ``AcceptedContactsStore`` keys the contact list, so one account's lock
/// screen can never show another account's contact. Each reader names the
/// account (the push route carries it). No account: no name, no write. Logout
/// keeps the rows; forget-device removes that account's through
/// ``clear(accountIdB64:defaults:)``.
public enum CallPeerNameMirror {

    /// Key prefix in the App Group shared suite; the account and the group
    /// follow.
    private static let keyPrefix = "me.lakko.call-peer.v2."
    /// The device-wide prefix this mirror used before it was per account.
    /// Never read; removed by ``clear(accountIdB64:defaults:)``.
    static let legacyKeyPrefix = "me.lakko.call-peer.v1."

    private static func accountPrefix(_ accountIdB64: String) -> String {
        keyPrefix + accountIdB64 + "."
    }

    private static func key(_ accountIdB64: String, _ groupIdHex: String) -> String {
        accountPrefix(accountIdB64) + groupIdHex.lowercased()
    }

    /// The mirrored display name for one account's conversation, or `nil`
    /// when this device has never mirrored one (a fresh install, an
    /// un-provisioned App Group, a group whose name the app has not resolved
    /// yet, no account).
    public static func name(
        accountIdB64: String?,
        groupIdHex: String,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) -> String? {
        guard let defaults, let accountIdB64 else { return nil }
        let value = defaults.string(forKey: key(accountIdB64, groupIdHex))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
        guard let value, !value.isEmpty else { return nil }
        return value
    }

    /// Mirror one conversation's display name for `accountIdB64`. App-side
    /// only.
    ///
    /// A no-op when the value has not changed, so the common case — the app
    /// re-resolving the same names on every sweep — costs a read and no write.
    /// A `nil` or blank name REMOVES the row rather than storing an empty
    /// string: an empty name on the lock screen would read as a bug, and the
    /// absent row makes the reader fall back to the unnamed wording it already
    /// handles.
    public static func mirror(
        accountIdB64: String,
        groupIdHex: String,
        name: String?,
        defaults: UserDefaults? = SharedStorage.sharedDefaults()
    ) {
        guard let defaults else { return }
        let storageKey = key(accountIdB64, groupIdHex)
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

    /// Forget-device: drop one account's names, and the old device-wide rows
    /// nothing reads any more. Other accounts' names stay.
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
