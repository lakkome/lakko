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
    /// contact, or the account is wiped).
    @discardableResult
    public func clearPendingInvitedMessages(queueKey: String) -> Bool {
        pendingInvitedQueue?.clear(conversationHex: queueKey) ?? false
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
