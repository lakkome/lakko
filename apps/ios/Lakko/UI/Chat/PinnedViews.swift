// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0052 decision 3 / `docs/ux/message-actions.md` sections 4 and 4.1 —
/// the three surfaces a pin has: the bar under the header, the sheet that asks
/// who the pin is for, and the list of every pin.
///
/// All three draw the same two strings per pin through
/// ``ChatViewModel/pinnedLines(for:)`` — the author (or `For you`) and the
/// excerpt — so the bar, the list and the reply quote cannot disagree about
/// what a message is called. None of them reads a store: the entries arrive
/// already ordered by ``PinnedBar``.

/// Section 4 — one pinned message at a time under the header: a stacked
/// indicator that says how many and which, the author and the excerpt on one
/// line each, and a list control at the far end. Tapping the row jumps to the
/// message and advances to the next older pin; the screen owns both.
struct PinnedBarView: View {

    /// ADR-0061 — observed so a language switch re-renders the view.
    @ObservedObject var l10n = Localization.shared
    let entry: PinnedEntry
    let index: Int
    let count: Int
    let author: String
    let excerpt: String
    let onTap: () -> Void
    let onList: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Button(action: onTap) {
                HStack(spacing: 10) {
                    PinnedStackIndicator(index: index, count: count)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(author.isEmpty ? l10n.t("chat.pinned_bar.label") : author)
                            .font(.caption)
                            .fontWeight(.semibold)
                            .foregroundStyle(Color.lakkoAccent)
                            .lineLimit(1)
                        Text(excerpt)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoText)
                            .lineLimit(1)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                Localization.joinList(
                    [PinnedBar.positionLabel(index: index, count: count)]
                        + (author.isEmpty ? [] : [author]) + [excerpt]))
            .accessibilityHint(l10n.t("chat.pinned_bar.hint"))
            Button(action: onList) {
                Image(systemName: "list.bullet")
                    .font(.system(size: 15, weight: .semibold))
                    .foregroundStyle(Color.lakkoTextMuted)
                    .frame(width: 36, height: 36)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(l10n.t("chat.pinned_bar.all"))
        }
        .padding(.leading, 12)
        .padding(.trailing, 4)
        .padding(.vertical, 6)
        .background(Color.lakkoSurface)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 0.5)
        }
        .accessibilityIdentifier(PinnedBarView.accessibilityID)
    }

    static let accessibilityID = "pinned-bar"
}

/// The thin stacked indicator beside the excerpt: one segment per pin (up to
/// a few, the way Telegram draws it), the shown one in the accent colour and
/// the rest in the border colour. It says how many there are and which one
/// this is without a number, and VoiceOver reads the number instead.
struct PinnedStackIndicator: View {

    let index: Int
    let count: Int

    /// More than this many pins are drawn as this many segments; the shown
    /// one is kept visible by sliding the window over the list.
    static let maxSegments = 4

    var body: some View {
        let segments = min(max(count, 1), Self.maxSegments)
        let window = Self.window(index: index, count: count, segments: segments)
        VStack(spacing: 2) {
            ForEach(0..<segments, id: \.self) { slot in
                RoundedRectangle(cornerRadius: 1, style: .continuous)
                    .fill(window + slot == index ? Color.lakkoAccent : Color.lakkoBorder)
                    .frame(width: 2.5)
            }
        }
        .frame(height: 30)
        .accessibilityHidden(true)
    }

    /// The first index the segments stand for, so the shown one is always one
    /// of them: the window slides once the shown index leaves it.
    static func window(index: Int, count: Int, segments: Int) -> Int {
        guard count > segments else { return 0 }
        return min(max(0, index - segments + 1), count - segments)
    }
}

/// Section 4.1 "The sheet" — the two-row choice: who is this pin for. The rows
/// arrive already narrowed to the message and this device's standing (a plain
/// member sees one row and no explanation; the missing row is the
/// explanation). Cancel does nothing.
struct PinScopeSheet: View {

    /// ADR-0061 — observed so a language switch re-renders the view.
    @ObservedObject var l10n = Localization.shared
    let options: [PinSheetOption]
    let onChoose: (PinChoice) -> Void
    let onCancel: () -> Void

    var body: some View {
        NavigationStack {
            List(options) { option in
                Button {
                    onChoose(option.choice)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(option.title)
                            .font(.body)
                            .foregroundStyle(Color.lakkoText)
                        Text(option.subtitle)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoTextMuted)
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(option.title)
                .accessibilityHint(option.subtitle)
                .listRowBackground(Color.lakkoSurface)
            }
            .scrollContentBackground(.hidden)
            .background(Color.lakkoBackground)
            .navigationTitle(
                l10n.t(options.first?.choice.isUnpin == true ? "chat.unpin.sheet_title" : "chat.pin.sheet_title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.cancel"), action: onCancel)
                        .foregroundStyle(Color.lakkoAccent)
                }
            }
        }
        // The inline navigation bar, the list's top inset, one two-line row per
        // option and the home indicator. Measured on the simulator: the earlier
        // `60 + 64 * rows` left the only row half under the bottom edge.
        .presentationDetents([.height(CGFloat(120 + options.count * 76))])
    }
}

/// Section 4.1 — every pin as rows, newest first, shared before personal,
/// each row jumping to its message and closing the list.
struct PinnedListSheet: View {

    /// ADR-0061 — observed so a language switch re-renders the view.
    @ObservedObject var l10n = Localization.shared
    let entries: [PinnedEntry]
    let lines: (PinnedEntry) -> (author: String, excerpt: String)
    let onPick: (PinnedEntry) -> Void
    let onClose: () -> Void

    var body: some View {
        NavigationStack {
            List(entries) { entry in
                let line = lines(entry)
                Button {
                    onPick(entry)
                } label: {
                    HStack(spacing: 10) {
                        Image(systemName: entry.scope == .personal ? "pin" : "pin.fill")
                            .font(.caption)
                            .foregroundStyle(Color.lakkoTextMuted)
                            .frame(width: 18)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(line.author.isEmpty ? l10n.t("chat.pinned_bar.label") : line.author)
                                .font(.caption)
                                .fontWeight(.semibold)
                                .foregroundStyle(Color.lakkoAccent)
                                .lineLimit(1)
                            Text(line.excerpt)
                                .font(.footnote)
                                .foregroundStyle(Color.lakkoText)
                                .lineLimit(2)
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .listRowBackground(Color.lakkoSurface)
            }
            .scrollContentBackground(.hidden)
            .background(Color.lakkoBackground)
            .navigationTitle(l10n.t("chat.pinned_bar.list_title"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(l10n.t("common.close"), action: onClose)
                        .foregroundStyle(Color.lakkoAccent)
                }
            }
        }
        .presentationDetents([.medium, .large])
    }
}

extension PinChoice {
    /// Whether the choice clears a pin (the sheet's title follows the rows).
    var isUnpin: Bool {
        switch self {
        case .unpinForMe, .unpinForEveryone: return true
        case .pinForMe, .pinForEveryone: return false
        }
    }
}
