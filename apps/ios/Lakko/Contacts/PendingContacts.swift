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
    /// Whether a person is among those added through their invite link who
    /// have not answered yet. The request went out when Add was pressed, so a
    /// search row reads "Request sent" for them and offers no second request.
    /// By account id too: their key may have rotated since.
    public static func holds(_ invited: [PendingContact], accountKey: Data, accountId: Data?) -> Bool {
        let idHex = accountId.map { groupIdHex($0) }
        return invited.contains { $0.signaturePubkey == accountKey || $0.accountIdHex == idHex }
    }

    public static func name(_ contact: PendingContact) -> String? {
        let trimmed = contact.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty { return trimmed }
        return contact.username.isEmpty ? nil : "@\(contact.username)"
    }

    /// ADR-0098 — the name drawn for a pending contact: ``name(_:)``, or
    /// `Deleted account` once this device learned their account is gone.
    @MainActor
    public static func shownName(_ contact: PendingContact) -> String? {
        if isDeleted(contact) { return ResolvedDisplayName.deletedName }
        return name(contact)
    }

    /// ADR-0098 — this device learned the pending contact's account is gone.
    @MainActor
    public static func isDeleted(_ contact: PendingContact) -> Bool {
        ProfileDirectory.shared.isDeletedAccount(signatureKey: contact.signaturePubkey)
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

/// ADR-0079 section 5, pairing state bundle v3 — the messages another device
/// of this account wrote to a pending contact, as DISPLAY-ONLY copies on this
/// one.
///
/// They are shown as own rows in the waiting state (in the pending chat, then
/// in the one-to-one once it lands), and nothing else: they are never put in
/// this device's own held-send queue and never sent from here, because the
/// device they were typed on sends them, and two senders would deliver each
/// one twice. Each copy goes when the real message arrives from the other
/// device (``matchIndex(_:text:attachmentCount:)``), or
/// ``maxAgeMs`` after it was written.
///
/// Kept per account and per key (the pending contact's ``PendingContact/queueKey``,
/// then the conversation hex once the one-to-one lands) in the
/// ``HeldSendQueue`` file format under its own directory, so the held-send
/// flush never reads them: message plaintext, sealed under the device key,
/// behind the account store gate, wiped with the account.
public struct PendingDisplayCopies {
    /// A copy still unmatched this long after it was written is dropped.
    public static let maxAgeMs: Int64 = 7 * 24 * 60 * 60 * 1000
    static let directoryName = "PendingDisplayCopies"

    private let queue: HeldSendQueue

    public init(
        ownerAccountIdB64: String,
        sealer: HeldSendQueue.Sealer = PendingDisplayCopies.deviceKeySealer,
        baseDirectory: URL? = nil
    ) {
        queue = HeldSendQueue(
            ownerAccountId: ownerAccountIdB64, sealer: sealer,
            baseDirectory: baseDirectory ?? Self.defaultBaseDirectory())
    }

    /// The device-key wrap the message log and the held-send queue use.
    public static let deviceKeySealer = HeldSendQueue.Sealer(
        seal: { try MessageLogStore.sealUnderWrapKey($0) },
        open: { try MessageLogStore.openUnderWrapKey($0) })

    static func defaultBaseDirectory() -> URL? {
        HeldSendQueue.defaultBaseDirectory()?.appendingPathComponent(directoryName, isDirectory: true)
    }

    /// Keep `messages` as copies under `key`, in order. Idempotent: a copy is
    /// named by its position and time, so a second bundle with the same
    /// messages adds nothing. Returns how many are held under `key` now.
    @discardableResult
    public func store(_ messages: [StateBundle.CarriedHeldMessage], key: String) -> Int {
        for (index, message) in messages.enumerated() {
            let attachments = message.attachments.compactMap { try? JSONEncoder().encode($0) }
            queue.enqueue(
                conversationHex: key,
                entry: HeldSendQueue.Entry(
                    localId: "copy-\(message.heldAtMs)-\(index)",
                    queuedAtMs: message.heldAtMs,
                    text: message.text,
                    attachments: attachments))
        }
        return queue.count(conversationHex: key)
    }

    /// The copies held under `key`, in order, without those past
    /// ``maxAgeMs`` (which are removed).
    public func list(key: String, nowMs: Int64 = Int64(Date().timeIntervalSince1970 * 1000)) -> [HeldSendQueue.Entry] {
        let all = queue.list(conversationHex: key)
        let live = Self.live(all, nowMs: nowMs)
        if live.count != all.count {
            for entry in all where !live.contains(entry) {
                queue.remove(conversationHex: key, localId: entry.localId)
            }
        }
        return live
    }

    /// Every key with copies, unfiltered by age.
    public func all() -> [(key: String, entries: [HeldSendQueue.Entry])] {
        queue.all().map { (key: $0.conversationHex, entries: $0.entries) }
    }

    /// Move every copy from `from` to `to`, keeping the order (the pending
    /// contact's one-to-one landed). `false` when a write failed; the copies
    /// not moved stay where they were.
    @discardableResult
    public func move(from: String, to: String) -> Bool {
        let entries = queue.list(conversationHex: from)
        guard !entries.isEmpty else { return true }
        let there = Set(queue.list(conversationHex: to).map(\.localId))
        for entry in entries where !there.contains(entry.localId) {
            guard queue.enqueue(conversationHex: to, entry: entry) else { return false }
        }
        return queue.clear(conversationHex: from)
    }

    /// The real message `text` with `attachmentCount` attachments arrived
    /// from this account's other device: remove the first copy it matches.
    /// Returns whether one was removed.
    @discardableResult
    public func consumeMatch(key: String, text: String, attachmentCount: Int) -> Bool {
        let copies = queue.list(conversationHex: key)
        guard let index = Self.matchIndex(copies, text: text, attachmentCount: attachmentCount) else {
            return false
        }
        return queue.remove(conversationHex: key, localId: copies[index].localId)
    }

    @discardableResult
    public func clear(key: String) -> Bool {
        queue.clear(conversationHex: key)
    }

    /// Forget device: every copy of the account.
    @discardableResult
    public static func clearAccount(ownerAccountIdB64: String, baseDirectory: URL? = nil) -> Bool {
        HeldSendQueue.clearAccount(
            ownerAccountId: ownerAccountIdB64,
            baseDirectory: baseDirectory ?? defaultBaseDirectory())
    }

    /// The copy a real message replaces: the FIRST, in order, with exactly
    /// the same text and the same number of attachments. `nil` when none.
    public static func matchIndex(_ copies: [HeldSendQueue.Entry], text: String, attachmentCount: Int) -> Int? {
        copies.firstIndex { $0.text == text && $0.attachments.count == attachmentCount }
    }

    /// `copies` without those written ``maxAgeMs`` or more before `nowMs`.
    public static func live(_ copies: [HeldSendQueue.Entry], nowMs: Int64) -> [HeldSendQueue.Entry] {
        copies.filter { nowMs - $0.queuedAtMs < maxAgeMs }
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
