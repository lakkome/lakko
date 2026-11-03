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
                photo: row.key.map { .person($0) })
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
