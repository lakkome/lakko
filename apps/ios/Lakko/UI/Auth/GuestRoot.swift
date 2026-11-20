// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// What a guest account sees in place of the app (`docs/ux/call-links.md`
/// section 9). Mirror of the web `guest` guard view and `GuestAccountScreen`.
///
/// A guest account has no conversations and no contacts, so the tab shell
/// behind a session is an empty one for it; a guest never lands there. The
/// app root shows this instead:
///
///   * the call link the guest opened, while it is parked: the landing, which
///     shows the call's state and, whenever the guest is not in the call (it
///     ended, it is dead, the join failed, the app was reopened), the
///     question;
///   * otherwise, while any call runs, the way back to it;
///   * otherwise the question: `Create account` or `No, thanks`.
///
/// The form behind `Create account` is always the whole screen.
///
/// None of the session runtimes the tab shell starts runs here: no chat
/// bootstrap, no KeyPackage publish, no mailbox claim, no DeviceGroup, no
/// automatic backup, no inbox poll. A call from a link needs the session and
/// the own profile name, and nothing else, so nothing else is kept on a
/// server for an account the person has not decided to keep. They start when
/// the account is kept and the tab shell mounts.
struct GuestRoot: View {
    let account: SessionAccount
    let services: AppServices

    @ObservedObject private var inviteIntake: InviteIntake
    @ObservedObject private var oneToOne: CallHost
    @ObservedObject private var group: GroupCallHost
    @ObservedObject private var link: LinkCallHost
    @StateObject private var offer: GuestAccountOfferModel

    init(account: SessionAccount, services: AppServices) {
        self.account = account
        self.services = services
        self._inviteIntake = ObservedObject(wrappedValue: services.inviteIntake)
        self._oneToOne = ObservedObject(wrappedValue: services.callHost)
        self._group = ObservedObject(wrappedValue: services.groupCallHost)
        self._link = ObservedObject(wrappedValue: services.linkCallHost)
        // An autoclosure: built once, for the first instance of this view.
        self._offer = StateObject(wrappedValue: Self.makeOffer(services: services))
    }

    private static func makeOffer(services: AppServices) -> GuestAccountOfferModel {
        let model = GuestAccountOfferModel(repository: services.authRepository)
        let intake = services.inviteIntake
        // Kept: once "Protect your account" is through the person goes into
        // the app, where an account belongs. The call link is not offered
        // again there.
        model.onKept = { intake.clearCallJoin() }
        return model
    }

    /// A call of any kind holds the one call slot: starting, waiting for
    /// admission, or live.
    private var callInProgress: Bool {
        CallSlot.occupant(
            oneToOneInCall: oneToOne.occupiesSlot,
            groupCallActive: group.occupiesSlot,
            linkCallActive: link.occupiesSlot
        ) != nil
    }

    private var showsForm: Bool { offer.step == .form && !callInProgress }

    var body: some View {
        content
            // The one full-screen call, as on every signed-in screen. A
            // guest has no app to put a call away into, so the call screen
            // offers no collapse here (`docs/ux/calls.md` section 4.5.3); the
            // system window of section 4.5.4 still opens when the app leaves
            // the screen.
            .callHostOverlay(
                oneToOne: services.callHost,
                group: services.groupCallHost,
                link: services.linkCallHost,
                inviteIntake: inviteIntake,
                allowsCollapse: false
            )
            // The call shows the guest's own name on its tile and announces
            // it to the other people in it. Idempotent: the guest sign-up
            // activates it before the session lands; this covers a relaunch.
            .task(id: account.accountIdB64) {
                ProfileDirectory.shared.activate(
                    accountIdB64: account.accountIdB64, username: account.username)
            }
    }

    @ViewBuilder
    private var content: some View {
        if let slug = inviteIntake.pendingCallJoinSlug {
            ZStack {
                // Kept mounted under the form, so `Back` returns to the
                // landing as it was.
                JoinCallLandingScreen(
                    viewModel: JoinCallLandingViewModel(
                        slug: slug,
                        authRepository: services.authRepository,
                        intake: inviteIntake,
                        linkCallHost: services.linkCallHost
                    ),
                    // The question is never one press away from a running
                    // call (another link's, here): the landing keeps its
                    // `Close` until none runs.
                    guestOffer: callInProgress ? nil : offer,
                    onDone: {}
                )
                .id(slug)
                if showsForm {
                    GuestClaimFormScreen(model: offer)
                }
            }
        } else if showsForm {
            GuestClaimFormScreen(model: offer)
        } else {
            switch guestScreen(guest: true, callInProgress: callInProgress) {
            case .inCall:
                GuestInCallReturn(link: link, inviteIntake: inviteIntake)
            case .offer, .none:
                GuestOfferScreen(model: offer)
            }
        }
    }
}

/// ``GuestInCallScreen`` wired to the call: a live call that was put away
/// comes back to the screen; a call still connecting or waiting for
/// admission has no screen yet, so its link is opened again instead.
///
/// A view of its own because the collapsed / expanded flag
/// (``CallBarHost``) is owned by the call overlay and reaches the screens
/// under it through the environment.
private struct GuestInCallReturn: View {
    @ObservedObject var link: LinkCallHost
    let inviteIntake: InviteIntake
    @EnvironmentObject private var callBar: CallBarHost

    var body: some View {
        GuestInCallScreen(onReturn: {
            callBar.expand()
            if link.presentation == nil, let slug = link.slot?.slug {
                inviteIntake.parkCallJoin(slug: slug)
            }
        })
    }
}
