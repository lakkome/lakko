// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Settings > Privacy > Blocked people (UX `reporting-and-moderation.md`
/// 1.1): the account's blocked people, most recently blocked first, with
/// `Unblock` on each row.
struct BlockedPeopleScreen: View {
    @ObservedObject private var blocks = BlockList.shared
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        Group {
            if blocks.entries.isEmpty {
                Text(l10n.t("block.list.empty"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color.lakkoBackground)
            } else {
                List(blocks.entries) { entry in
                    ResolvedName(signatureKey: entry.accountKey) { resolved in
                        HStack(spacing: 12) {
                            InitialsAvatar(
                                name: resolved.isPlaceholder ? "" : resolved.name,
                                identity: entry.id, size: 40, placeholderIcon: .personFill,
                                photo: .person(entry.accountKey))
                            Text(resolved.name)
                                .foregroundStyle(Color.lakkoText)
                                .lineLimit(1)
                            Spacer()
                            Button(l10n.t("block.unblock")) { BlockList.shared.unblock(entry.accountKey) }
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                        }
                    }
                }
                .listStyle(.insetGrouped)
            }
        }
        .navigationTitle(l10n.t("block.list.title"))
        .navigationBarTitleDisplayMode(.inline)
    }
}

/// Settings > Privacy > Your reports (UX 2.3): the reports sent from this
/// device, each with the target name as the reporter saw it, the reason, the
/// date and a status fetched with the report's receipt when the list opens.
struct YourReportsScreen: View {
    let accountIdB64: String?
    var client = ModerationClient()

    @ObservedObject private var store = ReportHistoryStore.shared
    @ObservedObject private var l10n = Localization.shared
    @State private var statuses: [String: ModerationClient.Status] = [:]

    private var reports: [SentReport] {
        accountIdB64.map { store.reports(accountIdB64: $0) } ?? []
    }

    var body: some View {
        let rows = reports
        Group {
            if rows.isEmpty {
                Text(l10n.t("report.list.empty"))
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(Color.lakkoBackground)
            } else {
                List(rows) { report in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(report.targetName)
                                .foregroundStyle(Color.lakkoText)
                                .lineLimit(1)
                            Spacer()
                            if let status = statuses[report.receiptId] {
                                Text(l10n.t(Self.statusKey(status)))
                                    .font(.footnote)
                                    .foregroundStyle(Color.lakkoTextMuted)
                            }
                        }
                        Text(Self.reasonKey(report.reason).map { l10n.t($0) } ?? "")
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                        Text(Date(timeIntervalSince1970: Double(report.sentAtMs) / 1000)
                            .formatted(Date.FormatStyle(date: .abbreviated, time: .omitted)
                                .locale(Localization.currentLocale)))
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .padding(.vertical, 2)
                }
                .listStyle(.insetGrouped)
            }
        }
        .navigationTitle(l10n.t("report.list.title"))
        .navigationBarTitleDisplayMode(.inline)
        .task { await loadStatuses(rows) }
    }

    /// The status of each report, fetched with its receipt and nothing else.
    private func loadStatuses(_ rows: [SentReport]) async {
        for report in rows {
            if let status = await client.status(receiptId: report.receiptId) {
                statuses[report.receiptId] = status
            }
        }
    }

    static func statusKey(_ status: ModerationClient.Status) -> String {
        switch status {
        case .received: return "report.status.received"
        case .action: return "report.status.action"
        case .none: return "report.status.none"
        }
    }

    static func reasonKey(_ code: UInt8) -> String? {
        reportReasonName(code: code).map { "report.reason." + $0.replacingOccurrences(of: "-", with: "_") }
    }
}
