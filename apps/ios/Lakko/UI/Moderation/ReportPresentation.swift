// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Which messages `Add messages` offers (UX `reporting-and-moderation.md`
/// 2.2): for a person or a message, only that person's own messages; for a
/// group or a channel, any message except the reporter's own. Never a system
/// row, a tombstone, a row still sending, or a row of an older segment.
enum ReportCandidates {
    static func from(
        _ messages: [ChatMessage],
        onlyAuthor: MemberIdentity?,
        name: (MemberIdentity?) -> String?
    ) -> [ReportCandidate] {
        Array(messages.compactMap { m -> ReportCandidate? in
            guard m.direction == .incoming, !m.deleted, m.messageId > 0,
                  !LineageRules.isHistoryRowId(m.messageId) else { return nil }
            if let onlyAuthor, m.authorKeyB64 != onlyAuthor { return nil }
            return ReportCandidate(
                messageId: m.messageId, authorName: onlyAuthor == nil ? name(m.authorKeyB64) : nil,
                text: m.text, createdAt: m.createdAt)
        }
        .reversed())
    }
}

/// ADR-0099 / UX 2.1 — a report on a channel from its info: nothing
/// included at first, every post this device holds offered, `Unsubscribe` as
/// the follow-up. The posts come from the channel's local post log (the one
/// the Feed reads), so they are there whether or not the conversation is
/// open; a picked post's evidence is its wire as received, from the log, or
/// from the posts loaded this session for one logged before the wire was
/// kept.
enum ChannelReportSource {
    static func request(
        channelId: Data, title: String, logged: [PersistedChannelPost], loaded: [PostView]
    ) -> ReportRequest {
        var wires: [UInt64: String] = [:]
        for post in loaded { wires[post.cursor] = post.payloadB64 }
        for post in logged { if let w = post.payloadB64 { wires[post.cursor] = w } }
        let candidates = logged.sorted { $0.cursor > $1.cursor }.map {
            ReportCandidate(messageId: Int64(clamping: $0.cursor), authorName: nil, text: $0.text, createdAt: "")
        }
        return ReportRequest(
            target: .channel(channelId: channelId), targetName: title, conversationKind: .channel,
            candidates: candidates, followUp: .unsubscribe,
            candidateEvidence: { messageId in
                guard messageId >= 0, let b64 = wires[UInt64(messageId)],
                      let wire = Data(base64UrlNoPadListing: b64) else { return nil }
                return .channelPost(channelId: channelId, post: wire)
            })
    }
}

/// The production ports of a report sheet: the moderation service through
/// the anti-censorship transport, the pinned root key, the highest-version
/// moderator-list gate, the evidence kept at receipt and the device-only
/// list of sent reports.
@MainActor
enum ReportPorts {
    static func live(
        request: ReportRequest,
        accountIdB64: String?,
        client: ModerationClient = ModerationClient(),
        followUp: @escaping (ReportFollowUp) -> Void
    ) -> ReportSheetModel.Ports {
        let rootKey = ModerationClient.pinnedRootKey
        return ReportSheetModel.Ports(
            moderatorList: {
                // A list that does not verify, or one older than a list this
                // device has seen, is treated as unavailable.
                await ModeratorListGate.admittedList(
                    rootKey: rootKey, fetch: { await client.fetchModeratorList() })
            },
            rootKey: rootKey,
            groupLookupKey: { handle in
                (try? await DiscoveryClient().oprfName(handle))?.lookupKey
            },
            evidence: { messageId in
                if let candidateEvidence = request.candidateEvidence { return candidateEvidence(messageId) }
                guard let accountIdB64, let groupIdHex = request.groupIdHex,
                      let groupId = ChatRepository.hexToBytes(groupIdHex) else { return nil }
                return MessageEvidenceStore.shared.get(
                    accountIdB64: accountIdB64, groupIdHex: groupIdHex, messageId: messageId)?
                    .item(groupId: groupId)
            },
            send: { await client.send(report: $0) },
            record: { receiptId, reason in
                guard let accountIdB64 else { return }
                ReportHistoryStore.shared.add(accountIdB64: accountIdB64, SentReport(
                    receiptId: receiptId, targetName: request.targetName, reason: reason,
                    sentAtMs: Int64(Date().timeIntervalSince1970 * 1000)))
            },
            applyFollowUp: followUp)
    }
}

extension View {
    /// Present the report sheet for `request`; `followUp` applies the
    /// checked `Also ...` action.
    func reportSheet(
        _ request: Binding<ReportRequest?>,
        followUp: @escaping (ReportFollowUp) -> Void = { _ in }
    ) -> some View {
        sheet(item: request) { req in
            ReportSheet(
                model: ReportSheetModel(
                    request: req,
                    ports: ReportPorts.live(
                        request: req, accountIdB64: AcceptedContactsStore.boundAccountIdB64,
                        followUp: followUp)),
                onClose: { sent in
                    request.wrappedValue = nil
                    if sent { ReportNoticeCenter.shared.show(Localization.text("report.sent")) }
                })
        }
    }
}
