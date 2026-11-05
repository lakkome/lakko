// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// ADR-0052 decision 2 / `docs/ux/message-actions.md` section 5.1 — the two
/// surfaces a forward has: the attribution line inside the bubble, and the
/// conversation picker Forward opens.

/// The one line above a forwarded message's content, inside the bubble: a
/// small forward arrow and "Forwarded from <name>" (or "Forwarded"), in the
/// muted text colour on the sunken bubble and the accent foreground, dimmed,
/// on the accent one — the same treatment the meta row gets, because the
/// muted token itself would vanish on the accent.
///
/// Plain text — no avatar, no username, no verification mark, no link styling
/// (section 5: the name is a label, not a person). A button on every
/// platform, named the same as the line, so tapping it can take the person to
/// the original when this device holds it; the screen decides whether it can.
struct ForwardedLine: View {

    let forward: ForwardRef
    /// Which bubble this sits in, so it takes that bubble's foreground.
    let outgoing: Bool
    let onTap: () -> Void

    var body: some View {
        let line = ForwardPayload.attributionLine(forward)
        Button(action: onTap) {
            HStack(spacing: 5) {
                Image(systemName: "arrowshape.turn.up.right")
                    .chatFont(.caption2)
                Text(line)
                    .chatFont(.caption1)
                    .lineLimit(1)
            }
            .foregroundStyle(outgoing ? Color.lakkoAccentForeground : Color.lakkoTextMuted)
            .opacity(outgoing ? 0.85 : 1)
            // The whole bubble width, not the text's own: above a media
            // bubble the line was proposed a width narrower than the media
            // and read "Forwarded from Te..." (seen live 2026-09-26).
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(line)
        .accessibilityIdentifier(ForwardedLine.accessibilityID)
    }

    static let accessibilityID = "forwarded-line"
}

/// Section 5.1 — "Forward to": the same merged list the Chats tab shows, one
/// row per conversation this device is a live member of, pinned first and
/// newest next, with the search field. Picking a row sends at once — no
/// comment field, no confirmation — and Cancel sends nothing.
///
/// The rows arrive already built and ordered by the chat list's own code
/// (``ChatListScreen/chatRows(viewModel:flags:readCursors:)`` over
/// ``ConversationList``), so this screen cannot disagree with the Chats tab
/// about which conversations exist or in what order; it only draws them and
/// filters by the query.
struct ForwardPickerScreen: View {

    /// Ordered, unfiltered, live memberships only.
    let rows: [ConversationListRow]
    /// The title, when the list is opened for something other than a forward
    /// (`Invite to call`, `docs/ux/call-links.md` section 3). `nil` is the
    /// forward's own.
    var title: String? = nil
    /// One line above the list, in the danger colour: why the last pick did
    /// not go through. The list stays open under it.
    var notice: String? = nil
    /// Rows already dealt with, for a list that stays open after a pick (a
    /// call's `Invite contacts`): each shows a check and ``doneLabel`` and
    /// cannot be picked again.
    var doneIds: Set<String> = []
    var doneLabel: String? = nil
    /// The word on the control that closes the list. `nil` is `Cancel`, which
    /// is right for a list one pick closes; a list that stays open is closed,
    /// not cancelled.
    var cancelTitle: String? = nil
    let onPick: (ConversationListRow) -> Void
    let onCancel: () -> Void

    @State private var searchText: String = ""

    private var visible: [ConversationListRow] {
        ConversationList.filtered(rows, query: searchText)
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                if let notice {
                    Text(notice)
                        .font(.footnote)
                        .foregroundStyle(Color.lakkoDanger)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 16)
                        .padding(.vertical, 8)
                        .accessibilityIdentifier(ForwardPickerScreen.noticeAccessibilityID)
                }
                if visible.isEmpty {
                    Text(Localization.text(rows.isEmpty ? "chat.forward.empty" : "chats.search.no_results"))
                        .foregroundStyle(Color.lakkoTextMuted)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .padding(.top, 80)
                } else {
                    List {
                        ForEach(visible) { row in
                            let done = doneIds.contains(row.id)
                            Button { onPick(row) } label: {
                                HStack(spacing: 8) {
                                    ConversationRowView(row: row, timeLabel: timeLabel(row))
                                    if done {
                                        Label(doneLabel ?? "", systemImage: "checkmark")
                                            .font(.footnote.weight(.medium))
                                            .foregroundStyle(Color.lakkoTextMuted)
                                            .fixedSize()
                                    }
                                }
                            }
                            .buttonStyle(.plain)
                            .disabled(done)
                            .listRowSeparator(.hidden)
                            .listRowInsets(
                                EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                        }
                    }
                    .listStyle(.plain)
                }
            }
            .navigationTitle(title ?? Localization.text("chat.forward.picker_title"))
            .navigationBarTitleDisplayMode(.inline)
            .searchable(
                text: $searchText, prompt: Text(Localization.text("chat.forward.search_label")))
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(cancelTitle ?? Localization.text("common.cancel"), action: onCancel)
                }
            }
        }
        .accessibilityIdentifier(ForwardPickerScreen.accessibilityID)
    }

    private func timeLabel(_ row: ConversationListRow) -> String? {
        row.lastActivityMs.map {
            ChatListFormatting.lastMessageTime(
                ms: $0, nowMs: ChatRepository.nowMs(), calendar: .current, locale: Localization.currentLocale)
        }
    }

    static let accessibilityID = "forward-picker"
    static let noticeAccessibilityID = "forward-picker-notice"
}
