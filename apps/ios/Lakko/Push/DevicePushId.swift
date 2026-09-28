// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import Security

/// Per-device push pseudonym — 32 random bytes generated on first
/// push registration and persisted in the iOS Keychain for the
/// lifetime of the device.
///
/// Mirror of:
///
///   * `apps/web/src/push/device-id.ts` (1.20b)
///   * `apps/android/.../push/DevicePushId.kt` (1.20d)
///
/// ## Why a fresh random value, not derived from identity
///
/// The push server sees `(device_pseudonym, provider) → token`.
/// `/push/notify` is gone (ADR-0054 decision 1: the DS forwards the wake),
/// and every push-gw route spends an anonymous token. To minimise what the server can link:
///
///   * Deriving from `identity_pubkey` would let the server
///     correlate identity uploads with push registrations the
///     moment a peer publishes a contact request.
///   * Deriving from the device root key requires exporting it
///     from Keychain (defeating the non-exportable posture) just
///     for this derivation.
///   * **Fresh random 32 bytes, persisted in Keychain**: the
///     server sees opaque randomness; clearing Keychain (via
///     ``clear()`` or device-side wipe) rotates the pseudonym.
///
/// The pseudonym is **not** privacy-critical against a passive
/// server adversary — it's opaque randomness. It IS critical that
/// peers who message us know our current pseudonym so they can
/// ask the DS to fan out a wake. That cross-device pseudonym
/// distribution is server-side fan-out plumbing (out of scope for
/// 1.20c).
///
/// ## Keychain layout
///
/// Generic password item:
///
///   * `kSecAttrService = "me.lakko.push"` — same prefix
///     convention as the rest of the iOS app's Keychain entries.
///   * `kSecAttrAccount = "device-id-v1"` — schema-versioned so a
///     future migration can land a v2 store without colliding.
///   * `kSecAttrAccessible = kSecAttrAccessibleAfterFirstUnlock` —
///     APNs registration callbacks fire before the user unlocks
///     the device on cold boot (background push wake); the
///     Keychain entry must be readable in that state.
///
/// `kSecAttrSynchronizable = false` (default) — the pseudonym
/// stays on this device. Multi-device push (iPad alongside iPhone)
/// generates independent pseudonyms; each registers its own token
/// row. The push-gw fan-out (deferred sub-sprint) handles the
/// "wake every device for this account" semantics server-side.
public enum DevicePushId {

    private static let serviceTag = "me.lakko.push"
    private static let accountTag = "device-id-v1"
    private static let pseudonymLength = 32

    /// Return the per-device push pseudonym, generating one and
    /// persisting it on first call. Subsequent calls return the
    /// same bytes for the lifetime of the Keychain entry.
    ///
    /// Throws ``DevicePushIdError/keychain(_:)`` on Keychain
    /// failure — the caller surfaces it but generally the push
    /// orchestrator falls back to "no pseudonym available" and
    /// the rest of the app keeps working.
    public static func getOrCreate() throws -> Data {
        if let existing = try read() {
            return existing
        }
        var fresh = Data(count: pseudonymLength)
        let status = fresh.withUnsafeMutableBytes { ptr -> Int32 in
            guard let base = ptr.baseAddress else { return errSecAllocate }
            return SecRandomCopyBytes(kSecRandomDefault, pseudonymLength, base)
        }
        guard status == errSecSuccess else {
            throw DevicePushIdError.keychain(status)
        }
        try write(fresh)
        return fresh
    }

    /// Read without create — returns `nil` if no entry exists.
    /// Useful for the unregister path: skip the server call when
    /// we have nothing to unregister.
    public static func peek() throws -> Data? {
        return try read()
    }

    /// Drop the persisted pseudonym. Called by
    /// ``AuthRepository/forgetDevice()`` and by the
    /// unsubscribe-from-push orchestrator.
    public static func clear() {
        var query = baseQuery()
        query[kSecAttrAccessible as String] = nil
        SecItemDelete(query as CFDictionary)
    }

    // MARK: - Internals

    private static func read() throws -> Data? {
        var query = baseQuery()
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        query[kSecReturnData as String] = true
        query[kSecAttrAccessible as String] = nil

        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        if status == errSecItemNotFound {
            return nil
        }
        guard status == errSecSuccess, let bytes = item as? Data else {
            throw DevicePushIdError.keychain(status)
        }
        // A corrupted-length entry should not silently confuse
        // every subsequent register call — drop it + force a
        // regen on the next `getOrCreate`.
        if bytes.count != pseudonymLength {
            clear()
            return nil
        }
        return bytes
    }

    private static func write(_ bytes: Data) throws {
        var add = baseQuery()
        add[kSecValueData as String] = bytes
        add[kSecAttrAccessible as String] =
            kSecAttrAccessibleAfterFirstUnlock
        let status = SecItemAdd(add as CFDictionary, nil)
        if status == errSecDuplicateItem {
            // Race: another call won the create. Replace the
            // value so the in-memory caller sees its bytes
            // persisted regardless of who got there first.
            var query = baseQuery()
            query[kSecAttrAccessible as String] = nil
            let update: [String: Any] = [
                kSecValueData as String: bytes,
                kSecAttrAccessible as String:
                    kSecAttrAccessibleAfterFirstUnlock,
            ]
            let upd = SecItemUpdate(
                query as CFDictionary,
                update as CFDictionary,
            )
            if upd != errSecSuccess {
                throw DevicePushIdError.keychain(upd)
            }
            return
        }
        if status != errSecSuccess {
            throw DevicePushIdError.keychain(status)
        }
    }

    private static func baseQuery() -> [String: Any] {
        return [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: serviceTag,
            kSecAttrAccount as String: accountTag,
        ]
    }
}

// MARK: - Errors

public enum DevicePushIdError: Error {
    /// Underlying `Security.framework` status — `OSStatus`.
    /// Treat as a "Keychain unavailable" condition; the rest
    /// of the app keeps working, just without push.
    case keychain(OSStatus)
}

extension DevicePushIdError: LocalizedError {
    public var errorDescription: String? {
        switch self {
        case .keychain(let s):
            return "Keychain error: OSStatus \(s)"
        }
    }
}
