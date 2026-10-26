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
    /// The 16-byte id of the invite link the request answered: a link
    /// refusal must name it (ADR-0079 amendment of 2026-10-19).
    public let linkId: Data

    public var id: String { accountIdHex }

    public init(
        accountIdHex: String, signaturePubkey: Data, x25519Pubkey: Data, displayName: String,
        username: String, addedAtMs: Int64, linkId: Data
    ) {
        self.accountIdHex = accountIdHex
        self.signaturePubkey = signaturePubkey
        self.x25519Pubkey = x25519Pubkey
        self.displayName = displayName
        self.username = username
        self.addedAtMs = addedAtMs
        self.linkId = linkId
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
            username: contact.username, addedAtMs: min(list[at].addedAtMs, contact.addedAtMs),
            linkId: contact.linkId)
        return next
    }

    /// ADR-0079 amendment of 2026-10-19 — the pending contact a link refusal
    /// cancels: the one whose stated account key signed it over the link
    /// that contact's request answered and this account's key (`verify` is
    /// the core's `verifyLinkRefusal`). `nil` when none does: a refusal
    /// nobody but the inviter could sign, for another link or another
    /// person, changes nothing. Mirror of web `pendingContactRefusedBy`.
    public static func refusedBy(
        _ list: [PendingContact], refusal: Data, ownAccountKey: Data,
        verify: (Data, Data, Data, Data) -> Bool
    ) -> PendingContact? {
        list.first { verify(refusal, $0.signaturePubkey, $0.linkId, ownAccountKey) }
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
    static let keyPrefix = "lakko.contacts.pending-invited.v2:"

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

    /// The contact link record whose ledger row is `slug` (the `/i/<slug>`
    /// segment), or `nil` when the register holds none.
    public static func contactRecord(slug: String, in records: [LinkRecord]) -> LinkRecord? {
        guard let ledgerId = base64UrlDecodeNoPad(slug) else { return nil }
        return records.first { $0.kind == .contact && $0.ledgerId == ledgerId }
    }

    /// Whether a row offers **Edit link** (`docs/ux/contact-invite-link.md`
    /// section 9): only when the synced record says so (the core's one rule,
    /// `LinkRecordEntry::editable`: not revoked, never copied, never
    /// answered). No record (the register not loaded yet) is not editable.
    public static func editable(_ record: LinkRecord?) -> Bool {
        record?.editable ?? false
    }

    /// Whether **Delete link** asks first: a link that was copied or used
    /// might be held by someone. Only a link the register knows was never
    /// copied and never used is deleted at once; no record asks.
    public static func deleteNeedsConfirmation(_ record: LinkRecord?) -> Bool {
        guard let record else { return true }
        return record.shared || record.uses > 0 || !record.answered.isEmpty
    }
}

/// The order of **Add** on a contact invite page (ADR-0079 section 5, amended
/// 2026-10-19; `docs/ux/contact-invite-link.md` section 3), with the page's
/// steps passed in so the order is unit-tested. The web
/// (`contacts/invite-add.ts`) and Android (`InviteAddOrder`) keep the same
/// order:
///
///  1. the ledger's consume: a link used up or gone since the page opened is
///     dead, and NOTHING is kept (no pending contact, no held message);
///  2. only then the pending contact is written, before the request is sent
///     so an answer that comes back at once finds it;
///  3. the request; a send that fails removes the pending contact again and
///     the failure propagates.
@MainActor
public enum InviteAddOrder {
    /// How Add ended.
    public enum Outcome: Equatable { case added, dead }

    /// - Parameters:
    ///   - consumeLive: `false` when the ledger answered used up or gone.
    ///   - recordPending: writes the pending contact; a throw stops Add with
    ///     nothing to forget.
    public static func run(
        consumeLive: () async throws -> Bool,
        recordPending: () throws -> Void,
        sendRequest: () async throws -> Void,
        forgetPending: () -> Void
    ) async throws -> Outcome {
        guard try await consumeLive() else { return .dead }
        try recordPending()
        do {
            try await sendRequest()
        } catch {
            forgetPending()
            throw error
        }
        return .added
    }
}

/// **Open chat** on the page of a person already in the visitor's contacts
/// (`docs/ux/contact-invite-link.md` section 3, amended 2026-10-24): the link
/// still carries its answer, so an inviter who removed the visitor gets them
/// back. The ledger consume first; a dead link sends nothing; no pending
/// contact is written, since the inviter already is one. A failed send
/// propagates so the page can say so. Mirror of web `answerAsKnownContact`.
@MainActor
public enum KnownContactAnswerOrder {
    /// How Open chat's answer ended.
    public enum Outcome: Equatable { case sent, dead }

    /// - Parameter consumeLive: `false` when the ledger answered used up or
    ///   gone.
    public static func run(
        consumeLive: () async throws -> Bool,
        sendRequest: () async throws -> Void
    ) async throws -> Outcome {
        guard try await consumeLive() else { return .dead }
        try await sendRequest()
        return .sent
    }
}

/// **Add** on a contact invite page when the visitor removed the inviter from
/// their contacts and the one-to-one remains (`docs/ux/contact-invite-link.md`
/// section 3, "B removed A, the chat remains"; 2026-10-24). The inviter's
/// device answers such a request by restoring, never by founding, so no
/// pending contact is written: it would wait for an answer that is never
/// sent. The ledger consume first (a dead link keeps nothing and sends
/// nothing); then the request; only once it is sent the inviter is put back
/// on the list with that one-to-one. A send failure leaves nothing changed.
@MainActor
public enum InviteRestoreOrder {
    /// How Add ended.
    public enum Outcome: Equatable { case restored, dead }

    public static func run(
        consumeLive: () async throws -> Bool,
        sendRequest: () async throws -> Void,
        restore: () throws -> Void
    ) async throws -> Outcome {
        guard try await consumeLive() else { return .dead }
        try await sendRequest()
        try restore()
        return .restored
    }
}

/// What the invite page shows once the link opened
/// (`docs/ux/contact-invite-link.md` section 3): the visitor's own link wins
/// over everything; an established contact over a pending one (2026-10-24:
/// the visitor already pressed Add on one of the inviter's links and waits
/// for the answer, so the inviter already is their contact and nothing is
/// sent again); otherwise the default page with Add. Pure. Mirror of web
/// `invitePageView`.
public enum InvitePageDecision: Equatable {
    case own
    case established
    case pending
    case open

    public static func decide(isOwn: Bool, established: Bool, pending: Bool) -> InvitePageDecision {
        if isOwn { return .own }
        if established { return .established }
        if pending { return .pending }
        return .open
    }
}
