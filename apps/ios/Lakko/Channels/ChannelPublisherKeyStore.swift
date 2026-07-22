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
///   * `kSecClassGenericPassword`, one item holding the whole JSON-encoded
///     `[PersistedPublisherKey]` blob keyed per channel (the same single-blob
///     shape ``ChannelSubscriberKeyStore`` uses).
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, background read allowed after first unlock.
///   * Hardware-isolated class key (Secure Enclave + device passcode);
///     user-space cannot read the payload even with root (invariant #7).
public enum ChannelPublisherKeyStore {

    /// Service tag — versioned so a future layout change can land beside v1.
    public static let serviceTag: String = "me.lakko.channel-publisher-keys.v1"

    /// Single account slot — the blob is global to the install, keyed per
    /// channel inside (matching the ``ChannelSubscriberKeyStore`` shape).
    private static let account: String = "publisher-signing-seeds"

    /// Persist the encoded signing-seed `blob`, replacing any existing item.
    public static func save(_ blob: Data) {
        delete()
        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: account,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: blob,
        ]
        // Best-effort: a Keychain write failure must not crash the channels
        // surface; the seed stays in memory for the session and re-persists on
        // the next mutation.
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    /// Read the persisted signing-seed blob, or `nil` when absent.
    public static func load() -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: account,
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
    public static func delete() {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: account,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
