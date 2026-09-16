// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// Every call's app-wide presentation: the 1x1 ringing banner and a call link's
/// status line above whatever screen is showing (Home, the chat list, Contacts,
/// any chat), the ONE full-screen call — 1x1, group or call link — and the
/// calls' error alert.
///
/// Mounted once, at the signed-in root, so no call is owned by a screen the user
/// can leave. Everything it shows comes from ``CallHost``, ``GroupCallHost`` and
/// ``LinkCallHost``.
struct CallHostOverlay: ViewModifier {
    @ObservedObject var host: CallHost
    @ObservedObject var group: GroupCallHost
    @ObservedObject var link: LinkCallHost
    @ObservedObject var inviteIntake: InviteIntake

    /// The call on the full-screen presentation. The kinds are exclusive while
    /// live (``CallSlot``); a 1x1 call first only makes a stale screen of
    /// another kind give way deterministically.
    private enum Presented {
        case oneToOne(CallSession)
        case group(CallUiState, id: String)
        case link(CallUiState, id: String)
    }

    private var presented: Presented? {
        if Self.presentsCallScreen(host.activeCallState), let session = host.activeCall {
            return .oneToOne(session)
        }
        if let ui = group.presentation, let id = group.presentationId {
            return .group(ui, id: id)
        }
        if let ui = link.presentation, let id = link.presentationId {
            return .link(ui, id: id)
        }
        return nil
    }

    private var errorMessage: String? {
        host.callError ?? group.callError ?? group.callNotice
    }

    func body(content: Content) -> some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) {
                if host.activeCallState == .ringing {
                    IncomingCallBanner(
                        callerName: host.activePeerName,
                        video: host.incomingVideo,
                        onAccept: { host.acceptIncomingCall(withVideo: false) },
                        onAcceptWithVideo: { host.acceptIncomingCall(withVideo: true) },
                        onDecline: { host.declineIncomingCall() }
                    )
                } else if let notice = link.notice {
                    LinkCallNoticeBanner(
                        notice: notice,
                        onOpen: {
                            if let slug = link.slot?.slug { inviteIntake.parkCallJoin(slug: slug) }
                        },
                        onCancel: { link.leave() },
                        onDismiss: { link.dismissOutcome() }
                    )
                }
            }
            .fullScreenCover(
                isPresented: Binding(
                    get: { presented != nil },
                    set: { _ in }
                )
            ) {
                switch presented {
                case .oneToOne(let session)?:
                    CallScreen(session: session) {
                        host.clearFinishedCall()
                    }
                case .group(let ui, let id)?:
                    CallScreen(externalState: ui) {
                        group.dismissPresentation()
                    }
                    .id("group-\(id)")
                case .link(let ui, let id)?:
                    CallScreen(externalState: ui) {
                        let slug = link.presentationSlug
                        link.dismissPresentation()
                        // Back to where the user was before the link, as the
                        // landing's own Close does.
                        if let slug, inviteIntake.pendingCallJoinSlug == slug {
                            inviteIntake.clearCallJoin()
                        }
                    }
                    .id("link-\(id)")
                case nil:
                    EmptyView()
                }
            }
            .alert(
                "Call",
                isPresented: Binding(
                    get: { errorMessage != nil },
                    set: { presented in
                        if !presented { dismissErrors() }
                    }
                ),
                presenting: errorMessage
            ) { _ in
                Button("OK", role: .cancel) { dismissErrors() }
            } message: { message in
                Text(message)
            }
    }

    private func dismissErrors() {
        host.dismissCallError()
        group.dismissCallError()
        group.dismissCallNotice()
    }

    /// The full call screen owns everything from the prejoin of an outgoing
    /// call, through an accepted incoming one, to the terminal "Call ended"
    /// overlay. A ringing call is the banner; no call is nothing.
    static func presentsCallScreen(_ state: CallSession.WebCallState?) -> Bool {
        switch state {
        case .idle?, .outgoing?, .connecting?, .connected?, .ending?, .done?:
            return true
        case .ringing?, nil:
            return false
        }
    }
}

extension View {
    func callHostOverlay(
        oneToOne: CallHost,
        group: GroupCallHost,
        link: LinkCallHost,
        inviteIntake: InviteIntake
    ) -> some View {
        modifier(CallHostOverlay(host: oneToOne, group: group, link: link, inviteIntake: inviteIntake))
    }
}

/// The ringing banner: who is calling, and Accept / Video / Decline.
private struct IncomingCallBanner: View {
    let callerName: String?
    let video: Bool
    let onAccept: () -> Void
    let onAcceptWithVideo: () -> Void
    let onDecline: () -> Void

    private var headline: String {
        let what = video ? "video call" : "call"
        if let callerName { return "\(callerName) — incoming \(what)" }
        return "Incoming \(what)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(headline)
                    .font(.headline)
                    .foregroundStyle(.white)
                    .lineLimit(2)
                Text("End-to-end encrypted")
                    .font(.caption2)
                    .foregroundStyle(.white.opacity(0.85))
            }
            .accessibilityElement(children: .combine)
            HStack(spacing: 8) {
                Button("Accept", action: onAccept)
                    .buttonStyle(.borderedProminent)
                    .tint(.white)
                    .foregroundStyle(.green)
                    .accessibilityLabel("Accept with audio")
                Button(action: onAcceptWithVideo) {
                    Label("Video", systemImage: "video.fill")
                }
                .buttonStyle(.bordered)
                .tint(.white)
                .accessibilityLabel("Accept with video")
                Spacer()
                Button("Decline", role: .destructive, action: onDecline)
                    .buttonStyle(.bordered)
                    .tint(.white)
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.orange)
    }
}

/// A call link's status away from its landing: still connecting / waiting for
/// admission (Open / Cancel), or how it ended (Dismiss). Same shape as the
/// ringing banner above.
private struct LinkCallNoticeBanner: View {
    let notice: LinkCallHostRules.Notice
    let onOpen: () -> Void
    let onCancel: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(notice.text)
                .font(.headline)
                .foregroundStyle(.white)
                .lineLimit(3)
            HStack(spacing: 8) {
                if notice.actions.contains(.open) {
                    Button("Open", action: onOpen)
                        .buttonStyle(.borderedProminent)
                        .tint(.white)
                        .foregroundStyle(Color.lakkoAccent)
                }
                Spacer()
                if notice.actions.contains(.cancel) {
                    Button("Cancel", role: .destructive, action: onCancel)
                        .buttonStyle(.bordered)
                        .tint(.white)
                }
                if notice.actions.contains(.dismiss) {
                    Button("Dismiss", action: onDismiss)
                        .buttonStyle(.bordered)
                        .tint(.white)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoAccent)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Call from a link")
    }
}
