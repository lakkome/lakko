// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

/// ADR-0079 section 5 — the person this account added through their invite
/// link, before their app has answered. iOS mirror of
/// `apps/web/src/contacts/pending-contacts.ts`.
///
/// After **Add** on the invite page the inviter is a contact at once on this
/// side (`docs/ux/contact-invite-link.md` section 4): listed in Contacts and
/// Chats under the name the link states, and written to. What is written is
/// kept in the durable held-send queue (``HeldSendQueue``, sealed under the
/// device key) under ``queueKey``; when the inviter's Welcome lands, the
/// messages move to the queue of the new one-to-one, which sends them in
/// order, and the record goes. The record itself (public keys, the stated
/// name, when Add was pressed) is kept per account in `UserDefaults`, behind
/// the account store gate.
public struct PendingContact: Codable, Equatable, Identifiable {
    /// The inviter's 16-byte account id, lower-case hex.
    public let accountIdHex: String
    /// The inviter's account key (Ed25519): what the one-to-one's roster and
    /// the contact list name them by.
    public let signaturePubkey: Data
    public let x25519Pubkey: Data
    /// The display name the link states, or `""`.
    public let displayName: String
    /// The username the link states, or `""`.
    public let username: String
    /// When **Add** was pressed (Unix ms): the 7-day line counts from here.
    public let addedAtMs: Int64

    public var id: String { accountIdHex }

    public init(
        accountIdHex: String, signaturePubkey: Data, x25519Pubkey: Data, displayName: String,
        username: String, addedAtMs: Int64
    ) {
        self.accountIdHex = accountIdHex
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.displayName = displayName
        self.username = username
        self.addedAtMs = addedAtMs
    }

    /// The held-send queue key of what is written before the chat exists:
    /// the inviter's account id hex (32 characters, never a conversation id,
    /// which is a 64-character group id).
    public var queueKey: String { accountIdHex }
}

/// The pure rules of the pending contacts (one definition each, tested).
public enum PendingContacts {
    /// After this long without an answer the chat line changes (UX section 4).
    public static let staleMs: Int64 = 7 * 24 * 60 * 60 * 1000

    /// `list` with `contact` added, or the existing record for the same
    /// person kept with its first Add time (a second Add through a fresh link
    /// does not reset the 7-day line).
    public static func adding(_ contact: PendingContact, to list: [PendingContact]) -> [PendingContact] {
        guard let at = list.firstIndex(where: { $0.accountIdHex == contact.accountIdHex }) else {
            return list + [contact]
        }
        var next = list
        next[at] = PendingContact(
            accountIdHex: contact.accountIdHex, signaturePubkey: contact.signaturePubkey,
            x25519Pubkey: contact.x25519Pubkey, displayName: contact.displayName,
            username: contact.username, addedAtMs: min(list[at].addedAtMs, contact.addedAtMs))
        return next
    }

    /// Which line the pending chat shows.
    public enum Line: Equatable { case waiting, stale }

    public static func line(_ contact: PendingContact, nowMs: Int64) -> Line {
        nowMs - contact.addedAtMs >= staleMs ? .stale : .waiting
    }

    /// The pending contacts the lists show: those with no one-to-one yet.
    public static func shown(_ list: [PendingContact], oneToOnePeerKeys: Set<Data>) -> [PendingContact] {
        list.filter { !oneToOnePeerKeys.contains($0.signaturePubkey) }
    }

    /// The pending contact a joined conversation belongs to: the one whose
    /// account key is a person of a one-to-one. `nil` for a group.
    public static func forConversation(
        _ list: [PendingContact], oneToOne: Bool, personKeys: [Data]
    ) -> PendingContact? {
        guard oneToOne else { return nil }
        let people = Set(personKeys)
        return list.first { people.contains($0.signaturePubkey) }
    }

    /// The name shown: the stated display name, else `@username`, else `nil`
    /// (the caller's catalog fallback).
    public static func name(_ contact: PendingContact) -> String? {
        let trimmed = contact.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        return contact.username.isEmpty ? nil : "@\(contact.username)"
    }
}

/// The pending-contact records, per account, in `UserDefaults` (no message
/// text: that lives in the sealed held-send queue).
public enum PendingContactStore {
    static let keyPrefix = "lakko.contacts.pending-invited.v1:"

    static func key(_ accountIdB64: String) -> String { keyPrefix + accountIdB64 }

    public static func load(accountIdB64: String, defaults: UserDefaults = .standard) -> [PendingContact] {
        guard let data = defaults.data(forKey: key(accountIdB64)),
              let list = try? JSONDecoder().decode([PendingContact].self, from: data)
        else { return [] }
        return list
    }

    /// Not written once the account's session has ended
    /// (``AccountStoreGate``).
    public static func save(_ list: [PendingContact], accountIdB64: String, defaults: UserDefaults = .standard) {
        guard let gate = AccountStoreGate.enter(accountIdB64) else { return }
        defer { gate.leave() }
        if list.isEmpty {
            defaults.removeObject(forKey: key(accountIdB64))
            return
        }
        guard let data = try? JSONEncoder().encode(list) else { return }
        defaults.set(data, forKey: key(accountIdB64))
    }

    /// Forget device: every record of the account.
    public static func clear(accountIdB64: String, defaults: UserDefaults = .standard) {
        defaults.removeObject(forKey: key(accountIdB64))
    }
}

/// ADR-0079 section 4 — the rules over the synced link-record register that
/// the lists need. iOS mirror of `apps/web/src/devices/sync/link-records.ts`;
/// the verdict on a request's link answer is the core's
/// (`SyncedState.linkAnswerVerdict`), never re-derived here.
public enum LinkRecords {
    /// The number of uses a link allows, or `nil` for unlimited (`0` is
    /// single use, `UInt32.max` unlimited, as `lakko-contacts` defines them).
    public static func effectiveLimit(maxUses: UInt32) -> UInt32? {
        if maxUses == 0 { return 1 }
        if maxUses == UInt32.max { return nil }
        return maxUses
    }

    /// Not revoked, not expired, not used up.
    public static func isLive(_ record: LinkRecord, nowMs: UInt64) -> Bool {
        if record.revoked { return false }
        if record.expiryMs != 0 && record.expiryMs <= nowMs { return false }
        if let limit = effectiveLimit(maxUses: record.maxUses), record.uses >= limit { return false }
        return true
    }

    /// The account's contact links the invite-links list shows: live ones
    /// with a ledger row, newest first.
    public static func listedContactLinks(_ records: [LinkRecord], nowMs: UInt64) -> [LinkRecord] {
        records
            .filter { $0.kind == .contact && !$0.ledgerId.isEmpty && isLive($0, nowMs: nowMs) }
            .sorted { $0.mintedAtMs > $1.mintedAtMs }
    }
}
