// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import LakkoCore

// ADR-0101 section 9 — the history comes back after a key reset, newest
// first: the I/O around ``HistoryReturnRules``. The requester rides the
// ADR-0091 gap look (one per group, its secret sealed under the device key,
// its signer rule `gapHistorySignerAccepted`); the answerer is handed each
// request at receipt (``ChatClient/historyRequestHandler``). iOS mirror of
// `apps/web/src/chat/ConversationRejoinRuntime.tsx` (history half) and
// `buildHistoryPage` (`apps/web/src/devices/history-bundle.ts`).

extension HistoryReturnRules {
    /// The `route` a history look is stored under, so it is told apart from
    /// an ADR-0091 re-entry look of the same group (which it never replaces).
    static let lookRoute = "history-return"
}

extension ChatRepository {
    /// Install the receipt hook (once per bootstrap).
    func installHistoryReturn() {
        ChatClient.historyRequestHandler = { [weak self] groupIdHex, senderKey, request in
            Task { @MainActor [weak self] in
                await self?.answerHistoryRequest(groupIdHex: groupIdHex, senderKeyB64: senderKey, request: request)
            }
        }
    }

    // MARK: - The requester

    /// At each poll of a returned conversation: post the current history
    /// request when due, with a fresh gap key whose look reads the answer.
    func requestHistoryIfDue(groupIdHex: String) async {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let store = HistoryReturnStore()
        let now = Self.nowMs()
        guard let state = store.read(accountIdB64: accountIdB64)[groupIdB64],
              HistoryReturnRules.requestDue(state, nowMs: now) else { return }
        // A re-entry look of this group is in flight: its answer comes first.
        if let held = reentryGapLooks.load(accountIdB64: accountIdB64, groupIdHex: groupIdHex),
           held.route != HistoryReturnRules.lookRoute { return }
        let gapKey = generateGapKey()
        guard reentryGapLooks.save(
            ReentryGapLook(
                groupIdHex: groupIdHex,
                keys: [ReentryGapKeyPair(publicKey: gapKey.publicKey, secret: gapKey.secret)],
                gapFromMs: 0, gapToMs: UInt64(max(0, now)), route: HistoryReturnRules.lookRoute),
            accountIdB64: accountIdB64) else { return }
        do {
            let envelope = try composeHistoryRequestEnvelope(
                gapKeyPub: gapKey.publicKey, beforeMessageId: state.before)
            try await publishApplicationMessage(groupIdHex: groupIdHex, plaintextBytes: envelope)
            store.write(HistoryReturnRules.asked(state, nowMs: now), groupIdB64: groupIdB64, accountIdB64: accountIdB64)
        } catch {
            NSLog("ADR-0101: a history request was not posted: \(ErrorLogTag.of(error))")
        }
    }

    /// A page filed by the history look: move `before` to its oldest id and
    /// ask again at once, or stop.
    func historyPageFiled(groupIdHex: String, messageIds: [Int64]) {
        guard let accountIdB64 = currentAccountIdB64, let record = groups[groupIdHex] else { return }
        let groupIdB64 = base64Encode(record.groupId)
        let store = HistoryReturnStore()
        guard let state = store.read(accountIdB64: accountIdB64)[groupIdB64] else { return }
        let next = HistoryReturnRules.nextStep(state, filedMessageIds: messageIds)
        store.write(next, groupIdB64: groupIdB64, accountIdB64: accountIdB64)
        guard !next.done else { return }
        Task { @MainActor [weak self] in await self?.requestHistoryIfDue(groupIdHex: groupIdHex) }
    }

    /// The account keys this account held before its current one (D2): the
    /// key its reset retired and the predecessors of a rotation.
    func formerOwnAccountKeys(accountIdB64: String) -> Set<Data> {
        var keys = Set(OwnAccountKeys.predecessorPublicKeys(accountIdB64: accountIdB64))
        if let previous = ResetContinuityStore().read(accountIdB64: accountIdB64)?.previousKeyB64
            .flatMap({ base64Decode($0) }), previous.count == 32 {
            keys.insert(previous)
        }
        if let current = ReachCenter.shared.ownAccountKey { keys.remove(current) }
        return keys
    }

    /// The DS ids a structured bundle's rows carry (8-byte big-endian).
    nonisolated static func historyMessageIds(_ bundle: StructuredHistoryBundle) -> [Int64] {
        bundle.groups.flatMap(\.messages).compactMap { m in
            guard m.messageId.count == 8 else { return nil }
            return m.messageId.reduce(Int64(0)) { ($0 << 8) | Int64($1) }
        }
    }

    // MARK: - The answerer

    /// One history request arrived (receipt). Answered only when
    /// ``HistoryReturnRules/answerAllowed(fromSelf:requesterResetAccepted:requesterWasInEarlierGroup:earlierGroupHeld:isOneToOne:ownIsOwnerOrAdmin:)``
    /// says so: a page of the conversation's earlier group, sealed to the
    /// request's gap key for the successor, posted to its rendezvous. An
    /// EMPTY page is posted too: it ends the requester.
    func answerHistoryRequest(groupIdHex: String, senderKeyB64: String, request: FfiHistoryRequest) async {
        guard let accountIdB64 = currentAccountIdB64, let identity = canonicalIdentity,
              let successor = groups[groupIdHex], let facts = continuityFacts(group: successor.group) else { return }
        let own = AuthorIdentity.own(group: successor.group)
        // ADR-0101 sections 10-11 (2026-11-28) — only a requester whose
        // SEALED reset is in force is answered: no history to an account
        // after an unsealed reset.
        let pin = readmissionPins().allPins().first {
            $0.value.pinnedSignaturePubkeyB64 == senderKeyB64 && PeerPinStore.sealedResetInForce($0.value) != nil
        }?.value
        let retired = Set(pin.map(PeerPinStore.actionableSupersededKeys) ?? [])
        let earlier = olderSegments(headGroupIdHex: headGroupIdHex(of: groupIdHex)).filter { $0 != groupIdHex }
        let source = earlier.reversed().first { hex in
            guard let held = groups[hex], let earlierFacts = continuityFacts(group: held.group) else { return false }
            return !retired.isDisjoint(with: earlierFacts.rosterPersonKeysB64)
        }
        let ownIsAdmin = own.map { facts.ownerKeyB64 == $0 || facts.adminKeysB64.contains($0) } ?? false
        guard HistoryReturnRules.answerAllowed(
            fromSelf: senderKeyB64 == own, requesterResetAccepted: pin != nil,
            requesterWasInEarlierGroup: source != nil, earlierGroupHeld: source.flatMap { groups[$0] } != nil,
            isOneToOne: facts.kind == .chat, ownIsOwnerOrAdmin: ownIsAdmin),
              let sourceHex = source,
              let rendezvous = try? gapHistoryRendezvous(gapKey: request.gapKeyPub) else { return }
        persistMessageLogBestEffort()
        let held = (messageLogStore.getLog(accountIdB64: accountIdB64)?.groups[sourceHex]?.messages ?? [])
            .filter { !HistoryBundleCodec.isBookkeepingSystemRow($0) }
        // D3 (live walk 2026-11-26) — this account's messages sent into the
        // old one-to-one after the reset are sent again into the successor
        // with the re-send marker (ADR-0073 R.7 case 1): they are not in the
        // history page as well.
        let rows = HistoryReturnRules.withoutResent(
            held, isOneToOne: facts.kind == .chat, resetSignedAtMs: pin?.resetSignedAtMs)
        let page = HistoryReturnRules.pageRows(
            rows, messageId: { $0.messageId }, before: request.beforeMessageId, pageSize: HistoryReturnRules.pageSize)
        do {
            let sealed = try HistoryBundleCodec.buildHistoryPage(
                sourceHex: sourceHex, rows: page, accountIdB64: accountIdB64, readmitter: identity,
                gapKey: request.gapKeyPub, targetGroupId: successor.groupId,
                ownKey: own.flatMap { Data(base64Encoded: $0) })
            try await WelcomeCourier.publishHistoryBundle(
                rendezvousPseudonym: rendezvous, sealedBytes: sealed, ds: dsClient)
            NSLog("ADR-0101: a history page was sent: rows=\(page.count)")
            // D4 — the empty page ends the requester: the history is served,
            // and a founder holding the ownership back may hand it over.
            if page.isEmpty {
                HistoryServedStore().markServed(
                    successorGroupIdB64: base64Encode(successor.groupId), accountIdB64: accountIdB64,
                    nowMs: Self.nowMs())
            }
        } catch {
            NSLog("ADR-0101: a history page was not sent: \(ErrorLogTag.of(error))")
        }
    }
}
