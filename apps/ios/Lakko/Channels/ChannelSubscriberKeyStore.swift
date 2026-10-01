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
///   * `kSecClassGenericPassword`, one item per account holding the whole
///     JSON-encoded `[PersistedSubscriberKey]` blob of that account.
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, background read allowed after first unlock.
///   * Hardware-isolated class key (Secure Enclave + device passcode);
///     user-space cannot read the payload even with root (invariant #7).
public enum ChannelSubscriberKeyStore {

    /// Service tag — versioned so a future layout change can land beside v1.
    public static let serviceTag: String = "me.lakko.channel-subscriber-keys.v1"

    /// The item's account slot is `"subscriber-keys|<accountIdB64>"`: one blob per
    /// Lakko ACCOUNT, keyed per channel inside (2026-09-30). The blob used to
    /// be global to the install (slot `"subscriber-keys"`), so a second account signed
    /// in on the same phone inherited the first one's channel keys. That
    /// device-wide item is never read now; ``deleteLegacy()`` removes it.
    static let legacyAccount: String = "subscriber-keys"

    static func itemAccount(_ accountIdB64: String) -> String {
        legacyAccount + "|" + accountIdB64
    }

    /// Persist the encoded keypair `blob`, replacing any existing item.
    public static func save(_ blob: Data, accountIdB64: String) {
        delete(accountIdB64: accountIdB64)
        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: itemAccount(accountIdB64),
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: blob,
        ]
        // Best-effort: a Keychain write failure must not crash the channels
        // surface; the keys stay in memory for the session and re-persist on
        // the next mutation.
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    /// Read the persisted keypair blob, or `nil` when absent.
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

    /// Delete the persisted blob (un-follow-all / logout / `forgetDevice`).
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
