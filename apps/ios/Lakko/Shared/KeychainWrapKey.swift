// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import CryptoKit
import Foundation
import Security

/// ADR-0041 Phase 1 — shared access-group handling for the AES-256
/// **wrap keys** that seal the MLS-snapshot and message-log envelopes
/// (``IdentitySnapshotStore`` / ``MessageLogStore``).
///
/// Both stores keep one 32-byte wrap key per app installation as a
/// generic-password Keychain item, keyed on `(service, account)`. To let
/// the NSE unwrap the App-Group-container envelopes, the wrap key must be
/// readable from the shared Keychain access group
/// (``SharedStorage/keychainAccessGroup``). This helper centralises the
/// access-group-aware read / create / migrate / delete so the two stores
/// share one audited path.
///
/// ## The load-bearing safety property
///
/// The wrap key must **never be regenerated** by the migration — the
/// existing envelopes are sealed under the current key, so a fresh key
/// would make them undecryptable (lost message history / MLS state).
/// Hence:
///
///   * ``read(service:account:)`` prefers the shared group, falls back
///     to the old app-private item — so the **same bytes** are used
///     regardless of which group they live in, and regardless of whether
///     the one-time migration has run yet.
///   * ``loadOrCreate(service:account:)`` mints a fresh key **only** when
///     the key is absent from **both** groups.
///   * ``migrate(service:account:)`` is **copy-not-move** and
///     copy-if-absent — it copies the existing bytes into the shared
///     group only when the shared group has none, and never deletes the
///     old item.
///
/// The prefer-shared / fall-back-old read makes call ordering irrelevant:
/// a write that happens before the migration reads the old key bytes and
/// seals with them; the copy later mirrors those exact bytes into the
/// shared group; every read resolves to the same key.
public enum KeychainWrapKey {

    // MARK: - High-level (prefer-shared / fall-back-old)

    /// Read the wrap key, preferring the shared access group and falling
    /// back to the old app-private item. `nil` when absent from both.
    public static func read(service: String, account: String) -> Data? {
        SharedStorage.effectiveValue(
            shared: read(
                service: service,
                account: account,
                accessGroup: SharedStorage.keychainAccessGroup
            ),
            old: read(service: service, account: account, accessGroup: nil)
        )
    }

    /// Return the existing wrap key (from either group) or mint + persist
    /// a fresh one. A fresh key is created **only** when none exists in
    /// either group — never during migration, so existing envelopes stay
    /// decryptable.
    public static func loadOrCreate(service: String, account: String) -> Data {
        if let existing = read(service: service, account: account) {
            return existing
        }
        // Fresh 32-byte AES-256 key (same generation as the original
        // stores). Only reached when the key is absent from BOTH groups —
        // never during migration — so existing envelopes stay decryptable.
        let fresh = SymmetricKey(size: .bits256).withUnsafeBytes { Data($0) }
        _ = store(service: service, account: account, keyBytes: fresh)
        return fresh
    }

    /// One-time, idempotent, copy-not-move migration of the wrap key into
    /// the shared access group. No-op when the shared group already holds
    /// it (idempotent) or the old item is absent (nothing to copy) or the
    /// shared group is unavailable (copy fails → old item stays
    /// authoritative). The old item is **never deleted** here.
    public static func migrate(service: String, account: String) {
        let sharedPresent = read(
            service: service,
            account: account,
            accessGroup: SharedStorage.keychainAccessGroup
        ) != nil
        let old = read(service: service, account: account, accessGroup: nil)
        guard SharedStorage.shouldCopy(sharedPresent: sharedPresent, oldPresent: old != nil),
              let bytes = old else {
            return
        }
        // Copy the EXISTING bytes into the shared group. On failure the
        // old item remains authoritative (reads fall back to it).
        _ = store(
            service: service,
            account: account,
            keyBytes: bytes,
            accessGroup: SharedStorage.keychainAccessGroup
        )
    }

    /// Store the wrap key, preferring the shared access group and falling
    /// back to the app-private item so the key is never left unpersisted.
    /// Returns `true` on success in either group.
    @discardableResult
    public static func store(service: String, account: String, keyBytes: Data) -> Bool {
        if store(
            service: service,
            account: account,
            keyBytes: keyBytes,
            accessGroup: SharedStorage.keychainAccessGroup
        ) {
            return true
        }
        return store(service: service, account: account, keyBytes: keyBytes, accessGroup: nil)
    }

    /// Wipe the wrap key from **both** groups. Called on `forgetDevice`
    /// (a deliberate, full wipe — distinct from the never-delete-old
    /// migration posture).
    public static func delete(service: String, account: String) {
        delete(service: service, account: account, accessGroup: SharedStorage.keychainAccessGroup)
        delete(service: service, account: account, accessGroup: nil)
    }

    // MARK: - Low-level (single access group)

    private static func read(service: String, account: String, accessGroup: String?) -> Data? {
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess else { return nil }
        return item as? Data
    }

    private static func store(
        service: String,
        account: String,
        keyBytes: Data,
        accessGroup: String?
    ) -> Bool {
        // Delete-then-add upsert, scoped to the target access group so a
        // shared-group store never touches the app-private item and vice
        // versa.
        delete(service: service, account: account, accessGroup: accessGroup)
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: keyBytes
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
        return SecItemAdd(query as CFDictionary, nil) == errSecSuccess
    }

    private static func delete(service: String, account: String, accessGroup: String?) {
        var query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account
        ]
        if let accessGroup { query[kSecAttrAccessGroup] = accessGroup }
        _ = SecItemDelete(query as CFDictionary)
    }
}
