// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation

/// ADR-0079 section 5 — the messages written to a person added through their
/// invite link before their app answered.
///
/// They are kept in the durable held-send queue (sealed under the device key,
/// ``HeldSendQueue``) under the pending contact's ``PendingContact/queueKey``.
/// The held-send flush never sends them from there: the key is not a
/// conversation this device holds. When the inviter's Welcome lands, they
/// move to the queue of the new one-to-one, in order, and the ordinary flush
/// sends them.
extension ChatRepository {

    private var pendingInvitedQueue: HeldSendQueue? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return HeldSendQueue(ownerAccountId: accountIdB64, sealer: heldSendSealer)
    }

    /// Keep one message written to a pending contact. `false` when it could
    /// not be kept durably (the caller puts the words back).
    public func queuePendingInvitedMessage(queueKey: String, text: String) -> Bool {
        guard !text.isEmpty, let queue = pendingInvitedQueue else { return false }
        let entry = HeldSendQueue.Entry(
            localId: UUID().uuidString, queuedAtMs: Int64(Date().timeIntervalSince1970 * 1000),
            text: text)
        return queue.enqueue(conversationHex: queueKey, entry: entry)
    }

    /// The messages kept for a pending contact, in the order written.
    public func pendingInvitedMessages(queueKey: String) -> [HeldSendQueue.Entry] {
        pendingInvitedQueue?.list(conversationHex: queueKey) ?? []
    }

    /// Drop the messages of a pending contact (the person deleted the
    /// contact, or the account is wiped), the display copies included.
    @discardableResult
    public func clearPendingInvitedMessages(queueKey: String) -> Bool {
        if let copies = pendingDisplayCopies {
            copies.clear(key: queueKey)
            invalidateDisplayCopies()
        }
        return pendingInvitedQueue?.clear(conversationHex: queueKey) ?? false
    }

    // MARK: - Display copies (pairing state bundle v3)

    var pendingDisplayCopies: PendingDisplayCopies? {
        guard let accountIdB64 = currentAccountIdB64 else { return nil }
        return PendingDisplayCopies(ownerAccountIdB64: accountIdB64, sealer: heldSendSealer)
    }

    /// The display-only copies of what another device of this account wrote
    /// to the pending contact `queueKey`, in order (expired ones dropped).
    public func pendingInvitedDisplayCopies(queueKey: String) -> [HeldSendQueue.Entry] {
        PendingDisplayCopies.live(displayCopies()[queueKey] ?? [], nowMs: Self.nowMs())
    }

    /// Every key's copies, read once and kept until ``invalidateDisplayCopies()``.
    func displayCopies() -> [String: [HeldSendQueue.Entry]] {
        if let cached = displayCopyCache { return cached }
        guard let copies = pendingDisplayCopies else { return [:] }
        var next: [String: [HeldSendQueue.Entry]] = [:]
        let now = Self.nowMs()
        for (key, entries) in copies.all() {
            // Reading through `list` drops the expired ones from disk too.
            let live = PendingDisplayCopies.live(entries, nowMs: now).count == entries.count
                ? entries : copies.list(key: key, nowMs: now)
            if !live.isEmpty { next[key] = live }
        }
        displayCopyCache = next
        return next
    }

    /// The copies changed on disk: read them again and re-draw the lists.
    func invalidateDisplayCopies() {
        displayCopyCache = nil
        state = state.with(threads: buildThreadList())
    }

    /// The rows a conversation draws for its display copies: own outgoing
    /// rows in the waiting state, with local ids far below every other row's
    /// (history rows sit at `id - n * 2^40`). Attachments of another client
    /// are not drawn (only their number is kept, to match the real message).
    func displayCopyRows(conversationHex: String, ownKey: MemberIdentity?) -> [ChatMessage] {
        let copies = PendingDisplayCopies.live(displayCopies()[conversationHex] ?? [], nowMs: Self.nowMs())
        return copies.enumerated().map { index, entry in
            ChatMessage(
                messageId: 0, direction: .outgoing, text: entry.text, attachments: [],
                createdAt: Self.isoTimestamp(ms: entry.queuedAtMs), authorKeyB64: ownKey,
                ack: .pending, localId: Int64.min + 1 + Int64(index))
        }
    }

    /// A batch of a group arrived: each message from this account's OTHER
    /// device (a sender key that is this account's, which the echo filter
    /// keeps from being this device's own) replaces the first display copy
    /// with the same text and the same number of attachments.
    func matchDisplayCopies(_ events: [IncomingChatEvent], groupIdHex: String, ownAuthorKey: MemberIdentity?) {
        guard let ownAuthorKey else { return }
        let key = conversationIdHex(ofGroup: groupIdHex)
        guard displayCopies()[key]?.isEmpty == false, let copies = pendingDisplayCopies else { return }
        var changed = false
        for event in events {
            guard case .application(_, let text, let attachments, let senderKey, _, _, _) = event,
                  senderKey == ownAuthorKey else { continue }
            if copies.consumeMatch(key: key, text: text, attachmentCount: attachments.count) { changed = true }
        }
        if changed { invalidateDisplayCopies() }
    }

    /// The pending contact's one-to-one landed: its display copies move to
    /// the conversation, and the messages of this account's other devices
    /// already in it replace their copies (they arrived before the move).
    func handOverDisplayCopies(queueKey: String, toGroupIdHex groupIdHex: String) {
        guard let copies = pendingDisplayCopies,
              displayCopies()[queueKey]?.isEmpty == false else { return }
        let conversationHex = conversationIdHex(ofGroup: groupIdHex)
        guard copies.move(from: queueKey, to: conversationHex) else {
            invalidateDisplayCopies()
            return
        }
        let head = groups[headGroupIdHex(of: conversationHex)]
        let own = head.flatMap { AuthorIdentity.own(group: $0.group) }
        for message in head?.messages ?? []
        where message.messageId > 0 && message.direction == .outgoing && own != nil && message.authorKeyB64 == own {
            copies.consumeMatch(
                key: conversationHex, text: message.text, attachmentCount: message.attachments.count)
        }
        invalidateDisplayCopies()
    }

    /// Whether a group is a one-to-one, and the account keys of its people:
    /// what a joined conversation is matched to a pending contact by.
    public func pendingInvitedFacts(groupIdHex: String) -> (oneToOne: Bool, personKeys: [Data])? {
        guard let record = groups[groupIdHex], let facts = continuityFacts(group: record.group) else {
            return nil
        }
        return (facts.kind == .chat, facts.rosterPersonKeysB64.compactMap { base64Decode($0) })
    }

    /// Move a pending contact's messages into the held-send queue of the
    /// one-to-one just joined, in order, then offer the flush. `true` when
    /// every message is kept under the conversation (the caller then drops
    /// the pending record); `false` leaves the pending queue as it was.
    public func handOverPendingInvitedMessages(queueKey: String, toGroupIdHex groupIdHex: String) -> Bool {
        guard let queue = pendingInvitedQueue else { return false }
        // Display only, best effort: never what decides the hand-over.
        handOverDisplayCopies(queueKey: queueKey, toGroupIdHex: groupIdHex)
        let entries = queue.list(conversationHex: queueKey)
        let conversationHex = conversationIdHex(ofGroup: groupIdHex)
        // Their give-up clock starts now: they could not be sent before the
        // conversation existed (the web and Android hand-over do the same).
        let nowMs = Int64(Date().timeIntervalSince1970 * 1000)
        for entry in entries {
            // Already moved by an earlier pass that stopped half way.
            if queue.list(conversationHex: conversationHex).contains(where: { $0.localId == entry.localId }) {
                continue
            }
            guard queue.enqueue(conversationHex: conversationHex, entry: entry.requeued(atMs: nowMs)) else {
                return false
            }
        }
        _ = queue.clear(conversationHex: queueKey)
        restoreHeldRows()
        continueAllSoon(force: true)
        Task { @MainActor [weak self] in await self?.flushHeldSends() }
        return true
    }
}
