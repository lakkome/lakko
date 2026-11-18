// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// The in-app incoming call (`docs/ux/calls.md` section 2.2).
///
/// ## What this replaces
///
/// A strip above the chat list: an icon, two lines of text, and three small
/// bordered pills reading Accept / Video / Decline. A call is an interruption
/// and is allowed to be one (section 1) — answering one from a banner at the
/// top of a list is the shape of a notification, not of a phone ringing, and it
/// left iOS the only client without the full-screen screen the spec describes.
///
/// ## The shape, and one correction it makes to the banner
///
/// Section 2.2: the caller's avatar at 96, their name, one line saying `Voice
/// call` or `Video call`, a small lock with `End-to-end encrypted`, and two
/// large round buttons — Decline and Accept. Nothing else: no code, no digits,
/// no protocol words.
///
/// A video call adds **Accept without video** as a ghost button between them,
/// which is a different arrangement from the banner's three equal pills: on a
/// video call the primary Accept answers **with** video, and answering without
/// it is the secondary choice. The banner had it the other way round, with
/// Accept meaning audio and video demoted to a pill of the same size.
///
/// It is drawn on the same ground as the in-call screen — the blurred,
/// darkened backdrop built from the caller's own colour, the same avatar, the
/// same round control faces — so the two read as one surface across the answer.
struct IncomingCallScreen: View {
    /// Re-renders the screen's words on a language switch (ADR-0061).
    @ObservedObject var l10n = Localization.shared
    /// The caller, already resolved through ``CallPeerName`` by the call host.
    let callerName: String?
    /// ADR-0084 — the caller's signature key, for their profile photo.
    var callerKey: Data? = nil
    /// The offer's own media flag, so the word matches what the caller chose.
    let video: Bool
    let onAccept: () -> Void
    let onAcceptWithoutVideo: () -> Void
    let onDecline: () -> Void

    /// The ring. One controller for the screen's lifetime; its `deinit` stops
    /// the players.
    @StateObject private var sounds = CallSoundsObservable()

    private var displayName: String {
        CallPeerName.resolve(profileName: callerName, chatTitle: nil)
            ?? CallPeerName.placeholder
    }

    /// What the callee is told, derived from the offer's flags so it cannot
    /// disagree with them (`docs/ux/calls.md` section 3).
    private var kindLabel: String { CallMediaChoice.kindLabel(video: video) }

    var body: some View {
        ZStack {
            CallBackdrop(identity: callerName ?? "lakko.call")

            VStack(spacing: 0) {
                Spacer(minLength: 0)

                VStack(spacing: 20) {
                    InitialsAvatar(
                        name: callerName ?? "", identity: callerName ?? "lakko.call",
                        size: 96, placeholderIcon: .personFill,
                        photo: callerKey.map(AvatarPhoto.person))

                    VStack(spacing: 8) {
                        Text(displayName)
                            .font(.system(size: 30, weight: .semibold))
                            .foregroundStyle(.white)
                            .lineLimit(1)
                            .minimumScaleFactor(0.6)
                        Text(kindLabel)
                            .font(.callout)
                            .foregroundStyle(Color.white.opacity(0.7))
                        encryptedLine
                    }
                }
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
                .accessibilityElement(children: .combine)
                .accessibilityLabel(Localization.joinList([
                    l10n.t(
                        video ? "call.incoming.video_a11y" : "call.incoming.voice_a11y",
                        texts: ["name": displayName]),
                    l10n.t("call.e2ee"),
                ]))

                Spacer(minLength: 0)
                Spacer(minLength: 0)

                controls
                    .padding(.bottom, 48)
            }
        }
        .preferredColorScheme(.dark)
        .onAppear {
            // The ring. `ringtone-incoming` shipped with the sound controller
            // and had no caller anywhere on iOS, so the in-app incoming call has
            // been silent for its whole life — the same defect the web client
            // was found to have. A phone that does not ring is not a call.
            sounds.controller.startLoop(.ringtoneIncoming)
            vibrate()
        }
        .onDisappear { sounds.controller.stopAll() }
    }

    /// Section 2.2 — a small lock and one word. The 1x1 native path refuses to
    /// connect unless its media keying is armed, so this is true of any call
    /// that can reach this screen.
    private var encryptedLine: some View {
        HStack(spacing: 6) {
            LkIconView(.lockFill, size: 14, relativeTo: .caption2)
            Text(l10n.t("call.e2ee"))
                .font(.caption)
        }
        .foregroundStyle(Color.white.opacity(0.6))
        .accessibilityHidden(true)
    }

    /// Decline and Accept, with the ghost "Accept without video" between them on
    /// a video call.
    private var controls: some View {
        VStack(spacing: 24) {
            if video {
                Button(action: onAcceptWithoutVideo) {
                    Text(l10n.t("call.incoming.accept_without_video"))
                        .font(.subheadline)
                        .foregroundStyle(Color.white.opacity(0.85))
                        .padding(.horizontal, 18)
                        .padding(.vertical, 10)
                        .background(Capsule().fill(Color.white.opacity(0.12)))
                }
                .buttonStyle(.plain)
                .accessibilityLabel(l10n.t(
                    "call.incoming.accept_without_video_a11y", texts: ["name": displayName]))
            }

            HStack(spacing: 0) {
                answerButton(
                    label: l10n.t("call.incoming.decline"),
                    icon: .phoneDownFill,
                    tint: .lakkoDanger,
                    spoken: l10n.t(
                        video ? "call.incoming.decline_video_a11y" : "call.incoming.decline_voice_a11y",
                        texts: ["name": displayName]),
                    action: onDecline)
                Spacer(minLength: 32)
                answerButton(
                    label: l10n.t("call.incoming.accept"),
                    icon: video ? .videoFill : .phoneFill,
                    tint: .lakkoSuccess,
                    spoken: l10n.t(
                        video ? "call.incoming.accept_video_a11y" : "call.incoming.accept_voice_a11y",
                        texts: ["name": displayName]),
                    action: onAccept)
            }
            .frame(maxWidth: 320)
        }
    }

    /// A large round button with its word beneath, the same face the in-call
    /// row uses so the surface does not change shape when the call is answered.
    private func answerButton(
        label: String, icon: LkIcon, tint: Color, spoken: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                ZStack {
                    Circle().fill(tint)
                    // The same proportion as the in-call end button's glyph.
                    LkIconView(icon, size: 32, relativeTo: nil)
                        .foregroundStyle(.white)
                }
                .frame(width: 72, height: 72)
                Text(label)
                    .font(.caption)
                    .foregroundStyle(Color.white.opacity(0.75))
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel(spoken)
    }

    private func vibrate() {
        UINotificationFeedbackGenerator().notificationOccurred(.warning)
    }
}
