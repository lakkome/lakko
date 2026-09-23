// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The Calls destination (`docs/ux/navigation.md` section 5): recent calls,
/// newest first, flat — not grouped by conversation — with day separators, and
/// the two link actions the design system's information architecture has
/// always given this screen.
///
/// The destination was removed by the redesign on the ground that "Calls has
/// no history, only a create-link panel". That was true of what existed; it
/// was never an argument that call history should not exist. It does now:
/// every finished call writes a row into ``CallHistoryStore`` at the moment it
/// writes the conversation's own row, and this screen reads that index. It
/// never walks a transcript.
///
/// Two targets per row, and their hit areas must not overlap: the row opens
/// the conversation, the trailing button calls back **in the same mode the row
/// records** — a video call re-dials video.
struct CallsScreen: View {

    /// The call index. Observed, so a call that has just ended is on the
    /// screen without this having to poll.
    @ObservedObject var history: CallHistoryStore
    /// Titles come from the conversation list's own resolver, so a call and
    /// its chat are never named two ways.
    @ObservedObject var conversations: ChatListViewModel
    /// ADR-0032 call links — the session the owner signs the capability with.
    let authRepository: AuthRepository
    /// Open the conversation a row belongs to.
    let onOpenConversation: (_ groupIdHex: String, _ kind: CallHistoryKind) -> Void
    /// Call that conversation back, in the mode the row records.
    let onCallBack: (_ groupIdHex: String, _ kind: CallHistoryKind, _ video: Bool) -> Void
    /// Hand a pasted or minted `/c/` link to the same intake a tapped link
    /// uses, so joining by link and tapping a link are one path.
    let onOpenLink: (URL) -> Void

    @State private var createLinkPresented = false
    @State private var joinByLinkPresented = false
    @State private var pastedLink = ""
    @State private var joinError: String?

    private let calendar = Calendar.current
    private let locale = Locale.current

    var body: some View {
        content
            .navigationTitle("Calls")
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Menu {
                        Button { createLinkPresented = true } label: {
                            Label("Create call link", systemImage: "phone.badge.plus")
                        }
                        Button { joinByLinkPresented = true } label: {
                            Label("Join by link", systemImage: "link")
                        }
                    } label: {
                        Image(systemName: "phone.badge.plus")
                    }
                    .accessibilityLabel("Call link")
                }
            }
            .sheet(isPresented: $createLinkPresented) {
                CreateCallLinkSheet(
                    authRepository: authRepository,
                    onDismiss: { createLinkPresented = false },
                    onStartCall: { url in
                        createLinkPresented = false
                        if let parsed = URL(string: url) { onOpenLink(parsed) }
                    })
            }
            .sheet(isPresented: $joinByLinkPresented) { joinByLinkSheet }
    }

    @ViewBuilder
    private var content: some View {
        let days = CallHistory.byDay(history.entries, calendar: calendar)
        if days.isEmpty {
            ScrollView { emptyState.padding(.top, 80) }
        } else {
            List {
                ForEach(days) { day in
                    Section {
                        ForEach(day.rows) { entry in
                            row(entry)
                                .listRowSeparator(.hidden)
                                .listRowInsets(
                                    EdgeInsets(top: 6, leading: 16, bottom: 6, trailing: 16))
                        }
                    } header: {
                        Text(
                            CallHistory.dayLabel(
                                day.day, now: Date(), calendar: calendar, locale: locale))
                    }
                }
            }
            .listStyle(.plain)
        }
    }

    // MARK: - Row

    /// Avatar and title as the conversation list draws them, the outcome and
    /// the time on the second line, a direction glyph at the leading edge, and
    /// a call-back button on the trailing edge.
    ///
    /// The two buttons are siblings rather than one nested in the other:
    /// SwiftUI would let a button inside a button take the whole row's taps,
    /// and the spec is explicit that these are two targets.
    private func row(_ entry: CallHistoryEntry) -> some View {
        let title = conversations.title(groupIdHex: entry.conversationId)
        let tint = entry.isUnanswered ? Color.lakkoDanger : Color.lakkoTextMuted
        return HStack(spacing: 12) {
            Button {
                onOpenConversation(entry.conversationId, entry.kind)
            } label: {
                HStack(spacing: 12) {
                    InitialsAvatar(name: title, identity: entry.conversationId, size: 40)
                    VStack(alignment: .leading, spacing: 3) {
                        Text(title)
                            .font(.body)
                            .foregroundStyle(Color.lakkoText)
                            .lineLimit(1)
                        HStack(spacing: 6) {
                            Image(systemName: CallHistory.directionGlyph(entry))
                                .font(.caption2)
                            Text(
                                CallHistory.rowSubtitle(
                                    entry, calendar: calendar, locale: locale))
                                .font(.subheadline)
                                .lineLimit(1)
                        }
                        .foregroundStyle(tint)
                    }
                    Spacer(minLength: 8)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(
                CallHistory.rowAccessibilityLabel(
                    entry, title: title, calendar: calendar, locale: locale))

            Button {
                onCallBack(entry.conversationId, entry.kind, entry.video)
            } label: {
                Image(systemName: entry.video ? "video.fill" : "phone.fill")
                    .font(.body)
                    .foregroundStyle(Color.lakkoAccent)
                    .frame(width: 44, height: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(CallHistory.callBackAccessibilityLabel(entry))
        }
        .padding(.vertical, 4)
    }

    // MARK: - Empty

    private var emptyState: some View {
        ContentUnavailableView {
            Label("No calls yet", systemImage: "phone")
        } description: {
            Text("Calls you make and receive appear here.")
        } actions: {
            Button("Create call link") { createLinkPresented = true }
                .buttonStyle(.borderedProminent)
            Button("Join by link") { joinByLinkPresented = true }
        }
    }

    // MARK: - Join by link

    private var joinByLinkSheet: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Paste the call link", text: $pastedLink, axis: .vertical)
                        .lineLimit(2...4)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                } footer: {
                    Text("The person who created the call shares the link.")
                }
                if let joinError {
                    Section {
                        Text(joinError)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            .navigationTitle("Join by link")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button("Cancel") { dismissJoinSheet() }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Join") { join() }
                        .disabled(pastedLink.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }

    private func join() {
        let trimmed = pastedLink.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed),
              InviteIntake.slug(from: url.path, prefix: CallInviteUrl.urlPrefix) != nil
        else {
            joinError = "That does not look like a call link. Paste the whole link and try again."
            return
        }
        dismissJoinSheet()
        onOpenLink(url)
    }

    private func dismissJoinSheet() {
        joinByLinkPresented = false
        pastedLink = ""
        joinError = nil
    }
}
