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
    /// ADR-0058 — the title follows the interface language.
    @ObservedObject private var l10n = Localization.shared
    /// Start a group video call and land in it. Returns the one sentence to
    /// show when it could not start (another call holds the slot, or there is
    /// no session), `nil` when it did.
    let onNewCall: () async -> UiText?
    /// Open the conversation a row belongs to.
    let onOpenConversation: (_ groupIdHex: String, _ kind: CallHistoryKind) -> Void
    /// Call that conversation back, in the mode the row records.
    let onCallBack: (_ groupIdHex: String, _ kind: CallHistoryKind, _ video: Bool) -> Void
    /// Hand a pasted or minted `/c/` link to the same intake a tapped link
    /// uses, so joining by link and tapping a link are one path.
    let onOpenLink: (URL) -> Void

    @State private var joinByLinkPresented = false
    @State private var pastedLink = ""
    @State private var joinError: UiText?
    /// A call is being started. Brief — the press founds the call locally —
    /// but long enough that a second press must not found a second one.
    @State private var starting = false
    /// Kept as a message so an alert already up follows a language switch.
    @State private var startError: UiText?

    private let calendar = Calendar.current
    /// The interface language, so day headings and times follow it (ADR-0058).
    private var locale: Locale { l10n.locale }

    var body: some View {
        content
            .navigationTitle(l10n.t("nav.calls"))
            .navigationBarTitleDisplayMode(.large)
            .toolbar {
                ToolbarItemGroup(placement: .topBarTrailing) {
                    // Demoted, not removed: a pasted link is a real case and a
                    // rare one, so it sits behind the one menu this screen has
                    // rather than beside the action it is not.
                    Menu {
                        Button { joinByLinkPresented = true } label: {
                            Label(l10n.t("call.history.join_by_link"), systemImage: "link")
                        }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                    }
                    .accessibilityLabel(l10n.t("common.more_options"))

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
            .sheet(isPresented: $joinByLinkPresented) { joinByLinkSheet }
            .alert(
                l10n.t("call.alert.title"),
                isPresented: Binding(
                    get: { startError != nil }, set: { if !$0 { startError = nil } }),
                presenting: startError
            ) { _ in
                Button(l10n.t("common.ok"), role: .cancel) { startError = nil }
            } message: { message in
                Text(message.text)
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

    // MARK: - Join by link

    private var joinByLinkSheet: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(l10n.t("call.history.paste_label"), text: $pastedLink, axis: .vertical)
                        .lineLimit(2...4)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled(true)
                } footer: {
                    Text(l10n.t("call.history.paste_hint"))
                }
                if let joinError {
                    Section {
                        Text(joinError.text)
                            .font(.footnote)
                            .foregroundStyle(Color.lakkoDanger)
                    }
                }
            }
            .navigationTitle(l10n.t("call.history.join_by_link"))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(l10n.t("common.cancel")) { dismissJoinSheet() }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button(l10n.t("chat.call.join_short")) { join() }
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
            joinError = .key("call.history.join_invalid")
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
