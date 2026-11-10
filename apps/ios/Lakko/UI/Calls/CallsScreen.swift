// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The Calls destination (`docs/ux/navigation.md` section 5, and
/// `docs/ux/call-links.md` section 2): recent calls, newest first, flat — not
/// grouped by conversation — with day separators, and ONE action.
///
/// ## What `New call` replaced
///
/// `Create call link`, a form that asked four questions — audio or video, when
/// the link expires, how many uses, what to label it — and produced a string of
/// text. **You could not place a call from the screen named after calling.**
///
/// `New call` starts a group video call and shows the call screen. Nothing is
/// asked first: no media choice, no participants, no name. A person who wanted
/// to call one specific contact has a faster route from the conversation or the
/// contact, which is why this button does not try to be that route. The link
/// comes later, from inside the call, for someone who is in neither place —
/// see `Invite` on ``CallScreen``.
///
/// `Join by link` stays, for a link someone has sent that did not open by
/// itself. It is not the main action and is not given the weight of one.
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
    /// ADR-0061 — the title follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// Start a group video call and land in it. Returns the one sentence to
    /// show when it could not start (another call holds the slot, or there is
    /// no session), `nil` when it did.
    let onNewCall: () async -> UiText?
    /// Open the conversation a row belongs to.
    let onOpenConversation: (_ groupIdHex: String, _ kind: CallHistoryKind) -> Void
    /// Call that conversation back, in the mode the row records.
    let onCallBack: (_ groupIdHex: String, _ kind: CallHistoryKind, _ video: Bool) -> Void
    /// A call is being started. Brief — the press founds the call locally —
    /// but long enough that a second press must not found a second one.
    @State private var starting = false
    /// Kept as a message so an alert already up follows a language switch.
    @State private var startError: UiText?

    private let calendar = Calendar.current
    /// The interface language, so day headings and times follow it (ADR-0061).
    private var locale: Locale { l10n.locale }

    var body: some View {
        content
            .navigationTitle(l10n.t("nav.calls"))
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    Button(action: startCall) {
                        if starting {
                            ProgressView()
                        } else {
                            Text(l10n.t("call.history.new_call"))
                        }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(starting)
                    .accessibilityLabel(l10n.t("call.history.new_call"))
                }
            }
            .lakkoDialog(item: $startError) { message in
                LakkoDialogContent(
                    title: l10n.t("call.alert.title"),
                    message: message.text,
                    actions: [.primary(l10n.t("common.ok")) { startError = nil }])
            }
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
            // Rebuilt on a layout-direction flip: a UIKit-backed List keeps the
            // mirroring it was built with (see SettingsScreen).
            .id(l10n.layoutDirection)
        }
    }

    // MARK: - Row

    /// A conversation's call: avatar and title as the conversation list draws
    /// them, the outcome and the time on the second line, a direction glyph at
    /// the leading edge, and a call-back button on the trailing edge. A call
    /// with no conversation has the first three and neither target.
    ///
    /// The two buttons are siblings rather than one nested in the other:
    /// SwiftUI would let a button inside a button take the whole row's taps,
    /// and the spec is explicit that these are two targets.
    @ViewBuilder
    private func row(_ entry: CallHistoryEntry) -> some View {
        switch CallHistory.rowShape(entry) {
        case .conversation(let groupIdHex, let kind):
            conversationRow(entry, groupIdHex: groupIdHex, kind: kind)
        case .noConversation(let title):
            noConversationRow(entry, title: title)
        }
    }

    /// The avatar, the title and the second line, shared by both row kinds.
    private func rowBody(
        _ entry: CallHistoryEntry, title: String, avatar: InitialsAvatar
    ) -> some View {
        let tint = entry.isUnanswered ? Color.lakkoDanger : Color.lakkoTextMuted
        return HStack(spacing: 12) {
            avatar
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
    }

    /// A call with no conversation (`docs/ux/navigation.md` section 5.1.1):
    /// the group-call glyph, never initials; no call-back button; and not a
    /// tap target. VoiceOver reads it as one element and offers no action.
    private func noConversationRow(_ entry: CallHistoryEntry, title: String) -> some View {
        rowBody(
            entry, title: title,
            avatar: InitialsAvatar(
                name: "", identity: entry.callIdHex ?? entry.id, size: 40, isGroup: true))
            .padding(.vertical, 4)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(
                CallHistory.rowAccessibilityLabel(
                    entry, title: title, calendar: calendar, locale: locale))
    }

    private func conversationRow(
        _ entry: CallHistoryEntry, groupIdHex: String, kind: CallHistoryKind
    ) -> some View {
        let title = conversations.title(groupIdHex: groupIdHex)
        return HStack(spacing: 12) {
            Button {
                onOpenConversation(groupIdHex, kind)
            } label: {
                rowBody(
                    entry, title: title,
                    avatar: InitialsAvatar(
                        name: title, identity: groupIdHex, size: 40,
                        isGroup: kind == .group,
                        photo: conversations.avatarPhoto(
                            groupIdHex: groupIdHex, isGroup: kind == .group)))
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(
                CallHistory.rowAccessibilityLabel(
                    entry, title: title, calendar: calendar, locale: locale))

            Button {
                onCallBack(groupIdHex, kind, entry.video)
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
            Label(l10n.t("call.history.empty"), systemImage: "phone")
        } description: {
            Text(l10n.t("call.history.empty_body"))
        } actions: {
            Button(l10n.t("call.history.new_call"), action: startCall)
                .buttonStyle(.borderedProminent)
                .disabled(starting)
        }
    }

    // MARK: - New call

    /// One press, one kind of call, and the person lands in it. The busy latch
    /// is set synchronously, before any await, so a double tap cannot found
    /// two calls.
    private func startCall() {
        guard !starting else { return }
        starting = true
        Task { @MainActor in
            let failure = await onNewCall()
            starting = false
            startError = failure
        }
    }
}
