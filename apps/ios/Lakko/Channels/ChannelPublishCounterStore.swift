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
///   * `kSecClassGenericPassword`, one item holding the whole JSON-encoded
///     `[PersistedPublishCounter]` blob keyed per channel (the same single-blob
///     shape ``ChannelPublisherKeyStore`` uses).
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly` — device-bound,
///     no iCloud Keychain sync, background read allowed after first unlock.
///   * Hardware-isolated class key (Secure Enclave + device passcode);
///     user-space cannot read the payload even with root (invariant #7).
public enum ChannelPublishCounterStore {

    /// Service tag — versioned so a future layout change can land beside v1.
    public static let serviceTag: String = "me.lakko.channel-publish-counters.v1"

    /// Single account slot — the blob is global to the install, keyed per
    /// channel inside (matching the ``ChannelPublisherKeyStore`` shape).
    private static let account: String = "publish-counters"

    /// Persist the encoded counter `blob`, replacing any existing item.
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
        // surface; the counter stays in memory for the session and re-persists
        // on the next bump.
        SecItemAdd(addQuery as CFDictionary, nil)
    }

    /// Read the persisted counter blob, or `nil` when absent.
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
