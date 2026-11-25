// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

#if DEBUG

/// `UiTestFixture.pinReorder` — conversation rows with three pins and two
/// unpinned rows, drawn with the list pieces the Chats list uses
/// (``ConversationRowView``, ``PinnedRun``, ``PinOrder``, the
/// ``ConversationRowMenu`` long-press menu, `.moveDisabled` on the unpinned
/// rows and `.onMove`), so a
/// UI test can hold a pinned row and drag it, or hold it and lift.
///
/// The order a drop writes is the shared helper's, kept in local state here
/// instead of the synced register; the lines at the top show it, the last
/// menu the long press opened, and the last row a tap opened.
struct PinReorderFixture: View {
    @State private var order: [String] = ["Anna", "Boris", "Clara"]
    @State private var menuOpenedFor = ""

    private let unpinned = ["Dmitri", "Elena"]

    private var rows: [ConversationListRow] {
        let pins = order.map {
            ConversationListRow(id: $0, kind: .chat, title: $0, preview: "Pinned", lastActivityMs: 1, pinned: true)
        }
        let rest = unpinned.enumerated().map { index, name in
            ConversationListRow(
                id: name, kind: .chat, title: name, preview: "Not pinned",
                lastActivityMs: Int64(10 - index))
        }
        return ConversationList.sorted(pins + rest, pinOrder: order)
    }

    @State private var opened = ""

    var body: some View {
        let ordered = rows
        let pinIds = PinnedRun.split(ordered, isPinned: { $0.pinned }).pinned.map(\.id)
        VStack(spacing: 0) {
            Text(order.joined(separator: ","))
                .accessibilityIdentifier("pin-order")
            Text(menuOpenedFor.isEmpty ? "none" : menuOpenedFor)
                .accessibilityIdentifier("menu-opened")
            Text(opened.isEmpty ? "none" : opened)
                .accessibilityIdentifier("row-opened")
            List {
                ForEach(ordered) { row in
                    item(row, pinIds: pinIds)
                        .moveDisabled(!row.pinned)
                }
                .onMove(perform: PinOrder.dragEnabled(
                    pinCount: pinIds.count, searching: false, inArchive: false)
                    ? { source, destination in
                        guard let drop = PinOrder.dropTarget(
                            pins: pinIds, source: source, destination: destination) else { return }
                        order = PinOrder.next(
                            .move(toIndex: drop.toIndex), stored: order, shown: pinIds, id: drop.id)
                    } : nil)
            }
            .listStyle(.plain)
        }
    }

    private func item(_ row: ConversationListRow, pinIds: [String]) -> some View {
        VStack(spacing: 8) {
            Button { opened = row.id } label: {
                ConversationRowView(row: row, timeLabel: nil)
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("row-\(row.id)")
        }
        .listRowSeparator(.hidden)
        .listRowInsets(EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
        .swipeActions(edge: .trailing, allowsFullSwipe: false) {
            Button("Mute") {}
        }
        .modifier(ConversationRowMenu(draggable: row.pinned && pinIds.count >= 2) {
            [.action("Open menu for \(row.id)", icon: nil) { menuOpenedFor = row.id }]
        })
    }
}

#endif
