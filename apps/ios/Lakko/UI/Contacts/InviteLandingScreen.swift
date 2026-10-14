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
    /// ADR-0073 R.7 case 1 / UX 16.6 — a link from a person who is already a
    /// contact opens that contact (the route's key is the contact's request
    /// id): this screen is replaced, it shows no result of its own.
    public var onOpenContact: (Int64) -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: InviteLandingViewModel,
        onDone: @escaping () -> Void,
        onOpenContact: @escaping (Int64) -> Void = { _ in }
    ) {
        self.viewModel = viewModel
        self.onDone = onDone
        self.onOpenContact = onOpenContact
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                content
                // No Done while the flow runs or hands over to the contact:
                // there is no result on this screen to be done with.
                if InviteLandingViewModel.showsResult(viewModel.state) {
                    Button(action: {
                        viewModel.dismiss()
                        onDone()
                    }) {
                        Text(l10n.t("common.done"))
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                }
            }
            .padding(24)
        }
        .navigationTitle(l10n.t("invite.contact.page_title"))
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await viewModel.run()
            openContactIfKnown()
        }
    }

    /// Hand over to the contact the link's owner already is, once.
    private func openContactIfKnown() {
        guard let contact = InviteLandingViewModel.contactToOpen(viewModel.state) else { return }
        viewModel.dismiss()
        onOpenContact(contact)
    }

    @ViewBuilder
    private var content: some View {
        switch viewModel.state {
        case .running, .done(.knownContact):
            // A known contact is never a result here: the progress stays
            // until the contact's own screen replaces this one.
            VStack(alignment: .leading, spacing: 8) {
                ProgressView()
                    .controlSize(.large)
                Text(l10n.t("invite.contact.accepting"))
                    .font(.title3)
                    .fontWeight(.semibold)
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
                    Button(l10n.t("common.try_again")) {
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
                Text(l10n.t("contacts.safety.number"))
                    .font(.subheadline)
                    .fontWeight(.semibold)
                Text(fingerprint)
                    .font(.system(.callout, design: .monospaced))
                    .textSelection(.enabled)
                    .lineLimit(nil)
                    .fixedSize(horizontal: false, vertical: true)
                Text(l10n.t("invite.contact.check_body"))
                .font(.footnote)
                .foregroundStyle(.secondary)
            }
            .padding(12)
            .background(Color.secondary.opacity(0.08))
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        }
    }

    /// The catalog key of the result's headline (ADR-0061 batch 4).
    private func headline(for result: AcceptInviteResult) -> String {
        switch result {
        case .accepted: return l10n.t("invite.contact.result.sent_title")
        case .alreadyAccepted, .knownContact: return l10n.t("invite.contact.result.already_title")
        case .expired: return l10n.t("invite.contact.result.expired_title")
        case .exhausted: return l10n.t("invite.contact.result.used_up_title")
        case .notFound: return l10n.t("invite.contact.result.not_found_title")
        case .malformedSlug: return l10n.t("invite.contact.invalid_title")
        case .damaged: return l10n.t("invite.contact.invalid_title")
        case .rateLimited: return l10n.t("invite.contact.result.rate_limited_title")
        case .requiresReauth: return l10n.t("chat.reauth.title")
        case .failed: return l10n.t("invite.contact.result.failed_title")
        }
    }

    /// The result's explanation. A failure's own detail is diagnostic prose
    /// (ADR-0061 section 6): it goes to the log, the screen says the
    /// action's sentence.
    private func copy(for result: AcceptInviteResult) -> String {
        switch result {
        case .accepted:
            return l10n.t("invite.contact.result.sent_body")
        case .alreadyAccepted, .knownContact:
            return l10n.t("invite.contact.result.already_body")
        case .expired:
            return l10n.t("invite.contact.expired")
        case .exhausted:
            return l10n.t("invite.contact.used_up")
        case .notFound:
            return l10n.t("invite.contact.no_longer_valid")
        case .malformedSlug:
            // The web renders a fragment-less link as the invalid panel.
            return l10n.t("invite.contact.invalid_body")
        case .damaged:
            return l10n.t("invite.link.damaged")
        case .rateLimited:
            return inviteRateLimitedMessage
        case .requiresReauth:
            return l10n.t("contacts.reauth.body")
        case .failed:
            return l10n.t("invite.contact.accept_failed")
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

    /// The contact a finished flow opens instead of showing a result: the
    /// link's owner is already an established contact.
    ///
    /// | state                         | opens a contact | shows a result |
    /// |-------------------------------|-----------------|----------------|
    /// | running                       | no              | no             |
    /// | done, a known contact         | that contact    | no             |
    /// | done, anything else           | no              | yes            |
    public nonisolated static func contactToOpen(_ state: State) -> Int64? {
        if case .done(.knownContact(let contactMessageId)) = state { return contactMessageId }
        return nil
    }

    /// Whether the screen shows a result (and its Done button) for `state`.
    public nonisolated static func showsResult(_ state: State) -> Bool {
        guard case .done = state else { return false }
        return contactToOpen(state) == nil
    }

    private let slug: String
    private let repository: ContactsRepository
    private let intake: InviteIntake
    private var didRun: Bool = false

    public init(
        slug: String,
        repository: ContactsRepository,
        intake: InviteIntake
    ) {
        self.slug = slug
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
        // Accepting is several writes in a row (the link is consumed, the
        // request is sent, the chat is created). This runs from the screen's
        // `.task`, which is cancelled when the person leaves; stopped half
        // way, the link would be spent with no request sent, and the next
        // tap would read "already accepted". So the accept runs to its end
        // whether or not the screen is still there.
        let repository = self.repository
        let slug = self.slug
        let result = await Cancellation.shielded {
            await repository.acceptInviteLink(slug: slug)
        }
        Self.logFailure(result)
        state = .done(result)
    }

    /// A failure's detail is diagnostic prose: logged, never shown.
    private static func logFailure(_ result: AcceptInviteResult) {
        if case .failed(let message) = result {
            NSLog("[invite] accept failed: \(message)")
        }
    }

    /// Run the accept flow again after a rate-limit refusal
    /// (`429`): nothing was consumed, so a second pass is the
    /// same request made later. Any other outcome is final.
    public func retry() async {
        guard case .done(.rateLimited) = state else { return }
        state = .running
        let result = await repository.acceptInviteLink(slug: slug)
        Self.logFailure(result)
        state = .done(result)
    }

    /// Drop the parked slug so a subsequent invite tap goes
    /// through cleanly.
    public func dismiss() {
        intake.clear()
    }
}
