// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed store for the channel **publisher signing seeds** (G-29
/// hardening, invariant #7).
///
/// A channel publisher signs every `ChannelPost` with a per-channel Ed25519
/// key. Its 32-byte seed used to live in memory only (a field on
/// ``ChannelsUiState``), so an app relaunch minted a *fresh* signing key —
/// the publisher could no longer continue its channel under the same key
/// (subscribers verify against the key the earlier posts were signed with).
/// ``ChannelsUiState`` now persists the seed here so the publisher survives a
/// relaunch and keeps signing under the same key.
///
/// The sibling of ``ChannelSubscriberKeyStore`` — same hardware-backed
/// Keychain posture as the device-identity stores (``AccountIdentitySeedStore``
/// / ``ExportKeyStore`` / ``DeviceIdentityStore``). Distinct from the channel
/// **content-key** seed, which is re-derived from the persisted MLS
/// PublisherGroup exporter on every publisher open (not stored here).
///
/// ## Posture
///
///   * `kSecClassGenericPassword`, one item per account holding the whole JSON-encoded
///     `[PersistedPublisherKey]` blob keyed per channel (the same single-blob
///     shape ``ChannelSubscriberKeyStore`` uses).
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, background read allowed after first unlock.
///   * Hardware-isolated class key (Secure Enclave + device passcode);
///     user-space cannot read the payload even with root (invariant #7).
public enum ChannelPublisherKeyStore {

    /// Service tag — versioned so a future layout change can land beside v1.
    public static let serviceTag: String = "me.lakko.channel-publisher-keys.v1"

    /// The item's account slot is `"publisher-signing-seeds|<accountIdB64>"`: one blob per
    /// Lakko ACCOUNT, keyed per channel inside (2026-09-30). The blob used to
    /// be global to the install (slot `"publisher-signing-seeds"`), so a second account signed
    /// in on the same phone inherited the first one's channel keys. That
    /// device-wide item is never read now; ``deleteLegacy()`` removes it.
    static let legacyAccount: String = "publisher-signing-seeds"

    static func itemAccount(_ accountIdB64: String) -> String {
        legacyAccount + "|" + accountIdB64
    }

    /// Persist the encoded signing-seed `blob`, replacing any existing item.
    ///
    /// Not written once the account's session has ended
    /// (``AccountStoreGate``): channel work still in flight then (a poll, an
    /// approve, a rotation) must not put a forgotten account's keys back.
    public static func save(_ blob: Data, accountIdB64: String) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        delete(accountIdB64: accountIdB64)
        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: itemAccount(accountIdB64),
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: blob,
        ]
        // Best-effort: a Keychain write failure must not crash the channels
        // surface; the seed stays in memory for the session and re-persists on
        // the next mutation.
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    /// Read the persisted signing-seed blob, or `nil` when absent.
    public static func load(accountIdB64: String) -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: itemAccount(accountIdB64),
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var out: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &out)
        guard status == errSecSuccess, let data = out as? Data else {
            return nil
        }
        return data
    }

    /// Delete the persisted blob (remove-channel / reset / logout /
    /// `forgetDevice`).
    public static func delete(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: itemAccount(accountIdB64),
        ]
        SecItemDelete(query as CFDictionary)
    }

    /// Delete the device-wide item this store used before it was per account.
    /// Nothing reads it; forget-device removes it.
    public static func deleteLegacy() {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: legacyAccount,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
