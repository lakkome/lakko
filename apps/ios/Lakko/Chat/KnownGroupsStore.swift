// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// Per-account `Set<String>` of known group-id hex strings.
///
/// Mirror of:
///
///   * `apps/web/src/chat/mls-store.ts`'s `knownGroupIds` IDB index
///     (1.17f5)
///   * `apps/android/.../chat/KnownGroupsStore.kt` (1.15f5)
///
/// Persists across process death so
/// ``ChatRepository/bootstrap(secrets:)`` can iterate every
/// previously-joined group + call `loadGroup` (UniFFI) to rebuild
/// each `Group` handle eagerly. Without this list the snapshot
/// restore would lay down OpenMLS provider storage but leave no
/// in-memory `Group` references — peer commits or application
/// messages to those groups would silently miss
/// ``ChatRepository/pollGroup(groupIdHex:waitSecs:)``'s lookup.
///
/// `UserDefaults` is the right tool — atomic upsert under the
/// hood, fast on warm reads, no SQLCipher ceremony for what is
/// essentially a tiny per-account string set. The Keychain-resident
/// material lives in ``DeviceKeyStore``; the group-id list isn't
/// secret on its own (it's just BLAKE3 output of MLS group state
/// the user is already privy to).
///
/// Per-account namespacing via the `"known_groups.<accountIdB64>"`
/// key shape — multiple accounts on the same device don't collide.
public final class KnownGroupsStore {

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Read the set of known group-id hex strings for
    /// `accountIdB64`. Empty set when the account has none yet
    /// (fresh signup or a cleanly-forgotten device).
    public func load(accountIdB64: String) -> Set<String> {
        guard let array = defaults.stringArray(forKey: Self.keyFor(accountIdB64)) else {
            return []
        }
        return Set(array)
    }

    /// Atomically add `groupIdHex` to the set for `accountIdB64`.
    /// No-op when already present.
    public func add(accountIdB64: String, groupIdHex: String) {
        var current = load(accountIdB64: accountIdB64)
        let (inserted, _) = current.insert(groupIdHex)
        if !inserted { return }
        // `UserDefaults` doesn't ship a native `StringSet` getter
        // like Android's `SharedPreferences.getStringSet`; we
        // serialise as a sorted `[String]` for deterministic
        // ordering on disk (helps reproducible-build verifier
        // when the prefs plist is part of a per-device backup).
        defaults.set(
            Array(current).sorted(),
            forKey: Self.keyFor(accountIdB64)
        )
    }

    /// Wipe the entire set for `accountIdB64`. Called from
    /// ``ChatRepository/forgetDevice()`` — same posture as the
    /// ``IdentitySnapshotStore/deleteSnapshot(accountIdB64:)``
    /// wipe (per-account, best-effort, no error surfaced if the
    /// row is already absent).
    public func clear(accountIdB64: String) {
        defaults.removeObject(forKey: Self.keyFor(accountIdB64))
    }

    private static func keyFor(_ accountIdB64: String) -> String {
        "me.lakko.known-groups.v1.\(accountIdB64)"
    }
}
