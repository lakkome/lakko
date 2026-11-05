// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Reactions-and-emoji spec section 7 — chips wrap onto more lines when
/// needed, aligned to the bubble's side by the column that holds them.
struct ReactionChipFlow: Layout {
    var spacing: CGFloat = 4

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var lineHeight: CGFloat = 0
        var widest: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > 0, x + size.width > maxWidth {
                y += lineHeight + spacing
                x = 0
                lineHeight = 0
            }
            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
            widest = max(widest, x - spacing)
        }
        return CGSize(width: widest, height: y + lineHeight)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        var x = bounds.minX
        var y = bounds.minY
        var lineHeight: CGFloat = 0
        for view in subviews {
            let size = view.sizeThatFits(.unspecified)
            if x > bounds.minX, x + size.width > bounds.maxX {
                y += lineHeight + spacing
                x = bounds.minX
                lineHeight = 0
            }
            view.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            lineHeight = max(lineHeight, size.height)
        }
    }
}

/// One person's row in the reactions list.
struct ReactorRow: Identifiable, Equatable {
    let id: String
    /// The 32-byte account key when known (an avatar, a contact link).
    let key: Data?
    let name: String
    let emoji: String
    let isOwn: Bool
}

/// Section 8.1 — who reacted with what, opened from a chip: a bottom sheet,
/// tabs `All N` then one per emoji in chip order, opening on the chip that
/// was pressed. Built from what the device holds; nothing is sent.
struct ReactionsListSheet: View {
    let rows: [ReactorRow]
    /// The emoji, in chip order.
    let emojiOrder: [String]
    @State var selected: String?
    let onOpenPerson: ((Data) -> Void)?

    @ObservedObject private var l10n = Localization.shared

    private var shown: [ReactorRow] {
        let filtered = selected.map { emoji in rows.filter { $0.emoji == emoji } } ?? rows
        return filtered.filter(\.isOwn) + filtered.filter { !$0.isOwn }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(l10n.t("chat.reaction.list_title"))
                .font(.headline)
                .foregroundStyle(Color.lakkoText)
                .padding(.horizontal, 20)
                .padding(.top, 20)
                .padding(.bottom, 8)
            if emojiOrder.count > 1 {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 6) {
                        tab(label: l10n.t("chat.reaction.list_all", counts: ["count": UInt64(rows.count)]), emoji: nil)
                        ForEach(emojiOrder, id: \.self) { emoji in
                            let count = rows.filter { $0.emoji == emoji }.count
                            tab(label: "\(emoji) \(count.formatted(.number.locale(l10n.locale)))", emoji: emoji)
                        }
                    }
                    .padding(.horizontal, 20)
                }
                .padding(.bottom, 8)
            }
            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(shown) { row in
                        reactorRow(row)
                        Divider().padding(.leading, 64)
                    }
                }
            }
        }
        .background(Color.lakkoSurface)
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
    }

    private func tab(label: String, emoji: String?) -> some View {
        let on = selected == emoji
        return Button { selected = emoji } label: {
            Text(label)
                .font(.subheadline)
                .foregroundStyle(on ? Color.lakkoText : Color.lakkoTextMuted)
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
                .background(Capsule().fill(on ? Color.lakkoAccentSoft : Color.lakkoSurfaceSunken))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(on ? .isSelected : [])
    }

    @ViewBuilder
    private func reactorRow(_ row: ReactorRow) -> some View {
        let content = HStack(spacing: 12) {
            InitialsAvatar(
                name: row.name, identity: row.key?.base64EncodedString() ?? row.id, size: 36,
                photo: row.isOwn ? .own : row.key.map { .person($0) })
            Text(row.isOwn ? l10n.t("chat.author.you") : row.name)
                .font(.body)
                .foregroundStyle(Color.lakkoText)
                .lineLimit(1)
            Spacer()
            if selected == nil {
                Text(row.emoji).font(.title3)
            }
        }
        .padding(.horizontal, 20)
        .frame(minHeight: 52)
        .contentShape(Rectangle())
        if let key = row.key, !row.isOwn, let onOpenPerson {
            Button { onOpenPerson(key) } label: { content }.buttonStyle(.plain)
        } else {
            content
        }
    }
}

/// The design-system "Reaction chip" (`.claude/skills/ux-ui/design-system.md`
/// section 3): every size the chip is drawn at, at the default text size. The
/// view scales each one with the person's text size (``ReactionChipLabel``);
/// these are the values a test and a reader compare with the spec.
enum ReactionChipMetrics {
    static let height: CGFloat = 32
    static let horizontalPadding: CGFloat = 10
    static let emojiSize: CGFloat = 18
    static let avatarSize: CGFloat = 22
    static let avatarRing: CGFloat = 1
    static let avatarOverlap: CGFloat = 4
    static let maxAvatars = 3
    static let contentGap: CGFloat = 4
    static let countSize: CGFloat = 14
    /// Between chips and between wrapped lines.
    static let chipSpacing: CGFloat = 6
    static let ownBorder: CGFloat = 1
}

/// What a chip shows after its emoji.
enum ReactionChipContent {
    case emojiOnly
    case count(String)
    /// Reactor keys (base64), at most ``ReactionChipMetrics/maxAvatars``
    /// drawn, each with its display name and photo.
    case avatars([(name: String, identity: String, photo: AvatarPhoto?)])
}

/// One reaction chip, or the "+N" overflow chip (`emoji` nil, the count
/// alone): a capsule of the design-system sizes, each scaled with the
/// person's text size so a larger Dynamic Type setting grows the chip, and
/// all of them together by the chat text size (`docs/ux/text-size.md`).
struct ReactionChipLabel: View {
    /// `nil` for the "+N" chip.
    let emoji: String?
    let content: ReactionChipContent
    /// The own chip: accent-soft fill and a 1 accent border.
    var mine: Bool = false

    @ScaledMetric(relativeTo: .subheadline) private var height = ReactionChipMetrics.height
    @ScaledMetric(relativeTo: .subheadline) private var padding = ReactionChipMetrics.horizontalPadding
    @ScaledMetric(relativeTo: .body) private var emojiSize = ReactionChipMetrics.emojiSize
    @ScaledMetric(relativeTo: .subheadline) private var avatarSize = ReactionChipMetrics.avatarSize
    @ScaledMetric(relativeTo: .subheadline) private var overlap = ReactionChipMetrics.avatarOverlap
    @ScaledMetric(relativeTo: .subheadline) private var gap = ReactionChipMetrics.contentGap
    @ScaledMetric(relativeTo: .subheadline) private var countSize = ReactionChipMetrics.countSize
    @Environment(\.chatTextScale) private var scale

    var body: some View {
        HStack(spacing: gap * scale) {
            if let emoji {
                Text(emoji).font(.system(size: emojiSize * scale))
            }
            switch content {
            case .emojiOnly:
                EmptyView()
            case .count(let text):
                Text(text)
                    .font(.system(size: countSize * scale))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .lineLimit(1)
            case .avatars(let reactors):
                HStack(spacing: -overlap * scale) {
                    ForEach(Array(reactors.prefix(ReactionChipMetrics.maxAvatars).enumerated()), id: \.offset) { _, r in
                        InitialsAvatar(name: r.name, identity: r.identity, size: avatarSize * scale, photo: r.photo)
                            // The ring sits outside the avatar, so the avatar
                            // keeps its full size and the overlap reads as a cut.
                            .background(
                                Circle()
                                    .fill(Color.lakkoSurface)
                                    .padding(-ReactionChipMetrics.avatarRing))
                    }
                }
                .padding(.horizontal, ReactionChipMetrics.avatarRing)
            }
        }
        .padding(.horizontal, padding * scale)
        .frame(minHeight: height * scale)
        .background(mine ? Color.lakkoAccentSoft : Color.lakkoSurfaceSunken)
        .overlay(
            Capsule()
                .strokeBorder(mine ? Color.lakkoAccent : Color.clear, lineWidth: ReactionChipMetrics.ownBorder)
        )
        .clipShape(Capsule())
    }
}
