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
        /// `docs/ux/calls.md` section 2.2 — the in-app incoming call. It shares
        /// the one full-screen presentation with the live call rather than
        /// having its own, because SwiftUI allows a view only one
        /// `fullScreenCover` and the two states are mutually exclusive anyway:
        /// `presentsCallScreen` excludes `.ringing` by construction.
        case incoming
        case oneToOne(CallSession)
        case group(CallUiState, id: String)
        case link(CallUiState, id: String)
    }

    private var presented: Presented? {
        if IncomingCallPresentation.showsInAppScreen(
            state: host.activeCallState, ringingOnSystemUi: host.ringingOnSystemUi) {
            return .incoming
        }
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
                if let notice = link.notice {
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
                case .incoming?:
                    // The same entry points the banner drove: the call plumbing
                    // is untouched, only the surface changed. On a video call
                    // Accept answers WITH video and the ghost button is the
                    // audio-only choice (section 2.2), which is the one place
                    // the banner had the emphasis backwards.
                    IncomingCallScreen(
                        callerName: host.activePeerName,
                        video: host.incomingVideo,
                        onAccept: { host.acceptIncomingCall(withVideo: host.incomingVideo) },
                        onAcceptWithoutVideo: { host.acceptIncomingCall(withVideo: false) },
                        onDecline: { host.declineIncomingCall() })
                case .oneToOne(let session)?:
                    // `docs/ux/calls.md` sections 3 and 4 — the button the
                    // caller tapped decides whether this opens on the camera
                    // check or on a call already ringing out, and the resolved
                    // name is who the screen says is on it.
                    CallScreen(
                        session: session,
                        surface: host.outgoingSurface,
                        peerName: host.activePeerName,
                        notice: host.outgoingNotice,
                        onPlaceCall: { micOn, cameraOn in
                            host.placeCheckedCall(micOn: micOn, cameraOn: cameraOn)
                        }
                    ) {
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

    /// The full call screen owns everything from the camera check of an
    /// outgoing video call, through a voice call already ringing out and an
    /// accepted incoming one, to the terminal "Call ended" overlay. A ringing
    /// call is the banner; no call is nothing.
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

/// A call link's status away from its landing: still connecting / waiting for
/// admission (Open / Cancel), or how it ended (Dismiss). Same shape as the
/// ringing banner above.
private struct LinkCallNoticeBanner: View {
    let notice: LinkCallHostRules.Notice
    let onOpen: () -> Void
    let onCancel: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "link")
                    .foregroundStyle(Color.lakkoAccent)
                    .padding(.top, 2)
                Text(notice.text)
                    .font(.headline)
                    .foregroundStyle(Color.lakkoText)
                    .lineLimit(3)
            }
            HStack(spacing: 8) {
                if notice.actions.contains(.open) {
                    Button("Open", action: onOpen)
                        .buttonStyle(.borderedProminent)
                        .tint(Color.lakkoAccent)
                }
                Spacer()
                if notice.actions.contains(.cancel) {
                    Button("Cancel", role: .destructive, action: onCancel)
                        .buttonStyle(.bordered)
                        .tint(Color.lakkoDanger)
                }
                if notice.actions.contains(.dismiss) {
                    Button("Dismiss", action: onDismiss)
                        .buttonStyle(.bordered)
                        .tint(Color.lakkoAccent)
                }
            }
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lakkoSurface)
        .overlay(alignment: .bottom) {
            Rectangle().fill(Color.lakkoBorder).frame(height: 1)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Call from a link")
    }
}
