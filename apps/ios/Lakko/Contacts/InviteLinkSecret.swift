// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// ADR-0028 amendment 2026-09-28 — every invite link (`/i/`, `/c/`, `/g/`)
/// carries a 12-byte secret after `#`, and the capability is encrypted under
/// it. The discovery ledger holds only the sealed blob; the fragment is never
/// sent to a server (a browser does not send it, and this app never uploads
/// it).
///
///   * pointer: `https://<host>/<k>/<8-char id>#<16-char secret>`
///   * inline:  `https://<host>/<k>/<b64url sealed>#<16-char secret>`, only
///     when the upload failed on transport
///
/// A link whose fragment is missing or is not exactly 16 base64url characters
/// decoding to 12 bytes is DAMAGED, on every kind, and so is a blob that does
/// not open under it (`FfiError.InviteLinkDamaged`).
public enum InviteLinkSecret {
    /// 12 bytes (`inviteLinkSecretLen()`).
    public static var secretLen: Int { Int(inviteLinkSecretLen()) }
    /// 16 base64url characters (`inviteLinkSecretUrlLen()`).
    public static var urlLen: Int { Int(inviteLinkSecretUrlLen()) }

    /// `base#<secret>`. Any fragment `base` already carried is replaced.
    public static func link(base: String, secret: Data) -> String {
        split(base).base + "#" + base64UrlEncodeNoPad(secret)
    }

    /// The part before `#` and the fragment after it (`nil` when there is
    /// none or it is empty). The slug parsers only ever see `base`.
    public static func split(_ link: String) -> (base: String, fragment: String?) {
        guard let hash = link.firstIndex(of: "#") else { return (link, nil) }
        let fragment = String(link[link.index(after: hash)...])
        return (String(link[..<hash]), fragment.isEmpty ? nil : fragment)
    }

    /// The secret a fragment carries, or `nil` when the link is damaged
    /// (absent, wrong length, not base64url, wrong decoded length).
    public static func read(_ fragment: String?) -> Data? {
        guard let fragment,
              fragment.count == urlLen,
              isBase64Url(fragment),
              let bytes = base64UrlDecodeNoPad(fragment),
              bytes.count == secretLen else { return nil }
        return bytes
    }

    /// A parked link token: the slug with its fragment kept after `#`, so a
    /// deep link can travel through the intake and the routes as one string.
    public static func token(slug: String, fragment: String?) -> String {
        guard let fragment, !fragment.isEmpty else { return slug }
        return slug + "#" + fragment
    }

    /// Whether `error` is the one refusal an open reports for a damaged link.
    public static func isDamaged(_ error: Error) -> Bool {
        if case FfiError.InviteLinkDamaged = error { return true }
        return false
    }
}

/// The copy of a damaged link, shared by `/i/`, `/c/` and `/g/`
/// (`docs/ux/group-invite-page.md`, `docs/ux/call-links.md`).
public var inviteLinkDamagedMessage: String { Localization.text("invite.link.damaged") }

/// Where ``InviteLinkSecretStore`` keeps one account's slug -> secret map.
public protocol InviteLinkSecretBackend: AnyObject {
    func load(accountIdB64: String) -> Data?
    func save(accountIdB64: String, blob: Data)
    func delete(accountIdB64: String)
}

/// The production backend: one Keychain generic-password item per account
/// (`kSecAttrService = me.lakko.invite-link-secrets.v2`, `kSecAttrAccount =
/// accountIdB64`), `AfterFirstUnlockThisDeviceOnly`, never synced — the same
/// per-account secret posture as ``BackupFloorStore`` and ``PhraseStore``.
public final class KeychainInviteLinkSecretBackend: InviteLinkSecretBackend {
    public static let defaultServiceTag = "me.lakko.invite-link-secrets.v2"

    private let serviceTag: String

    /// - Parameter serviceTag: the Keychain service the blobs live under
    ///   (``OwnLinkStore`` keeps its records under its own).
    public init(serviceTag: String = KeychainInviteLinkSecretBackend.defaultServiceTag) {
        self.serviceTag = serviceTag
    }

    public func load(accountIdB64: String) -> Data? {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess else { return nil }
        return item as? Data
    }

    public func save(accountIdB64: String, blob: Data) {
        delete(accountIdB64: accountIdB64)
        let add: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            kSecValueData: blob,
        ]
        let status = SecItemAdd(add as CFDictionary, nil)
        if status != errSecSuccess {
            NSLog("invite link secrets: Keychain write failed (%d)", Int(status))
        }
    }

    public func delete(accountIdB64: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: serviceTag,
            kSecAttrAccount: accountIdB64,
        ]
        SecItemDelete(query as CFDictionary)
    }
}

/// An in-memory backend (tests).
public final class InMemoryInviteLinkSecretBackend: InviteLinkSecretBackend {
    private var blobs: [String: Data] = [:]

    public init() {}

    public func load(accountIdB64: String) -> Data? { blobs[accountIdB64] }
    public func save(accountIdB64: String, blob: Data) { blobs[accountIdB64] = blob }
    public func delete(accountIdB64: String) { blobs[accountIdB64] = nil }
}

/// The link secrets of the `/i/` links this device minted, per account, by
/// ledger slug (`docs/ux/client-redesign.md` section 5.3).
///
/// The server lists an owner's live invites but never saw their secrets, so a
/// listed link can be copied again only on the device that made it; on any
/// other device the row offers Revoke alone. The secret is a per-account
/// secret, so it lives in the Keychain (``KeychainInviteLinkSecretBackend``)
/// and survives a restart; revoking a link deletes its secret, and forget
/// device deletes the account's whole map. Never synced, never logged.
public final class InviteLinkSecretStore {
    public static let shared = InviteLinkSecretStore()

    private let backend: InviteLinkSecretBackend

    public init(backend: InviteLinkSecretBackend = KeychainInviteLinkSecretBackend()) {
        self.backend = backend
    }

    private func map(accountIdB64: String) -> [String: String] {
        guard let blob = backend.load(accountIdB64: accountIdB64),
              let decoded = try? JSONDecoder().decode([String: String].self, from: blob)
        else { return [:] }
        return decoded
    }

    private func write(accountIdB64: String, _ map: [String: String]) {
        if map.isEmpty {
            backend.delete(accountIdB64: accountIdB64)
            return
        }
        guard let blob = try? JSONEncoder().encode(map) else { return }
        backend.save(accountIdB64: accountIdB64, blob: blob)
    }

    /// Not written once the account's session has ended
    /// (``AccountStoreGate``): the secret arrives after the ledger upload,
    /// a wait the session may not survive.
    public func put(accountIdB64: String, slug: String, secret: Data) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = map(accountIdB64: accountIdB64)
        current[slug] = base64UrlEncodeNoPad(secret)
        write(accountIdB64: accountIdB64, current)
    }

    public func secret(accountIdB64: String, slug: String) -> Data? {
        InviteLinkSecret.read(map(accountIdB64: accountIdB64)[slug])
    }

    /// Revoke: the link is gone, so is its secret.
    public func remove(accountIdB64: String, slug: String) {
        // A read-modify-write: after a wipe it would write the rest back.
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        var current = map(accountIdB64: accountIdB64)
        guard current.removeValue(forKey: slug) != nil else { return }
        write(accountIdB64: accountIdB64, current)
    }

    /// Forget device: every secret of the account.
    public func clear(accountIdB64: String) {
        backend.delete(accountIdB64: accountIdB64)
    }
}
