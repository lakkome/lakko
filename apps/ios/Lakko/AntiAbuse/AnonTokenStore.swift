// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore
import Security

/// Where ``AnonTokenPool`` keeps its tokens between launches (ADR-0054 S5).
///
/// A token is valid in its issuance epoch and the next, so a pool that
/// survives a relaunch does not draw a fresh batch every time the app starts.
/// The pool calls these under its own lock; an implementation must not call
/// back into the pool.
public protocol AnonTokenStore: AnyObject, Sendable {
    /// Every token stored for `accountKey`, by context. Unreadable rows are
    /// skipped (the pool refills).
    func load(accountKey: String) -> [AnonTokenContext: [AnonToken]]
    /// Replace what is stored for `(accountKey, context)` with `tokens`; an
    /// empty list removes the row.
    func save(accountKey: String, context: AnonTokenContext, tokens: [AnonToken])
    /// Delete every token stored for `accountKey` (Forget device, Delete
    /// account). Other accounts' tokens stay.
    func delete(accountKey: String)
}

/// The production store: one Keychain item per (account, context).
///
///   * `kSecAttrService = "me.lakko.anon-tokens.v1"`
///   * `kSecAttrAccount = "<accountKey>|<context>"` (`|` never occurs in the
///     base64 account key)
///   * `kSecValueData = JSON [{ "context", "epoch", "header" }]`
///   * `kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly`: bound to this
///     device, never synced through iCloud Keychain and never restored to
///     another device from a backup. App-private: not in the shared access
///     group the notification extension reads, which never spends a token.
///
/// Nothing here logs a token value; a Keychain failure logs its status only.
public final class KeychainAnonTokenStore: AnonTokenStore, @unchecked Sendable {

    /// Service tag, versioned so a future shape lands beside v1.
    public static let serviceTag = "me.lakko.anon-tokens.v1"

    private let service: String

    /// - Parameter service: the Keychain service; tests use their own.
    public init(service: String = KeychainAnonTokenStore.serviceTag) {
        self.service = service
    }

    public func load(accountKey: String) -> [AnonTokenContext: [AnonToken]] {
        var out: [AnonTokenContext: [AnonToken]] = [:]
        for context in AnonTokenContext.allCases {
            let query: [CFString: Any] = [
                kSecClass: kSecClassGenericPassword,
                kSecAttrService: service,
                kSecAttrAccount: Self.itemAccount(accountKey, context),
                kSecReturnData: true,
                kSecMatchLimit: kSecMatchLimitOne,
            ]
            var item: CFTypeRef?
            let status = SecItemCopyMatching(query as CFDictionary, &item)
            guard status == errSecSuccess, let data = item as? Data else {
                if status != errSecItemNotFound {
                    NSLog("[anon-token] store read %@ failed: OSStatus %d", context.rawValue, status)
                }
                continue
            }
            guard let rows = try? JSONDecoder().decode([StoredToken].self, from: data) else {
                NSLog("[anon-token] store row %@ undecodable, dropped", context.rawValue)
                deleteItem(Self.itemAccount(accountKey, context))
                continue
            }
            let tokens = rows
                .filter { $0.context == context.rawValue }
                .map { AnonToken(context: $0.context, epoch: $0.epoch, headerValue: $0.header) }
            if !tokens.isEmpty { out[context] = tokens }
        }
        return out
    }

    public func save(accountKey: String, context: AnonTokenContext, tokens: [AnonToken]) {
        let account = Self.itemAccount(accountKey, context)
        guard !tokens.isEmpty else {
            deleteItem(account)
            return
        }
        let rows = tokens.map { StoredToken(context: $0.context, epoch: $0.epoch, header: $0.headerValue) }
        guard let data = try? JSONEncoder().encode(rows) else { return }
        let match: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ]
        let update: [CFString: Any] = [kSecValueData: data]
        var status = SecItemUpdate(match as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = match
            add[kSecAttrAccessible] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            add[kSecValueData] = data
            status = SecItemAdd(add as CFDictionary, nil)
        }
        if status != errSecSuccess {
            NSLog("[anon-token] store write %@ failed: OSStatus %d", context.rawValue, status)
        }
    }

    public func delete(accountKey: String) {
        for context in AnonTokenContext.allCases {
            deleteItem(Self.itemAccount(accountKey, context))
        }
        // Rows of a context this build no longer knows.
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecReturnAttributes: true,
            kSecMatchLimit: kSecMatchLimitAll,
        ]
        var items: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &items) == errSecSuccess,
              let rows = items as? [[String: Any]]
        else { return }
        let prefix = accountKey + "|"
        for row in rows {
            guard let account = row[kSecAttrAccount as String] as? String else { continue }
            if account.hasPrefix(prefix) { deleteItem(account) }
        }
    }

    private func deleteItem(_ account: String) {
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ]
        _ = SecItemDelete(query as CFDictionary)
    }

    static func itemAccount(_ accountKey: String, _ context: AnonTokenContext) -> String {
        "\(accountKey)|\(context.rawValue)"
    }

    private struct StoredToken: Codable {
        let context: String
        let epoch: UInt64
        let header: String
    }
}
