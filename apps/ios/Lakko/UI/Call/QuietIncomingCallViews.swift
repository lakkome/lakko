// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The two quiet surfaces a muted conversation's call is offered on
/// (`docs/ux/calls.md` section 2.1.1): a panel at the top of that conversation,
/// and Accept / Decline on that conversation's row in the list.
///
/// They live in one file because they are two views of one offer
/// (``QuietIncomingCall``) with one pair of words, and splitting them is how the
/// two would come to say different things. Neither holds a rule: whether to draw
/// at all is ``QuietIncomingCallPresentation``, and both buttons drive the same
/// ``CallHost`` entry points the full-screen incoming call and the phone's own
/// call screen drive — there is one accept path and one decline path on this
/// client.
///
/// **Nothing here covers anything.** No sheet, no alert, no `fullScreenCover`,
/// no overlay: the panel is an ordinary child of the chat's own `VStack`, in the
/// slot above the message list, and the row's buttons are a second line inside
/// the list cell. That is the whole point of section 2.1.1 — muting took away
/// the interruption, so an offer that displaced what the person was reading
/// would put it straight back.

/// The panel at the top of the muted conversation's chat.
///
/// Drawn on the accent-soft ground rather than the green of the group-call
/// banner next to it: green is the colour of a call that is already happening
/// and can be joined, and this is an offer waiting for an answer. It is also
/// deliberately quieter than either — a muted conversation's call should read
/// as a notice with buttons, not as an alarm.
struct QuietIncomingCallPanel: View {
    /// Re-renders the words on a language switch (ADR-0061).
    @ObservedObject var l10n = Localization.shared
    let call: QuietIncomingCall
    let onAccept: () -> Void
    let onAcceptWithoutVideo: () -> Void
    let onDecline: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                InitialsAvatar(
                    name: call.callerName ?? "", identity: call.callerName ?? "lakko.call", size: 36,
                    placeholderSymbol: "person.fill", photo: call.callerKey.map(AvatarPhoto.person))
                Image(systemName: call.video ? "video.fill" : "phone.fill")
                    .foregroundStyle(Color.lakkoAccent)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(call.displayName)
                        .font(.headline)
                        .foregroundStyle(Color.lakkoText)
                        .lineLimit(1)
                    Text(call.kindLabel)
                        .font(.caption)
                        .foregroundStyle(Color.lakkoTextMuted)
                }
                Spacer(minLength: 8)
            }
            HStack(spacing: 8) {
                Button(QuietIncomingCallPresentation.declineLabel, action: onDecline)
                    .buttonStyle(.bordered)
                    .tint(Color.lakkoDanger)
                Spacer(minLength: 8)
                // Section 2.2's third choice, on a video call only. The panel
                // has room for it where the list row does not, and the spec's
                // emphasis is kept: the primary Accept answers a video call
                // WITH video, and answering without it is the secondary word.
                if call.video {
                    Button(action: onAcceptWithoutVideo) {
                        Text(QuietIncomingCallPresentation.acceptWithoutVideoLabel)
                            .font(.subheadline)
                            .foregroundStyle(Color.lakkoAccent)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 8)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                Button(QuietIncomingCallPresentation.acceptLabel, action: onAccept)
                    .buttonStyle(.borderedProminent)
                    .tint(Color.lakkoAccent)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoAccentSoft)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(QuietIncomingCallPresentation.spokenSummary(call))
    }
}

/// Accept and Decline on the conversation's own row in the list.
///
/// A second line inside the cell, NOT inside the row's own tap target: the row
/// is a `Button` that opens the conversation, and a button nested in another
/// button's label does not reliably get the tap. So this sits beside it, which
/// also keeps opening the chat working while the call is on offer.
///
/// Only the two words the spec names. Accepting from here takes the call as it
/// was offered — a video call answers with video — because the row has no space
/// for a third choice and the chat's own panel is one tap away for anyone who
/// wants it.
struct QuietIncomingCallRowActions: View {
    /// Re-renders the words on a language switch (ADR-0061).
    @ObservedObject var l10n = Localization.shared
    let call: QuietIncomingCall
    let onAccept: () -> Void
    let onDecline: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Text(call.kindLabel)
                .font(.caption)
                .foregroundStyle(Color.lakkoAccent)
                .lineLimit(1)
            Spacer(minLength: 8)
            Button(QuietIncomingCallPresentation.declineLabel, action: onDecline)
                .buttonStyle(.bordered)
                .tint(Color.lakkoDanger)
                .controlSize(.small)
            Button(QuietIncomingCallPresentation.acceptLabel, action: onAccept)
                .buttonStyle(.borderedProminent)
                .tint(Color.lakkoAccent)
                .controlSize(.small)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(QuietIncomingCallPresentation.spokenSummary(call))
    }
}
