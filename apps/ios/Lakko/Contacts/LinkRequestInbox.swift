// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0079 (reworked 2026-10-17) — the requests of the account mailbox that
/// answer one of this account's invite links and are NOT DECIDED yet: the
/// synced link records are not loaded, or this device's claim is in progress
/// or won but not yet built. Kept on the device, never shown, until
/// ``LinkClaims/step(own:answeredElsewhere:verdict:)`` decides them; a
/// relaunch finds them here (the mailbox cursor has moved past their rows).
///
/// A row carries the link answer, whose secret is SECRET (it opens the
/// link), so the list is sealed under the device's Keychain wrap key (the
/// sealer the message log uses) before it reaches `UserDefaults`
/// (`me.lakko.contacts.link-requests.v1.<account>`), behind the account store
/// gate. A body that does not open reads as none held.
public final class LinkRequestInbox {

    /// One undecided request.
    public struct Row: Codable, Equatable {
        public let request: IncomingContactRequest
        /// The link's 16-byte id.
        public let linkId: Data
        /// The link's 12-byte secret, as the requester opened it. SECRET.
        public let linkSecret: Data
        /// The requester's reply grant, encoded (`encodeReachGrant`): what
        /// opens their mailbox for the Welcome. Kept in the row, sealed like
        /// the link secret, because a grant handed to ``ReachCenter`` before
        /// it is active (a poll at a cold start) is dropped, and every later
        /// pass then had no mailbox for the requester (seen on a Pixel
        /// 2026-11-10; this client stored the row the same way).
        public let replyGrantWire: Data?

        public init(request: IncomingContactRequest, linkId: Data, linkSecret: Data, replyGrantWire: Data? = nil) {
            self.request = request
            self.linkId = linkId
            self.linkSecret = linkSecret
            self.replyGrantWire = replyGrantWire
        }

        /// The reply grant, or `nil` when none was kept or it does not decode.
        public var replyGrant: FfiReachGrant? {
            replyGrantWire.flatMap { try? decodeReachGrant(bytes: $0) }
        }
    }

    /// How many undecided requests are kept; past it a new one is not kept
    /// (its sender's app is told nothing, as for a dropped request). Chosen,
    /// not measured.
    public static let maxRows = 64

    static let prefix = "me.lakko.contacts.link-requests.v1."

    private let accountIdB64: String
    private let defaults: UserDefaults
    private let seal: (Data) throws -> Data
    private let open: (Data) throws -> Data

    public init(
        accountIdB64: String,
        defaults: UserDefaults = .standard,
        seal: @escaping (Data) throws -> Data = { try MessageLogStore.sealUnderWrapKey($0) },
        open: @escaping (Data) throws -> Data = { try MessageLogStore.openUnderWrapKey($0) }
    ) {
        self.accountIdB64 = accountIdB64
        self.defaults = defaults
        self.seal = seal
        self.open = open
    }

    static func storageKey(_ accountIdB64: String) -> String { prefix + accountIdB64 }

    /// The undecided requests, in arrival order.
    public func rows() -> [Row] {
        guard let sealed = defaults.data(forKey: Self.storageKey(accountIdB64)),
              let plain = try? open(sealed),
              let rows = try? JSONDecoder().decode([Row].self, from: plain) else { return [] }
        return rows
    }

    @discardableResult
    private func write(_ rows: [Row]) -> Bool {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return false }
        defer { gate.leave() }
        if rows.isEmpty {
            defaults.removeObject(forKey: Self.storageKey(accountIdB64))
            return true
        }
        guard let plain = try? JSONEncoder().encode(rows), let sealed = try? seal(plain) else { return false }
        defaults.set(sealed, forKey: Self.storageKey(accountIdB64))
        return true
    }

    /// Keep one more undecided request (once per `messageId`). `false` when
    /// it could not be kept (the bound, or a failed write).
    @discardableResult
    public func add(_ row: Row) -> Bool {
        var all = rows()
        if all.contains(where: { $0.request.messageId == row.request.messageId }) { return true }
        guard all.count < Self.maxRows else { return false }
        all.append(row)
        return write(all)
    }

    /// The request is decided (answered, shown, or built): it goes.
    public func remove(messageId: Int64) {
        let all = rows()
        let kept = all.filter { $0.request.messageId != messageId }
        if kept.count != all.count { write(kept) }
    }

    /// Forget device: drop the account's row.
    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: storageKey(accountIdB64))
    }
}
