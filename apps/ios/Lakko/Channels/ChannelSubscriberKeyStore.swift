// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed store for the channel **subscriber keypairs** (G-29
/// hardening, invariant #7).
///
/// The subscriber-side X25519 **secret** half unwraps the per-subscriber HPKE
/// seed envelopes a channel publisher fans out on every rotation — leaking it
/// lets an attacker decrypt every post the device can read on its subscribed
/// channels. It used to live in `UserDefaults` (plaintext at rest);
/// ``ChannelsUiState`` now persists the keypair blob here instead, under the
/// same hardware-backed Keychain posture as the device-identity stores
/// (``AccountIdentitySeedStore`` / ``ExportKeyStore`` / ``DeviceIdentityStore``).
///
/// ## Posture
///
///   * `kSecClassGenericPassword`, one item holding the whole
///     JSON-encoded `[PersistedSubscriberKey]` blob (the existing
///     single-blob shape, not per-account — matches the legacy
///     `UserDefaults` key it replaces).
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, background read allowed after first unlock.
///   * Hardware-isolated class key (Secure Enclave + device passcode);
///     user-space cannot read the payload even with root (invariant #7).
public enum ChannelSubscriberKeyStore {

    /// Service tag — versioned so a future layout change can land beside v1.
    public static let serviceTag: String = "me.lakko.channel-subscriber-keys.v1"

    /// Single account slot — the blob is global to the install (the legacy
    /// `UserDefaults` key was not account-namespaced either).
    private static let account: String = "subscriber-keys"

    /// Persist the encoded keypair `blob`, replacing any existing item.
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
        // surface; the keys stay in memory for the session and re-persist on
        // the next mutation.
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    /// Read the persisted keypair blob, or `nil` when absent.
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

    /// Delete the persisted blob (un-follow-all / logout / `forgetDevice`).
    public static func delete() {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: account,
        ]
        SecItemDelete(query as CFDictionary)
    }
}
