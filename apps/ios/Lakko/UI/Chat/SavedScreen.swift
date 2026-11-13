// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// One row of the Saved screen, built from the saved entry and, when this
/// device holds it, the message itself (pure).
struct SavedRow: Equatable, Identifiable {
    let id: String
    /// The saved message: its segment and DS id.
    let ref: SavedMessageRef
    /// The conversation holding the segment (its current group), and the
    /// row's on-screen id there; `nil` while this device does not know the
    /// segment (the row is drawn from the snapshot and does not open).
    let openGroupIdHex: String?
    let rowId: Int64?
    let chatTitle: String
    let author: String
    let line: String
    /// The message's date (UX section 3), see ``SavedMessages/rowDateMs``.
    let dateMs: UInt64
    /// The held message's first attachment, for the thumbnail.
    let attachment: AttachmentRef?
    /// Whether the message is held, so the row opens.
    var held: Bool { rowId != nil && openGroupIdHex != nil }

    /// Most recently saved first. A row's conversation is the one whose
    /// lineage holds the saved segment; its title is that conversation's
    /// (`title` takes the conversation's current group id).
    static func rows(
        _ entries: [SavedMessageView],
        threads: [ChatThread],
        headOf: (String) -> String = { $0 },
        title: (String) -> String?
    ) -> [SavedRow] {
        SavedMessages.ordered(entries).map { entry in
            let ref = SavedMessages.ref(of: entry)
            // A held segment first; else the conversation the lineage
            // register names for it (its title, the row not openable).
            let thread = threads.first { $0.holds(segmentIdHex: ref.segmentIdHex) }
            let lineageThread = thread == nil
                ? threads.first { $0.groupIdHex == headOf(ref.segmentIdHex) } : nil
            let rowId = thread?.rowId(of: ref)
            let message = rowId.flatMap { id in thread?.messages.first { $0.messageId == id } }
            let line: String
            if let message, !message.deleted {
                let snap = SavedMessages.snapshot(
                    text: message.text, attachments: message.attachments, author: "",
                    invitationLine: message.invitationLine(conversationIdHex: ref.segmentIdHex))
                line = SavedMessages.rowLine(excerpt: snap.excerpt, kind: snap.kind)
            } else {
                line = SavedMessages.rowLine(excerpt: entry.excerpt, kind: entry.attachmentKind)
            }
            return SavedRow(
                id: "\(ref.segmentIdHex):\(entry.messageId)",
                ref: ref,
                openGroupIdHex: thread?.groupIdHex,
                rowId: message == nil ? nil : rowId,
                chatTitle: (thread ?? lineageThread).flatMap { title($0.groupIdHex) } ?? "",
                author: entry.author,
                line: line,
                dateMs: SavedMessages.rowDateMs(
                    snapshotSentAtMs: entry.sentAtMs,
                    heldSentAtMs: message.map(SavedMessages.sentAtMs),
                    savedAtMs: entry.savedAtMs),
                attachment: message?.attachments.first)
        }
    }
}

/// UX `saved-messages-and-file-lifetime.md` section 3 — Saved.
struct SavedScreen: View {
    @ObservedObject var center: SavedMessagesCenter
    @ObservedObject var chat: ChatRepository
    /// The current group of the conversation a group id belongs to (the
    /// conversation lineage register).
    var headOf: (String) -> String = { $0 }
    /// The chat's title for its current group id.
    let title: (String) -> String?
    /// Open the chat (by its current group) at the row.
    let onOpen: (_ groupIdHex: String, _ rowId: Int64) -> Void

    @ObservedObject private var l10n = Localization.shared
    @State private var error: UiText?

    private var rows: [SavedRow] {
        SavedRow.rows(center.entries, threads: chat.state.threads, headOf: headOf, title: title)
    }

    var body: some View {
        Group {
            if rows.isEmpty {
                VStack(spacing: 12) {
                    LkIconView(.bookmark, size: 48, relativeTo: nil)
                        .foregroundStyle(Color.lakkoTextMuted)
                    Text(l10n.t("saved.empty"))
                        .foregroundStyle(Color.lakkoTextMuted)
                        .multilineTextAlignment(.center)
                }
                .padding(32)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List {
                    if let error {
                        Text(error.text).font(.footnote).foregroundStyle(Color.lakkoDanger)
                    }
                    ForEach(rows) { row in
                        rowView(row)
                            .onTapGesture {
                                if let group = row.openGroupIdHex, let rowId = row.rowId { onOpen(group, rowId) }
                            }
                            .accessibilityAddTraits(.isButton)
                            // Lakko's own long-press menu, not the system one.
                            .lakkoLongPressMenu {
                                [.action(l10n.t("chat.message.unsave"), icon: .bookmarkSlash, destructive: true) {
                                    remove(row)
                                }]
                            }
                    }
                }
                .listStyle(.plain)
            }
        }
        .navigationTitle(l10n.t("saved.title"))
        .navigationBarTitleDisplayMode(.inline)
    }

    private func rowView(_ row: SavedRow) -> some View {
        HStack(alignment: .top, spacing: 12) {
            if let ref = row.attachment, InlineMedia.decision(for: ref).bubble == .photo {
                SavedThumb(ref: ref)
            }
            VStack(alignment: .leading, spacing: 2) {
                HStack {
                    Text(row.chatTitle).font(.subheadline.weight(.semibold)).lineLimit(1)
                    Spacer()
                    Text(Self.date(row.dateMs, locale: l10n.locale))
                        .font(.caption).foregroundStyle(Color.lakkoTextMuted)
                }
                if !row.author.isEmpty {
                    Text(row.author).font(.caption).foregroundStyle(Color.lakkoTextMuted).lineLimit(1)
                }
                Text(row.line).font(.body).foregroundStyle(Color.lakkoText).lineLimit(2)
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
    }

    private func remove(_ row: SavedRow) {
        Task {
            do {
                try await center.unsave(row.ref)
                error = nil
            } catch {
                self.error = .key("chat.error.save")
            }
        }
    }

    static func date(_ ms: UInt64, locale: Locale) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(ms) / 1000)
        return date.formatted(.dateTime.day().month(.abbreviated).year().locale(locale))
    }
}

/// A saved photo's thumbnail, from the held message.
private struct SavedThumb: View {
    let ref: AttachmentRef
    @State private var image: UIImage?

    var body: some View {
        Group {
            if let image {
                Image(uiImage: image).resizable().scaledToFill()
            } else {
                Color.lakkoSurfaceSunken
            }
        }
        .frame(width: 44, height: 44)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .task(id: ref.blobId) {
            guard let thumb = ref.thumb, let bytes = await MediaSessionCache.shared.thumbData(thumb) else { return }
            image = UIImage(data: bytes)
        }
    }
}
