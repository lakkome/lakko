// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import LakkoCore
import SwiftUI

/// Every call's app-wide presentation: the 1x1 ringing banner and a call link's
/// status line above whatever screen is showing (Home, the chat list, Contacts,
/// any chat), the ONE full-screen call — 1x1, group or call link — and the
/// calls' error alert.
///
/// Mounted once, at the signed-in root, so no call is owned by a screen the user
/// can leave. Everything it shows comes from ``CallHost``, ``GroupCallHost`` and
/// ``LinkCallHost``.
///
/// It also owns the one thing a call's presentation needs that no host does:
/// whether the call is **put away** (`docs/ux/calls.md` section 4.5). Collapsing
/// hides the full-screen call and shows ``CollapsedCallBar`` in the same top
/// inset the link notice uses, above whatever screen is showing. The flag lives
/// in a ``CallBarHost`` held here — the presentation mode is a property of the
/// call, so it belongs where the calls are and nowhere a route can reach it.
struct CallHostOverlay: ViewModifier {
    @ObservedObject var host: CallHost
    @ObservedObject var group: GroupCallHost
    @ObservedObject var link: LinkCallHost
    @ObservedObject var inviteIntake: InviteIntake
    @ObservedObject var l10n = Localization.shared

    /// Collapsed / expanded, and when the live call connected. Lives as long as
    /// this modifier, which is as long as the signed-in session.
    @StateObject private var callBar = CallBarHost()

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

    /// What the full-screen presentation shows right now, with a collapsed call
    /// taken away from it.
    ///
    /// Only the three live-call branches are suppressed. An incoming ring is
    /// never collapsed — ``CallBarPresentation/collapseSurvives(_:)`` clears the
    /// flag for `Ringing` — but the branch is written so that a stale flag could
    /// never swallow an offer, which would be a missed call.
    private var presentedNow: Presented? {
        guard callBar.collapsed else { return presented }
        if case .incoming? = presented { return presented }
        return nil
    }

    private var presented: Presented? {
        if IncomingCallPresentation.showsInAppScreen(
            state: host.activeCallState,
            ringingOnSystemUi: host.ringingOnSystemUi,
            // `docs/ux/calls.md` section 2.1.1 — a muted conversation's call is
            // answerable but never thrown across the screen. It is offered by
            // the panel at the top of its own chat and by its row in the
            // conversation list, both of which sit in the layout.
            quiet: host.incomingQuiet
        ) {
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

    // MARK: - The collapsed bar

    /// Which call the bar is about. The order mirrors ``presented``'s, so the
    /// bar and the screen are never about different calls; only one of the three
    /// can be live at a time anyway (``CallSlot``).
    private enum BarSubject {
        /// A 1x1 call. The session is observed by the bar itself, because mute
        /// and the camera are its own published state and pressing mute on the
        /// bar has to redraw it.
        case oneToOne(CallSession, label: String)
        /// A group call or a call from a link, whose host already holds the
        /// state both the screen and the bar read.
        case external(CallUiState, label: String)
    }

    private var barSubject: BarSubject? {
        if let session = host.activeCall {
            return .oneToOne(
                session,
                label: CallPeerName.callLabel(
                    profileName: host.activePeerName, groupCall: false))
        }
        if let ui = group.presentation ?? link.presentation {
            return .external(
                ui,
                label: CallPeerName.callLabel(profileName: ui.peerName, groupCall: true))
        }
        return nil
    }

    /// The bar, wired to whichever object holds the live call's mic, camera and
    /// encryption state — so pressing mute on the bar redraws it, and the bar
    /// and the call screen read one value each rather than two copies.
    @ViewBuilder
    private var collapsedBar: some View {
        switch barSubject {
        case .oneToOne(let session, let label):
            OneToOneCallBar(
                session: session,
                label: label,
                phase: barPhase,
                connectedAt: callBar.connectedAt,
                onExpand: { callBar.expand() })
        case .external(let ui, let label):
            ExternalCallBar(
                state: ui,
                label: label,
                phase: barPhase,
                connectedAt: callBar.connectedAt,
                onExpand: { callBar.expand() })
        case nil:
            // Unreachable: the rule hides the bar for `Idle`, which is the phase
            // a missing subject reports.
            EmptyView()
        }
    }

    /// Where the live call is, as the section-4.5 rule needs to see it.
    private var barPhase: CallBarPhase {
        switch barSubject {
        case .oneToOne:
            return CallBarPresentation.phase(oneToOne: host.activeCallState)
        case .external(let ui, _):
            return CallBarPresentation.phase(external: ui.phase)
        case nil:
            return .idle
        }
    }

    /// A collapsed call has ended. Its screen was never drawn, so nothing will
    /// dismiss the `Call ended` overlay on its behalf — drop the presentation
    /// the same way that overlay's own Close does, or the host holds a finished
    /// call for the rest of the session.
    private func dismissEndedCollapsedCall() {
        if host.activeCall != nil {
            host.clearFinishedCall()
        } else if group.presentation != nil {
            group.dismissPresentation()
        } else if link.presentation != nil {
            link.dismissPresentation()
        }
    }

    func body(content: Content) -> some View {
        content
            .safeAreaInset(edge: .top, spacing: 0) {
                // Both can be up at once — a link call whose outcome has not
                // been dismissed leaves its notice while a NEW call of another
                // kind runs — so they stack rather than race for the slot. The
                // notice is above because it names an outcome and carries
                // actions; the bar below is the live call, one tap from coming
                // back. For one and the same link call they never overlap:
                // `LinkCallHostRules.notice` is silent exactly while the call is
                // live, which is the only time the bar is up for it.
                VStack(spacing: 0) {
                    if let notice = link.notice {
                        LinkCallNoticeBanner(
                            notice: notice,
                            // A call started here has no link and so no landing
                            // to go back to; Open is hidden rather than dead.
                            onOpen: link.slot?.slug.map { slug in
                                { inviteIntake.parkCallJoin(slug: slug) }
                            },
                            onCancel: { link.leave() },
                            onDismiss: { link.dismissOutcome() }
                        )
                    }
                    if callBarVisible(phase: barPhase, callScreenUp: presentedNow != nil) {
                        collapsedBar
                    }
                }
            }
            // The running time and the collapse both outlive the call screen, so
            // the phase is folded here rather than inside it.
            .onChange(of: barPhase) { _, phase in
                if callBar.observe(phase: phase) { dismissEndedCollapsedCall() }
            }
            .fullScreenCover(
                isPresented: Binding(
                    get: { presentedNow != nil },
                    set: { _ in }
                )
            ) {
                switch presentedNow {
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
                    // `docs/ux/calls.md` sections 3 and 4 — the call is
                    // already ringing out when this opens, for both kinds, and
                    // the resolved name is who the screen says is on it.
                    CallScreen(
                        session: session,
                        peerName: host.activePeerName,
                        notice: host.outgoingNotice,
                        connectedAt: callBar.connectedAt,
                        onCollapse: { callBar.collapse() }
                    ) {
                        host.clearFinishedCall()
                    }
                case .group(let ui, let id)?:
                    CallScreen(
                        externalState: ui,
                        connectedAt: callBar.connectedAt,
                        onCollapse: { callBar.collapse() }
                    ) {
                        group.dismissPresentation()
                    }
                    .id("group-\(id)")
                case .link(let ui, let id)?:
                    // `Invite` (`docs/ux/call-links.md` section 3) is offered
                    // only by the device that founded the call, which is the
                    // only one whose signature a joiner's committer election
                    // would follow. `LinkCallHost` holds the offer for exactly
                    // that case, so passing it through is the whole gate.
                    CallScreen(
                        externalState: ui,
                        invite: link.invite,
                        connectedAt: callBar.connectedAt,
                        onCollapse: { callBar.collapse() }
                    ) {
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
                l10n.t("call.alert.title"),
                isPresented: Binding(
                    get: { errorMessage != nil },
                    set: { presented in
                        if !presented { dismissErrors() }
                    }
                ),
                presenting: errorMessage
            ) { _ in
                Button(l10n.t("common.ok"), role: .cancel) { dismissErrors() }
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

/// The collapsed bar for a 1x1 call. A view of its own so the ``CallSession``
/// can be observed: mute, the camera and the E2EE arming are its published
/// state, and the bar both shows and toggles the first of them.
private struct OneToOneCallBar: View {
    @ObservedObject var session: CallSession
    let label: String
    let phase: CallBarPhase
    let connectedAt: Date?
    let onExpand: () -> Void

    var body: some View {
        CollapsedCallBar(
            label: label,
            phase: phase,
            connectedAt: connectedAt,
            // Exactly what decides the call screen's chip, read from the same
            // place the screen reads it through `driveFrom`.
            encrypted: session.e2eeActive,
            cameraSending: session.cameraEnabled,
            micOn: session.micEnabled,
            onExpand: onExpand,
            onToggleMic: { session.setMicrophoneEnabled(!session.micEnabled) })
    }
}

/// The collapsed bar for a group call or a call from a link. The ``CallUiState``
/// belongs to the host, so it is the same object the call screen renders — and
/// writing the local participant's `micOn` is the whole mute control on this
/// path, exactly as the call screen's own row does it.
private struct ExternalCallBar: View {
    @ObservedObject var state: CallUiState
    let label: String
    let phase: CallBarPhase
    let connectedAt: Date?
    let onExpand: () -> Void

    private var local: CallUiState.Participant? {
        state.participants.first(where: { $0.isLocal })
    }

    var body: some View {
        let micOn = local?.micOn ?? true
        CollapsedCallBar(
            label: label,
            phase: phase,
            connectedAt: connectedAt,
            encrypted: state.e2ee.active,
            cameraSending: local?.camOn ?? false,
            micOn: micOn,
            onExpand: onExpand,
            onToggleMic: { state.setMic(!micOn) })
    }
}

/// A call link's status away from its landing: still connecting / waiting for
/// admission (Open / Cancel), or how it ended (Dismiss). Same shape as the
/// ringing banner above.
private struct LinkCallNoticeBanner: View {
    @ObservedObject var l10n = Localization.shared
    let notice: LinkCallHostRules.Notice
    /// `nil` when there is no landing to return to.
    let onOpen: (() -> Void)?
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
                if notice.actions.contains(.open), let onOpen {
                    Button(l10n.t("common.open"), action: onOpen)
                        .buttonStyle(.borderedProminent)
                        .tint(Color.lakkoAccent)
                }
                Spacer()
                if notice.actions.contains(.cancel) {
                    Button(l10n.t("common.cancel"), role: .destructive, action: onCancel)
                        .buttonStyle(.bordered)
                        .tint(Color.lakkoDanger)
                }
                if notice.actions.contains(.dismiss) {
                    Button(l10n.t("common.dismiss"), action: onDismiss)
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
        .accessibilityLabel(l10n.t("call.link.notice.a11y"))
    }
}
