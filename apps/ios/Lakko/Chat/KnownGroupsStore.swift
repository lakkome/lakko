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
///
/// ADR-0041 Phase 1 — the list is a non-secret decrypt input the NSE
/// needs, so it moves from `UserDefaults.standard` to the shared
/// `UserDefaults(suiteName:)` suite. Reads prefer the suite and fall
/// back to `.standard`; the one-time migration copies the `.standard`
/// list into the suite when the suite has none (copy-not-move — the
/// `.standard` entry is never deleted by the migration).
public final class KnownGroupsStore {

    /// The shared suite (NSE-readable). `nil` when the App Group is not
    /// provisioned for this process → the store transparently uses
    /// `standardDefaults` exactly as before ADR-0041.
    private let sharedDefaults: UserDefaults?
    /// The old app-private location (`UserDefaults.standard`).
    private let standardDefaults: UserDefaults

    public init(
        sharedDefaults: UserDefaults? = SharedStorage.sharedDefaults(),
        standardDefaults: UserDefaults = .standard
    ) {
        self.sharedDefaults = sharedDefaults
        self.standardDefaults = standardDefaults
    }

    /// Backward-compatible single-store convenience — routes both the
    /// shared and the fallback location to one `UserDefaults` instance.
    /// Used by tests that inject a transient suite for isolation.
    public convenience init(defaults: UserDefaults) {
        self.init(sharedDefaults: defaults, standardDefaults: defaults)
    }

    /// Read the set of known group-id hex strings for
    /// `accountIdB64`. Empty set when the account has none yet
    /// (fresh signup or a cleanly-forgotten device).
    ///
    /// ADR-0041 — prefers the shared suite, falls back to `.standard`.
    public func load(accountIdB64: String) -> Set<String> {
        let key = Self.keyFor(accountIdB64)
        if let shared = sharedDefaults, let array = shared.stringArray(forKey: key) {
            return Set(array)
        }
        if let array = standardDefaults.stringArray(forKey: key) {
            return Set(array)
        }
        return []
    }

    /// Atomically add `groupIdHex` to the set for `accountIdB64`.
    /// No-op when already present. ADR-0041 — writes go to the shared
    /// suite when available, else `.standard`.
    public func add(accountIdB64: String, groupIdHex: String) {
        var current = load(accountIdB64: accountIdB64)
        let (inserted, _) = current.insert(groupIdHex)
        if !inserted { return }
        // `UserDefaults` doesn't ship a native `StringSet` getter
        // like Android's `SharedPreferences.getStringSet`; we
        // serialise as a sorted `[String]` for deterministic
        // ordering on disk (helps reproducible-build verifier
        // when the prefs plist is part of a per-device backup).
        let sorted = Array(current).sorted()
        let key = Self.keyFor(accountIdB64)
        if let shared = sharedDefaults {
            shared.set(sorted, forKey: key)
        } else {
            standardDefaults.set(sorted, forKey: key)
        }
    }

    /// ADR-0045 UX §6.2.4 — drop ONE group from the set.
    ///
    /// The `Delete this chat` path, and the only per-group removal there has
    /// ever been here: everything else about this store is add-or-wipe-all,
    /// because until a membership could END there was no group a device
    /// stopped wanting to rehydrate. Written to **both** suites, unlike
    /// ``add(accountIdB64:groupIdHex:)`` which writes to one — a row left in
    /// `.standard` would be re-read by the fallback the moment the shared
    /// suite is unavailable, resurrecting the deleted chat.
    ///
    /// No-op when absent.
    public func remove(accountIdB64: String, groupIdHex: String) {
        let key = Self.keyFor(accountIdB64)
        for defaults in [sharedDefaults, standardDefaults].compactMap({ $0 }) {
            guard let array = defaults.stringArray(forKey: key) else { continue }
            let remaining = array.filter { $0 != groupIdHex }
            if remaining.count == array.count { continue }
            defaults.set(remaining.sorted(), forKey: key)
        }
    }

    /// Wipe the entire set for `accountIdB64` from **both** the shared
    /// suite and `.standard`. Called from
    /// ``AccountLocalDataWipe`` — same posture as the
    /// ``IdentitySnapshotStore/deleteSnapshot(accountIdB64:)``
    /// wipe (per-account, best-effort, no error surfaced if the
    /// row is already absent).
    public func clear(accountIdB64: String) {
        let key = Self.keyFor(accountIdB64)
        sharedDefaults?.removeObject(forKey: key)
        standardDefaults.removeObject(forKey: key)
    }

    /// ADR-0041 Phase 3 — which locally-stored account knows `groupIdHex`?
    /// Enumerates every `me.lakko.known-groups.v1.<accountIdB64>` row (the
    /// shared suite first, then `.standard`) and returns the first
    /// `accountIdB64` whose set contains the hex, or `nil` when no account
    /// on this device knows the group.
    ///
    /// Used by the silent-push NSE background decrypt
    /// (``NsePushDecrypt``): the extension has no live ``ChatRepository``
    /// to consult, so it resolves the wake's group pseudonym to an account
    /// straight from this store — the direct mirror of Android's
    /// `KnownGroupsStore.findAccountForGroup`. The comparison is
    /// case-insensitive so a wake's `g` in either hex case matches (stored
    /// ids are lowercase, produced by ``DsClient/pseudonymHex(_:)``).
    public func findAccountForGroup(groupIdHex: String) -> String? {
        let needle = groupIdHex.lowercased()
        let prefix = Self.keyPrefix
        for defaults in [sharedDefaults, standardDefaults].compactMap({ $0 }) {
            for (key, _) in defaults.dictionaryRepresentation() where key.hasPrefix(prefix) {
                guard let array = defaults.stringArray(forKey: key) else { continue }
                if array.contains(where: { $0.lowercased() == needle }) {
                    return String(key.dropFirst(prefix.count))
                }
            }
        }
        return nil
    }

    /// ADR-0041 Phase 1 — one-time, idempotent, **copy-not-move**
    /// migration of the known-groups list into the shared suite. No-op
    /// when the suite already has the list (idempotent), `.standard` has
    /// none (nothing to copy), or the suite is unavailable.
    public func migrateToSharedSuite(accountIdB64: String) {
        guard let shared = sharedDefaults else { return }
        let key = Self.keyFor(accountIdB64)
        let suitePresent = shared.stringArray(forKey: key) != nil
        let standardArray = standardDefaults.stringArray(forKey: key)
        guard SharedStorage.shouldCopy(sharedPresent: suitePresent, oldPresent: standardArray != nil),
              let array = standardArray else {
            return
        }
        shared.set(array, forKey: key)
    }

    /// Per-account key prefix — the suffix is the standard-base64
    /// `accountIdB64` shared by every per-account store. Enumerated by
    /// ``findAccountForGroup(groupIdHex:)`` to reverse a group → account.
    private static let keyPrefix: String = "me.lakko.known-groups.v1."

    private static func keyFor(_ accountIdB64: String) -> String {
        keyPrefix + accountIdB64
    }
}
