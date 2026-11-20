// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
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
/// hides the full-screen call and shows ``FloatingCallWindow``, a small window
/// floating over whatever screen is showing. The flag lives in a
/// ``CallBarHost`` held here — the presentation mode is a property of the call,
/// so it belongs where the calls are and nowhere a route can reach it.
///
/// And it owns the system's picture-in-picture window (section 4.5.4,
/// ``CallPictureInPicture``), which opens by itself when the app leaves the
/// screen during a call, full screen or collapsed.
struct CallHostOverlay: ViewModifier {
    @ObservedObject var host: CallHost
    @ObservedObject var group: GroupCallHost
    @ObservedObject var link: LinkCallHost
    @ObservedObject var inviteIntake: InviteIntake
    @ObservedObject var l10n = Localization.shared
    /// Whether a call can be put away at all. A guest who joined from a link
    /// has no app to collapse into (section 4.5.3), so the guest root turns it
    /// off and the call screen offers neither the chevron nor the swipe.
    var allowsCollapse: Bool = true

    /// Collapsed / expanded, and when the live call connected. Lives as long as
    /// this modifier, which is as long as the signed-in session.
    @StateObject private var callBar = CallBarHost()
    /// A one-to-one call's session as a ``CallUiState``, the object the window
    /// and the system window read. Group and link calls already hold one.
    @StateObject private var oneToOneMirror = OneToOneCallMirror()
    /// The system's picture-in-picture window (section 4.5.4).
    @StateObject private var pip = CallPictureInPicture()

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

    /// Rendered at display time, so an alert already up follows a language
    /// switch (the hosts hold the message, not its text).
    private var errorMessage: String? {
        (host.callError ?? group.callError ?? group.callNotice)?.text
    }

    // MARK: - The collapsed window

    /// Which call the window is about. The order mirrors ``presented``'s, so
    /// the window and the screen are never about different calls; only one of
    /// the three can be live at a time anyway (``CallSlot``).
    private enum WindowSubject {
        /// A 1x1 call, read through ``oneToOneMirror``; mute goes to the
        /// session, whose state the mirror then follows.
        case oneToOne(CallSession, label: String)
        /// A group call or a call from a link, whose host already holds the
        /// state both the screen and the window read.
        case external(CallUiState, label: String)
    }

    private var windowSubject: WindowSubject? {
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

    /// The window, wired to the object that holds the live call's state, so
    /// pressing mute on the window redraws it and the window and the call
    /// screen read one value each rather than two copies.
    @ViewBuilder
    private var collapsedWindow: some View {
        switch windowSubject {
        case .oneToOne(let session, let label):
            FloatingCallWindow(
                state: oneToOneMirror.state,
                groupCall: false,
                label: label,
                phase: barPhase,
                connectedAt: callBar.connectedAt,
                peerKey: host.activePeerKey,
                onExpand: { callBar.expand() },
                onToggleMic: { session.setMicrophoneEnabled(!session.micEnabled) })
        case .external(let ui, let label):
            ExternalCallWindow(
                state: ui,
                label: label,
                // A group call rings nobody: while it starts, the window reads
                // `Connecting...`, never `Calling...` (section 3.4).
                phase: CallBarPresentation.groupDisplayPhase(barPhase),
                connectedAt: callBar.connectedAt,
                onExpand: { callBar.expand() })
        case nil:
            // Unreachable: the rule hides the window for `Idle`, which is the
            // phase a missing subject reports.
            EmptyView()
        }
    }

    /// Where the live call is, as the section-4.5 rule needs to see it.
    private var barPhase: CallBarPhase {
        switch windowSubject {
        case .oneToOne:
            return CallBarPresentation.phase(oneToOne: host.activeCallState)
        case .external(let ui, _):
            return CallBarPresentation.phase(external: ui.phase)
        case nil:
            return .idle
        }
    }

    /// Hand the live call to the system window: the state it reads, whether
    /// the call has a window at all, and who it is with.
    private func followPictureInPicture() {
        switch windowSubject {
        case .oneToOne(_, let label):
            pip.follow(
                state: oneToOneMirror.state, groupCall: false, label: label,
                peerKey: host.activePeerKey, live: CallWindowContent.hasWindow(barPhase))
        case .external(let ui, let label):
            pip.follow(
                state: ui, groupCall: true, label: label, peerKey: nil,
                live: CallWindowContent.hasWindow(barPhase))
        case nil:
            pip.follow(state: nil, groupCall: false, label: "", peerKey: nil, live: false)
        }
    }

    /// What the system window has to be told again when it changes.
    private var pipKey: String {
        let subject: String
        switch windowSubject {
        case .oneToOne(let session, let label):
            subject = "1:\(ObjectIdentifier(session).hashValue):\(label)"
        case .external(let ui, let label):
            subject = "g:\(ObjectIdentifier(ui).hashValue):\(label)"
        case nil:
            subject = "none"
        }
        return "\(subject):\(CallWindowContent.hasWindow(barPhase))"
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
            // The collapsed / expanded flag, for a screen under the overlay
            // that offers its own way back to the call (a guest's
            // `Return to call`, ``GuestRoot``).
            .environmentObject(callBar)
            // The view the system's picture-in-picture window opens out of:
            // clear, full size, behind everything.
            .background(PipSourceViewHost(pip: pip).allowsHitTesting(false))
            // The collapsed call: one small window floating over whatever
            // screen is showing (section 4.5). Never over the call screen,
            // which the rule's `callScreenUp` keeps it from.
            .overlay {
                if callBarVisible(phase: barPhase, callScreenUp: presentedNow != nil) {
                    collapsedWindow
                }
            }
            // A one-to-one call's session, mirrored for the window and the
            // system window. Followed whether or not the call is collapsed:
            // the system window opens from a full-screen call too.
            .task(id: host.activeCall.map(ObjectIdentifier.init)) {
                oneToOneMirror.follow(session: host.activeCall, peerName: host.activePeerName)
            }
            .onChange(of: host.activePeerName) { _, name in
                oneToOneMirror.follow(session: host.activeCall, peerName: name)
            }
            .task(id: pipKey) { followPictureInPicture() }
            .safeAreaInset(edge: .top, spacing: 0) {
                // Both can be up at once — a link call whose outcome has not
                // been dismissed leaves its notice while a NEW call of another
                // kind runs — so they stack rather than race for the slot. The
                // notice is above because it names an outcome and carries
                // actions; the live call itself floats in its window. For one
                // and the same link call they never overlap:
                // `LinkCallHostRules.notice` is silent exactly while the call
                // is live, which is the only time the window is up for it.
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
                    // The starter of the call this device was in ended it for
                    // everyone (section 4.1.2): one line saying so, for a few
                    // seconds, wherever the person is in the app.
                    if let ended = group.endedForEveryoneNotice ?? link.endedForEveryoneNotice {
                        CallEndedForEveryoneBanner(text: ended.text)
                    }
                }
            }
            // The running time and the collapse both outlive the call screen, so
            // the phase is folded here rather than inside it.
            .onChange(of: barPhase) { _, phase in
                if callBar.observe(phase: phase) { dismissEndedCollapsedCall() }
                // A choice of microphone, camera or speaker holds for the
                // call, not for the next one. Here rather than on the call
                // screen, which is not there for a call that ended while put
                // away.
                if phase == .ended || phase == .idle { CallDevices.resetForNextCall() }
            }
            .fullScreenCover(
                isPresented: Binding(
                    get: { presentedNow != nil },
                    set: { _ in }
                )
            ) {
                presentedCall
                    // While the call screen is up the system window opens out
                    // of it rather than out of the app hidden beneath.
                    .background(PipSourceViewHost(pip: pip).allowsHitTesting(false))
            }
            .lakkoDialog(
                isPresented: Binding(
                    get: { errorMessage != nil },
                    set: { presented in
                        if !presented { dismissErrors() }
                    }
                ),
                title: l10n.t("call.alert.title"),
                message: errorMessage
            ) {
                [.primary(l10n.t("common.ok")) { dismissErrors() }]
            }
    }

    /// The one full-screen presentation's content.
    @ViewBuilder
    private var presentedCall: some View {
        switch presentedNow {
        case .incoming?:
            // The same entry points the banner drove: the call plumbing
            // is untouched, only the surface changed. On a video call
            // Accept answers WITH video and the ghost button is the
            // audio-only choice (section 2.2), which is the one place
            // the banner had the emphasis backwards.
            IncomingCallScreen(
                callerName: host.activePeerName,
                callerKey: host.activePeerKey,
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
                peerKey: host.activePeerKey,
                notice: host.outgoingNotice,
                connectedAt: callBar.connectedAt,
                onCollapse: allowsCollapse ? { callBar.collapse() } : nil
            ) {
                host.clearFinishedCall()
            }
        case .group(let ui, let id)?:
            // `End` offers the choice of section 4.1.2 only to the
            // account that started the call.
            CallScreen(
                externalState: ui,
                connectedAt: callBar.connectedAt,
                endedLine: group.endedForEveryoneNotice?.text,
                onEndForEveryone: group.startedCall ? { group.endForEveryone() } : nil,
                onCollapse: allowsCollapse ? { callBar.collapse() } : nil,
                onOwnLink: { inviteIntake.handle($0) }
            ) {
                group.dismissPresentation()
            }
            .id("group-\(id)")
        case .link(let ui, let id)?:
            // `Invite` (`docs/ux/call-links.md` section 3) is offered
            // only by the device that founded the call, which is the
            // only one whose signature a joiner's committer election
            // would follow. `LinkCallHost` holds the offer for exactly
            // that case, so passing it through is the whole gate. It
            // opens the app's own menu on the call screen: Copy link,
            // and Invite contacts.
            CallScreen(
                externalState: ui,
                invite: link.invite,
                connectedAt: callBar.connectedAt,
                endedLine: link.endedForEveryoneNotice?.text,
                // The link's owner, and nobody else.
                onEndForEveryone: link.startedCall ? { link.endForEveryone() } : nil,
                onCollapse: allowsCollapse ? { callBar.collapse() } : nil,
                onOwnLink: { inviteIntake.handle($0) }
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
        inviteIntake: InviteIntake,
        allowsCollapse: Bool = true
    ) -> some View {
        modifier(CallHostOverlay(
            host: oneToOne, group: group, link: link, inviteIntake: inviteIntake,
            allowsCollapse: allowsCollapse))
    }
}

/// The collapsed window for a group call or a call from a link. The
/// ``CallUiState`` belongs to the host, so it is the same object the call
/// screen renders, and writing the local participant's `micOn` is the whole
/// mute control on this path, exactly as the call screen's own row does it.
private struct ExternalCallWindow: View {
    @ObservedObject var state: CallUiState
    let label: String
    let phase: CallBarPhase
    let connectedAt: Date?
    let onExpand: () -> Void

    var body: some View {
        FloatingCallWindow(
            state: state,
            groupCall: true,
            label: label,
            phase: phase,
            connectedAt: connectedAt,
            peerKey: nil,
            onExpand: onExpand,
            onToggleMic: {
                let micOn = state.participants.first(where: { $0.isLocal })?.micOn ?? true
                state.setMic(!micOn)
            })
    }
}

/// A one-to-one ``CallSession`` as a ``CallUiState``: the same mirror the call
/// screen builds for itself (``CallUiState/driveFrom(session:peerName:)``),
/// held at the host so the collapsed window and the system window can read the
/// call while the call screen is not there.
@MainActor
final class OneToOneCallMirror: ObservableObject {
    let state = CallUiState()
    private weak var session: CallSession?
    private var peerName: String?
    private var bindings: [AnyCancellable] = []

    /// Follow `session`, or none. Idempotent for the same session and name.
    func follow(session next: CallSession?, peerName name: String?) {
        guard next !== session || name != peerName else { return }
        bindings = []
        session = next
        peerName = name
        if let next {
            bindings = state.driveFrom(session: next, peerName: name)
        } else {
            state.reset()
        }
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
                LkIconView(.link, size: 20)
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
