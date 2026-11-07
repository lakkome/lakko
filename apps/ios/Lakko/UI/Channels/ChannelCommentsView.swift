// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI
import UIKit

/// `docs/ux/group-and-channel-privacy.md` section 13.2 — the comments under
/// a post, in place: the first two and `Show {n} more comments`, a quiet
/// `Comment` when there are none, the field once the thread is open. A name
/// is what its author typed: no `@username`, no profile link.
struct ChannelCommentsView: View {
    @ObservedObject var model: ChannelConversationModel
    let item: ChannelTimelineItem
    let isPublisher: Bool
    /// The full reaction picker for a comment (the reaction row's `+`).
    var onMoreReactions: (ChannelConversationModel.Comment) -> Void = { _ in }

    @ObservedObject private var l10n = Localization.shared
    /// The message menu every message and post uses (quick reactions on top,
    /// then the actions that apply), opened on a comment too.
    @Environment(\.messageMenuPresenter) private var menuPresenter
    /// Each comment's last on-screen frame, for where the menu draws it.
    @State private var frames = CommentFrames()
    @State private var draft = ""
    @State private var replyTo: ChannelConversationModel.Comment?
    @State private var sending = false

    private var thread: ChannelConversationModel.CommentThread? { model.threads[item.cursor] }
    private var comments: [ChannelConversationModel.Comment] { model.visibleComments(item.cursor) }
    private var expanded: Bool { thread?.expanded ?? false }
    /// The post's comment count: the service's (ADR-0087 phase B), else the
    /// comments loaded.
    private var total: Int {
        max(Int(clamping: model.commentCountByPost[item.cursor] ?? 0), comments.count)
    }
    private var canWrite: Bool { model.access == .reading || model.access == .publisher }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            if thread?.loading == true, comments.isEmpty {
                // Loading: a thin line under the post, no spinner.
                Rectangle().fill(Color.lakkoBorder).frame(height: 1).frame(maxWidth: 120)
            } else if thread?.failedToLoad == true {
                HStack(spacing: 8) {
                    Text(l10n.t("channels.comments.failed"))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                    Button(l10n.t("common.try_again")) { Task { await model.loadComments(item) } }
                        .font(.footnote)
                }
            }
            let shown = expanded ? comments : Array(comments.prefix(2))
            ForEach(shown) { comment in commentRow(comment) }
            ForEach(Array((thread?.failedSends ?? []).enumerated()), id: \.offset) { _, failed in
                failedRow(failed)
            }
            if !expanded {
                if total > 2 || thread?.mayHaveMore == true {
                    Button {
                        model.expandComments(item)
                    } label: {
                        Text(l10n.t("channels.comments.show_more", counts: [
                            "count": UInt64(max(total - min(comments.count, 2), 1))]))
                            .font(.footnote.weight(.medium))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Color.lakkoAccent)
                } else if model.access == .reading || model.access == .publisher {
                    Button { model.expandComments(item) } label: {
                        Text(l10n.t(total == 0 ? "channels.comments.comment" : "channels.comments.write"))
                            .font(.footnote)
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Color.lakkoTextMuted)
                }
            } else {
                if thread?.mayHaveMore == true {
                    Button { Task { await model.loadComments(item, more: true) } } label: {
                        Text(l10n.t("channels.comments.more")).font(.footnote.weight(.medium))
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Color.lakkoAccent)
                }
                if let newCount = thread?.newSinceOpen, newCount > 0 {
                    Text(l10n.t("channels.comments.new", counts: ["count": UInt64(newCount)]))
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                field
                Button { model.collapseComments(item) } label: {
                    Text(l10n.t("channels.comments.hide")).font(.footnote)
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color.lakkoTextMuted)
            }
        }
    }

    private func commentRow(_ comment: ChannelConversationModel.Comment) -> some View {
        commentContent(comment, chips: true)
            .contentShape(Rectangle())
            .onGeometryChange(for: CGRect.self) { $0.frame(in: .global) } action: {
                frames.byCursor[comment.cursor] = $0
            }
            .onLongPressGesture(minimumDuration: MessageLongPress.thresholdSeconds) { openMenu(comment) }
            .accessibilityAction(named: l10n.t("channels.comments.actions")) { openMenu(comment) }
    }

    /// One comment as drawn: the author's avatar, a quote of what it answers,
    /// the name and text, and (in the timeline) its reaction chips.
    private func commentContent(_ comment: ChannelConversationModel.Comment, chips: Bool) -> some View {
        HStack(alignment: .top, spacing: 8) {
            InitialsAvatar(name: comment.name, identity: comment.authorKey.base64EncodedString(), size: 24)
            VStack(alignment: .leading, spacing: 2) {
                if let target = comment.replyTo,
                   let quoted = comments.first(where: { $0.cursor == target }) {
                    Text(quoted.name + ": " + quoted.text)
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                        .lineLimit(1)
                }
                (Text(comment.name.isEmpty ? l10n.t("channels.members.unnamed") : comment.name)
                    .font(.footnote.weight(.semibold))
                 + Text("  " + comment.text).font(.footnote))
                    .foregroundStyle(Color.lakkoText)
                    .fixedSize(horizontal: false, vertical: true)
                if chips { commentChips(comment) }
            }
            Spacer(minLength: 0)
        }
    }

    /// Open the message menu on a comment: the quick reactions on top (the
    /// comment's own row), then Reply, Copy, Delete and Ban as they apply.
    private func openMenu(_ comment: ChannelConversationModel.Comment) {
        guard let menuPresenter, let frame = frames.byCursor[comment.cursor], frame != .zero else { return }
        let row = canWrite && comment.cursor != 0 ? model.commentReactionRow(comment, of: item) : .empty
        let canReact = canWrite && comment.cursor != 0 && !row.emoji.isEmpty
        let actions = Self.commentActions(
            canReply: canWrite, own: comment.own, isPublisher: isPublisher, hasText: !comment.text.isEmpty)
        guard canReact || !actions.isEmpty else { return }
        menuPresenter.open(MessageMenuRequest(
            id: -Int64(clamping: comment.cursor) - 1,
            bubbleFrame: frame,
            isOutgoing: false,
            actions: actions,
            canReact: canReact,
            reactionRow: row,
            bubble: AnyView(
                commentContent(comment, chips: false)
                    .padding(8)
                    .background(RoundedRectangle(cornerRadius: 12).fill(Color.lakkoSurface))),
            onReact: { emoji in Task { await model.reactComment(comment, of: item, emoji: emoji) } },
            onMoreReactions: { onMoreReactions(comment) },
            perform: { action in
                switch action {
                case .reply:
                    replyTo = comment
                    model.expandComments(item)
                case .copy:
                    MessageClipboard.copy(comment.text)
                case .delete:
                    Task { await model.deleteComment(comment, of: item) }
                case .ban:
                    Task { await model.ban(comment) }
                default:
                    break
                }
            }))
    }

    /// The comment menu's actions, in the message menu's order: Reply, Copy,
    /// then the destructive ones (Delete for its writer or a publisher, Ban
    /// for a publisher on someone else's comment).
    nonisolated static func commentActions(
        canReply: Bool, own: Bool, isPublisher: Bool, hasText: Bool
    ) -> [MessageMenuAction] {
        var actions: [MessageMenuAction] = []
        if canReply { actions.append(.reply) }
        if hasText { actions.append(.copy) }
        if own || isPublisher { actions.append(.delete) }
        if isPublisher, !own { actions.append(.ban) }
        return actions
    }

    /// Section 13a (phase B) — the reaction chips of a comment, as under a
    /// post: a tap reacts, the counts are anonymous.
    @ViewBuilder
    private func commentChips(_ comment: ChannelConversationModel.Comment) -> some View {
        let row = model.commentChips(comment, of: item)
        if !row.chips.isEmpty {
            ReactionChipFlow(spacing: ReactionChipMetrics.chipSpacing) {
                ForEach(row.chips, id: \.emoji) { chip in
                    ReactionChipLabel(
                        emoji: chip.emoji,
                        content: .count(Int(chip.count).formatted(.number.locale(l10n.locale))),
                        mine: chip.mine)
                        .contentShape(Rectangle())
                        .onTapGesture {
                            guard canWrite else { return }
                            Task { await model.reactComment(comment, of: item, emoji: chip.emoji) }
                        }
                        .accessibilityAddTraits(.isButton)
                        .accessibilityLabel(chip.mine
                            ? l10n.t("chat.reaction.chip_yours_a11y", texts: ["emoji": chip.emoji])
                            : l10n.t("chat.reaction.chip_a11y", texts: ["emoji": chip.emoji],
                                     counts: ["count": UInt64(chip.count)]))
                }
            }
        }
    }

    private func failedRow(_ comment: ChannelConversationModel.Comment) -> some View {
        HStack(spacing: 8) {
            Text(comment.text).font(.footnote).foregroundStyle(Color.lakkoTextMuted).lineLimit(2)
            Spacer(minLength: 0)
            Button(l10n.t("common.retry")) {
                Task { await model.comment(item, text: comment.text, replyTo: comment.replyTo) }
            }
            .font(.footnote)
        }
    }

    @ViewBuilder
    private var field: some View {
        if model.access == .reading || model.access == .publisher {
            VStack(alignment: .leading, spacing: 4) {
                if let replyTo {
                    HStack {
                        Text(replyTo.name + ": " + replyTo.text)
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .lineLimit(1)
                        Spacer()
                        Button { self.replyTo = nil } label: { Image(systemName: "xmark") }
                            .buttonStyle(.plain)
                            .accessibilityLabel(l10n.t("common.cancel"))
                    }
                }
                HStack(spacing: 6) {
                    TextField(l10n.t("channels.comments.write"), text: $draft, axis: .vertical)
                        .lineLimit(1...4)
                        .font(.footnote)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 7)
                        .background(RoundedRectangle(cornerRadius: 16).fill(Color.lakkoSurfaceSunken))
                        .disabled(sending)
                    Button {
                        let text = draft
                        let target = replyTo?.cursor
                        sending = true
                        Task {
                            if await model.comment(item, text: text, replyTo: target) {
                                draft = ""
                                replyTo = nil
                            }
                            sending = false
                        }
                    } label: {
                        Image(systemName: "arrow.up.circle.fill")
                            .font(.system(size: 26))
                            .foregroundStyle(
                                draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                                    ? Color.lakkoTextMuted : Color.lakkoAccent)
                    }
                    .buttonStyle(.plain)
                    .disabled(sending || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityLabel(l10n.t("chat.composer.send"))
                }
            }
        }
    }
}

/// The comments' last on-screen frames: a reference type, so recording a
/// frame on every scroll tick does not redraw the list.
final class CommentFrames {
    var byCursor: [UInt64: CGRect] = [:]
}
