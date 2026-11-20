// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// UX `reporting-and-moderation.md` section 2.2 — the report sheet: why,
/// what's included, anything to add, then the follow-up checkbox, the line
/// on who sees it and `Send report`. A bottom sheet on a phone.
struct ReportSheet: View {
    @StateObject var model: ReportSheetModel
    /// Close the sheet. `sent` is true when the report went.
    let onClose: (_ sent: Bool) -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var selecting = false
    @State private var limitHit = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    reasons
                    included
                    commentField
                    followUp
                    whoSees
                    stateLine
                    sendButton
                }
                .padding(16)
            }
            .background(Color.lakkoBackground)
            .navigationTitle(l10n.t("report.title", texts: ["name": model.request.targetName]))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel")) { onClose(false) }
                        .disabled(model.phase == .sending)
                }
            }
            .navigationDestination(isPresented: $selecting) { selectList }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
        .interactiveDismissDisabled(model.phase == .sending)
        .onChange(of: model.phase) { _, phase in
            if phase == .sent { onClose(true) }
        }
    }

    // MARK: - Why

    private var reasons: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(l10n.t("report.why"))
                .font(.headline)
                .foregroundStyle(Color.lakkoText)
                .padding(.bottom, 8)
            ForEach(ReportSheetModel.reasons, id: \.code) { reason in
                Button {
                    model.reason = reason.code
                } label: {
                    HStack(spacing: 12) {
                        Image(systemName: model.reason == reason.code ? "largecircle.fill.circle" : "circle")
                            .foregroundStyle(model.reason == reason.code ? Color.lakkoAccent : Color.lakkoTextMuted)
                        Text(l10n.t(reason.key))
                            .foregroundStyle(Color.lakkoText)
                        Spacer()
                    }
                    .padding(.vertical, 10)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(model.reason == reason.code ? [.isSelected] : [])
                Divider()
            }
        }
    }

    // MARK: - What's included

    private var includedText: String {
        switch model.includedLine {
        case .none: return l10n.t("report.included_none")
        case .count(let n): return l10n.t("report.included", counts: ["n": UInt64(n)])
        case .partial(let included, let total):
            return l10n.t("report.included_partial", texts: ["n": "\(included)", "total": "\(total)"])
        }
    }

    private var included: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(includedText)
                    .foregroundStyle(Color.lakkoText)
                Spacer()
                if !model.request.candidates.isEmpty {
                    Button(l10n.t("report.add_messages")) { selecting = true }
                        .foregroundStyle(Color.lakkoAccent)
                        .disabled(model.phase == .sending)
                }
            }
            if model.requestedCount == 0 {
                Text(l10n.t("report.empty_hint"))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
        }
    }

    // MARK: - Anything to add

    private var commentField: some View {
        TextField(l10n.t("report.comment"), text: $model.comment, axis: .vertical)
            .lineLimit(3...8)
            .padding(10)
            .background(RoundedRectangle(cornerRadius: LakkoRadius.control).fill(Color.lakkoSurface))
            .disabled(model.phase == .sending)
    }

    // MARK: - Then

    @ViewBuilder
    private var followUp: some View {
        if let follow = model.request.followUp {
            Button {
                model.followUpChecked.toggle()
            } label: {
                HStack(spacing: 12) {
                    Image(systemName: model.followUpChecked ? "checkmark.square.fill" : "square")
                        .foregroundStyle(model.followUpChecked ? Color.lakkoAccent : Color.lakkoTextMuted)
                    Text(followUpTitle(follow))
                        .foregroundStyle(Color.lakkoText)
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(model.phase == .sending)
            .accessibilityAddTraits(model.followUpChecked ? [.isSelected] : [])
        }
    }

    private func followUpTitle(_ follow: ReportFollowUp) -> String {
        switch follow {
        case .block: return l10n.t("report.also_block", texts: ["name": model.request.personName ?? model.request.targetName])
        case .leave: return l10n.t("report.also_leave")
        case .unsubscribe: return l10n.t("report.also_unsubscribe")
        }
    }

    private var whoSees: some View {
        var line = l10n.t("report.who_sees")
        if case .person = model.request.target {
            line += " " + l10n.t("report.not_told", texts: ["name": model.request.personName ?? model.request.targetName])
        }
        return Text(line)
            .font(.footnote)
            .foregroundStyle(Color.lakkoTextMuted)
    }

    @ViewBuilder
    private var stateLine: some View {
        switch model.phase {
        case .failed: errorText("report.failed")
        case .limited: errorText("report.limited")
        case .unavailable: errorText("report.unavailable")
        case .editing, .sending, .sent: EmptyView()
        }
    }

    private func errorText(_ key: String) -> some View {
        Text(l10n.t(key))
            .font(.footnote)
            .foregroundStyle(Color.lakkoDanger)
            .accessibilityAddTraits(.updatesFrequently)
    }

    private var sendButton: some View {
        Button {
            Task { await model.send() }
        } label: {
            HStack {
                Spacer()
                if model.phase == .sending {
                    ProgressView().tint(Color.lakkoAccentForeground)
                } else {
                    Text(l10n.t("report.send")).font(.body.weight(.semibold))
                }
                Spacer()
            }
            .padding(.vertical, 12)
            .foregroundStyle(Color.lakkoAccentForeground)
            .background(
                RoundedRectangle(cornerRadius: LakkoRadius.control)
                    .fill(model.canSend ? Color.lakkoAccent : Color.lakkoTextMuted))
        }
        .buttonStyle(.plain)
        .disabled(!model.canSend)
    }

    // MARK: - Choose messages

    private var selectList: some View {
        List {
            if limitHit || model.atLimit {
                Text(l10n.t("report.select.limit", texts: ["n": "\(ReportSheetModel.maxMessages)"]))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            ForEach(model.request.candidates) { candidate in
                Button {
                    limitHit = !model.toggle(candidate.messageId)
                } label: {
                    HStack(alignment: .top, spacing: 12) {
                        Image(systemName: model.isPicked(candidate.messageId) ? "checkmark.circle.fill" : "circle")
                            .foregroundStyle(model.isPicked(candidate.messageId) ? Color.lakkoAccent : Color.lakkoTextMuted)
                        VStack(alignment: .leading, spacing: 2) {
                            if let author = candidate.authorName {
                                Text(author).font(.caption.weight(.semibold)).foregroundStyle(Color.lakkoTextMuted)
                            }
                            Text(candidate.text)
                                .foregroundStyle(Color.lakkoText)
                                .lineLimit(3)
                        }
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(model.isPicked(candidate.messageId) ? [.isSelected] : [])
            }
        }
        .listStyle(.plain)
        .navigationTitle(l10n.t("report.select.title"))
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button(l10n.t("report.select.done")) { selecting = false }
            }
        }
    }
}

/// UX 2.3 — "Report sent. Moderators will review it." at the bottom of the
/// screen, gone by itself.
struct ReportSentToast: View {
    @ObservedObject var center: ReportNoticeCenter = .shared

    var body: some View {
        if let shown = center.shown {
            Text(shown.text)
                .font(.subheadline)
                .foregroundStyle(Color.lakkoText)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(Capsule().fill(Color.lakkoSurface).shadow(radius: 6, y: 2))
                .padding(.horizontal, 16)
                .padding(.bottom, 72)
                .id(shown.id)
                .transition(.opacity)
                .allowsHitTesting(false)
                .onAppear { UIAccessibility.post(notification: .announcement, argument: shown.text) }
        }
    }
}
