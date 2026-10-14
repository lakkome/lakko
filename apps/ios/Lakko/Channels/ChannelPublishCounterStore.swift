// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Keychain-backed store for the channel **publish counters** (AEAD
/// nonce-reuse defence).
///
/// A channel publisher seals every `ChannelPost` under an AEAD key + nonce
/// that `lakko-channels` HKDF-derives from
/// `(seed, channel_id, epoch_id, sender_pubkey, message_counter)` and requires
/// the per-post `message_counter` to be strictly monotone within
/// `(channel_id, epoch_id, sender_pubkey)` — with the counter living in client
/// storage BETWEEN SESSIONS (`crates/lakko-channels/src/post.rs`). The channel
/// content seed is STABLE across an app relaunch (it re-derives from the
/// persisted MLS PublisherGroup exporter, see ``ChannelPublisherKeyStore``), so
/// an in-memory counter that restarts at 0 after a relaunch re-issues the same
/// `(key, nonce)` on different plaintexts under the same epoch — ChaCha20
/// keystream reuse (catastrophic). Persisting the counter at rest keeps it
/// forever-monotone across relaunches, closing the reuse.
///
/// The counter is NOT secret — it rides in the clear post header on the wire —
/// but it is persisted alongside the publisher signing seed in the same
/// hardware-backed Keychain for a single, consistent per-channel publisher
/// store. `epoch_id` is folded into the HKDF, so this counter is
/// forever-monotone per channel and is NEVER reset on a rotation (epoch change).
///
/// The sibling of ``ChannelPublisherKeyStore`` / ``ChannelSubscriberKeyStore``
/// — same hardware-backed Keychain posture as the device-identity stores
/// (``AccountIdentitySeedStore`` / ``ExportKeyStore`` / ``DeviceIdentityStore``).
///
/// ## Posture
///
///   * `kSecClassGenericPassword`, one item per account holding the whole JSON-encoded
///     `[PersistedPublishCounter]` blob keyed per channel (the same single-blob
///     shape ``ChannelPublisherKeyStore`` uses).
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, background read allowed after first unlock.
///   * Hardware-isolated class key (Secure Enclave + device passcode);
///     user-space cannot read the payload even with root (invariant #7).
public enum ChannelPublishCounterStore {

    /// Service tag — versioned so a future layout change can land beside v1.
    public static let serviceTag: String = "me.lakko.channel-publish-counters.v1"

    /// The item's account slot is `"publish-counters|<accountIdB64>"`: one blob per
    /// Lakko ACCOUNT, keyed per channel inside (2026-09-30). The blob used to
    /// be global to the install (slot `"publish-counters"`), so a second account signed
    /// in on the same phone inherited the first one's channel keys. That
    /// device-wide item is never read now; ``deleteLegacy()`` removes it.
    static let legacyAccount: String = "publish-counters"

    static func itemAccount(_ accountIdB64: String) -> String {
        legacyAccount + "|" + accountIdB64
    }

    /// Persist the encoded counter `blob`, replacing any existing item.
    ///
    /// Throws ``AccountStoreGate/Sealed`` when the account's session has
    /// ended (nothing is written), and ``ChannelPublishCounterStoreError``
    /// on a Keychain failure. The publish path stops on either
    /// (``ChannelsUiState/nextCounter(channelId:)``): a post whose counter
    /// is not on disk could be issued again after a relaunch.
    ///
    /// The item is updated in place, and added only when absent: deleting it
    /// first left a window where a failed add lost the floor altogether, and
    /// a relaunch would then start the counter at 0 again.
    public static func save(_ blob: Data, accountIdB64: String) throws {
        let gate = try AccountStoreGate.enterOrEnd(accountIdB64)
        defer { gate.leave() }
        let match: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: itemAccount(accountIdB64),
        ]
        let updated = SecItemUpdate(match as CFDictionary, [kSecValueData: blob] as CFDictionary)
        if updated == errSecSuccess { return }
        guard updated == errSecItemNotFound else {
            throw ChannelPublishCounterStoreError.keychain(status: updated)
        }
        let addQuery: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: itemAccount(accountIdB64),
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: blob,
        ]
        let added = SecItemAdd(addQuery as CFDictionary, nil)
        guard added == errSecSuccess else {
            throw ChannelPublishCounterStoreError.keychain(status: added)
        }
    }

    /// Read the persisted counter blob, or `nil` when absent.
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

/// A publish counter the Keychain did not take.
public enum ChannelPublishCounterStoreError: Error, Equatable {
    case keychain(status: OSStatus)
}
