// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// SwiftUI screen the user lands on after tapping a
/// `https://lakko.me/i/<slug>` Universal Link.
///
/// Mirror of:
///
///   * `apps/android/.../ui/contacts/InviteLandingScreen.kt`
///     (1.15e11b)
///   * `apps/web/src/contacts/InviteLandingRoute.tsx` (1.17e11b)
///
/// Drives ``ContactsRepository/acceptInviteLink(slug:)`` on first
/// appear + renders one of the nine discriminated
/// ``AcceptInviteResult`` variants. The user dismisses the result
/// with Done; the host calls
/// ``InviteIntake/clear()`` to drop the parked slug.
///
/// **Phase 1.9b4 (ADR-0005 §13.4) note:** the iOS / Android
/// landing screens auto-accept every invite — including
/// `bootstrap_referral` referrals — as soon as the surface
/// mounts. The tap on the universal link is the consent gesture;
/// requiring a second tap on Accept would be redundant. The web
/// client keeps an explicit Accept button for non-referral
/// invites (lets the user attach an intro) and only auto-
/// accepts `bootstrap_referral=true` invites for parity.
public struct InviteLandingScreen: View {

    @ObservedObject public var viewModel: InviteLandingViewModel
    public var onDone: () -> Void

    public init(
        viewModel: InviteLandingViewModel,
        onDone: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onDone = onDone
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                content
                Button(action: {
                    viewModel.dismiss()
                    onDone()
                }) {
                    Text("Done")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
            }
            .padding(24)
        }
        .navigationTitle("Invite")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await viewModel.run()
        }
    }

    @ViewBuilder
    private var content: some View {
        switch viewModel.state {
        case .running:
            VStack(alignment: .leading, spacing: 8) {
                ProgressView()
                    .controlSize(.large)
                Text("Verifying invite…")
                    .font(.title3)
                    .fontWeight(.semibold)
                Text("Checking the link.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }

        case .done(let result):
            VStack(alignment: .leading, spacing: 12) {
                Text(headline(for: result))
                    .font(.title2)
                    .fontWeight(.semibold)
                Text(copy(for: result))
                    .font(.body)
                    .foregroundStyle(.secondary)
                fingerprintBlock(for: result)
                if result == .rateLimited {
                    Button("Try again") {
                        Task { await viewModel.retry() }
                    }
                    .buttonStyle(.bordered)
                }
            }
        }
    }

    /// Surface the inviter's Signal-style safety number on
    /// the `.accepted` result. iOS auto-accepts every invite
    /// (ADR-0005 §13.4), so this is a **post-facto OOB
    /// verification** affordance — defence-in-depth against
    /// link-substitution attacks. The user can compare the
    /// digits against what the inviter shared through a
    /// trusted channel and remove the contact if mismatched.
    /// Mirror of the web `Invite.tsx` safety-number row,
    /// adapted to the iOS auto-accept flow.
    @ViewBuilder
    private func fingerprintBlock(for result: AcceptInviteResult) -> some View {
        if case .accepted(_, let fingerprint, _) = result {
            VStack(alignment: .leading, spacing: 6) {
                Text("Safety number")
                    .font(.subheadline)
                    .fontWeight(.semibold)
                Text(fingerprint)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(nil)
                    .fixedSize(horizontal: false, vertical: true)
                Text(
                    "Compare this with the number the inviter shows you " +
                    "in person or over a call. If it differs, remove the " +
                    "contact and ask for a fresh link."
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
            }
            .padding(12)
            .background(Color.secondary.opacity(0.08))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
    }

    private func headline(for result: AcceptInviteResult) -> String {
        switch result {
        case .accepted: return "Invite accepted"
        case .alreadyAccepted: return "Already accepted"
        case .expired: return "Invite expired"
        case .exhausted: return "Invite exhausted"
        case .notFound: return "Invite not found"
        case .malformedSlug: return "Invalid invite link"
        case .damaged: return "Damaged invite link"
        case .rateLimited: return "Try again in a minute"
        case .requiresReauth: return "Please log in first"
        case .failed: return "Couldn't accept invite"
        }
    }

    private func copy(for result: AcceptInviteResult) -> String {
        switch result {
        case .accepted:
            return
                "Your contact request is on its way. Once they accept, " +
                "the chat appears under Chats."
        case .alreadyAccepted:
            return
                "You already accepted this invite on this device. " +
                "Find the person under Contacts."
        case .expired:
            return
                "This invite link is past its expiry. Ask the inviter " +
                "to share a fresh one."
        case .exhausted:
            return
                "This invite has already been used the maximum number " +
                "of times. Ask the inviter to share a fresh one."
        case .notFound:
            return
                "This invite no longer exists. It may have been revoked " +
                "by the inviter or never existed."
        case .malformedSlug:
            return
                "This link is incomplete. Ask for it to be sent again."
        case .damaged:
            return groupInviteDamagedMessage
        case .rateLimited:
            return inviteRateLimitedMessage
        case .requiresReauth:
            return
                "You need an active session to accept an invite. Log in " +
                "and then re-tap the link from your messages."
        case .failed(let message):
            return message
        }
    }
}

/// State + dispatcher for ``InviteLandingScreen``.
///
/// Mirror of `apps/android/.../ui/contacts/InviteLandingViewModel.kt`
/// (1.15e11b). Single-shot: kicks the consume on first
/// ``run()`` call, settles into ``State/done(_:)``, stays there
/// until the screen is dismissed.
@MainActor
public final class InviteLandingViewModel: ObservableObject {

    /// Two-state local FSM. Mirrors the Android side's `Running` /
    /// `Done` sealed hierarchy.
    public enum State: Equatable {
        case running
        case done(AcceptInviteResult)
    }

    @Published public private(set) var state: State = .running

    private let slug: String
    /// The link's fragment (the link secret), `nil` when it had none.
    private let fragment: String?
    private let repository: ContactsRepository
    private let intake: InviteIntake
    private var didRun: Bool = false

    public init(
        slug: String,
        fragment: String?,
        repository: ContactsRepository,
        intake: InviteIntake
    ) {
        self.slug = slug
        self.fragment = fragment
        self.repository = repository
        self.intake = intake
    }

    /// Drive the accept-invite flow exactly once. The
    /// `.task { await viewModel.run() }` modifier fires on view
    /// appear; SwiftUI may re-run the modifier across body
    /// rebuilds, but the `didRun` guard keeps the consume to a
    /// single invocation per VM lifetime.
    public func run() async {
        guard !didRun else { return }
        didRun = true
        let result = await repository.acceptInviteLink(slug: slug, fragment: fragment)
        state = .done(result)
    }

    /// Run the accept flow again after a rate-limit refusal
    /// (`429`): nothing was consumed, so a second pass is the
    /// same request made later. Any other outcome is final.
    public func retry() async {
        guard case .done(.rateLimited) = state else { return }
        state = .running
        let result = await repository.acceptInviteLink(slug: slug, fragment: fragment)
        state = .done(result)
    }

    /// Drop the parked slug so a subsequent invite tap goes
    /// through cleanly.
    public func dismiss() {
        intake.clear()
    }
}
