// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI

/// The invite page (ADR-0079, `docs/ux/contact-invite-link.md` section 3): the
/// screen a tapped `https://lakko.me/i/<id>#<secret>` link lands on.
///
/// Mirror of `apps/web/src/routes/Invite.tsx` and the Android
/// `InviteLandingScreen.kt`. Opening the link spends nothing and sends
/// nothing; **Add** spends one ledger use, records the inviter as a pending
/// contact and sends the request carrying the link answer, then the page
/// closes into the chat with the inviter. No fingerprint, no safety number:
/// those stay on the contact's info screen.
///
/// A signed-out person sees no inviter card at all (UX section 3, amended
/// 2026-10-19): the ordinary sign-in screen, with the deep link parked; once a
/// session lands the link is checked again here. A link that does not work
/// (revoked, expired, used up, unknown, damaged) has no page: the person is
/// taken to Contacts, and so is a person whose Add found the link dead.
public struct InviteLandingScreen: View {

    @ObservedObject public var viewModel: InviteLandingViewModel
    public var onDone: () -> Void
    /// The chat with the inviter: the pending chat after **Add**, the
    /// one-to-one when they are already a contact.
    public var onOpenChat: (InviteLandingViewModel.ChatTarget) -> Void
    /// A link that does not work: the Contacts screen, nothing else shown
    /// (amended 2026-10-19).
    public var onOpenContacts: () -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: InviteLandingViewModel,
        onDone: @escaping () -> Void,
        onOpenChat: @escaping (InviteLandingViewModel.ChatTarget) -> Void,
        onOpenContacts: @escaping () -> Void
    ) {
        self.viewModel = viewModel
        self.onDone = onDone
        self.onOpenChat = onOpenChat
        self.onOpenContacts = onOpenContacts
    }

    public var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                content
            }
            .frame(maxWidth: .infinity)
            .padding(24)
        }
        .navigationTitle(l10n.t("invite.contact.page_title"))
        .navigationBarTitleDisplayMode(.inline)
        .task { await viewModel.open() }
        .onChange(of: viewModel.chatToOpen) { _, target in
            guard let target else { return }
            viewModel.dismiss()
            onOpenChat(target)
        }
        .onChange(of: viewModel.state) { _, state in
            guard state == .dead else { return }
            viewModel.dismiss()
            onOpenContacts()
        }
    }

    private func name(_ stated: String?) -> String {
        stated ?? l10n.t("invite.contact.page.unnamed")
    }

    @ViewBuilder
    private var content: some View {
        switch viewModel.state {
        case .loading:
            ProgressView().controlSize(.large).padding(.top, 40)

        case .ready(let invite, let adding, let failed):
            InitialsAvatar(
                name: invite.statedName ?? "",
                identity: base64Encode(invite.ownerSignaturePubkey),
                size: 72,
                placeholderSymbol: "person.fill")
            Text(l10n.t("invite.contact.page.title", texts: ["name": name(invite.statedName)]))
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            if let username = invite.username, !username.isEmpty, invite.statedName != "@\(username)" {
                Text("@\(username)")
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if !invite.label.isEmpty {
                Text(invite.label)
                    .font(.body)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }
            Button {
                Task { await viewModel.add() }
            } label: {
                Group {
                    if adding { ProgressView() } else { Text(l10n.t("invite.contact.page.add")) }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(adding)
            Button {
                viewModel.dismiss()
                onDone()
            } label: {
                Text(l10n.t("invite.contact.page.not_now")).frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
            .disabled(adding)
            if failed {
                Text(l10n.t("invite.contact.page.add_failed", texts: ["name": name(invite.statedName)]))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .multilineTextAlignment(.center)
            }

        case .alreadyContacts(_, let statedName, _, let answering, let failure):
            Text(l10n.t("invite.contact.page.already_title", texts: ["name": name(statedName)]))
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            // Amended 2026-10-24 — Open chat still delivers the link's
            // answer, then the chat opens (``InviteLandingViewModel/openChatAsKnownContact()``).
            Button {
                Task { await viewModel.openChatAsKnownContact() }
            } label: {
                Group {
                    if answering {
                        ProgressView()
                    } else {
                        Text(l10n.t("invite.contact.page.open_chat"))
                    }
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(answering)
            if let failure {
                Text(failure == .rateLimited
                    ? inviteRateLimitedMessage
                    : l10n.t("invite.contact.page.answer_failed", texts: ["name": name(statedName)]))
                    .font(.footnote)
                    .foregroundStyle(Color.lakkoDanger)
                    .multilineTextAlignment(.center)
            }

        case .own:
            message(title: l10n.t("invite.contact.page.own_title"), body: l10n.t("invite.contact.page.own_body"))

        case .dead:
            // No page: the screen goes to Contacts (see `onChange` above).
            EmptyView()

        case .rateLimited:
            Text(inviteRateLimitedMessage)
                .font(.body)
                .multilineTextAlignment(.center)
            Button(l10n.t("common.try_again")) {
                Task { await viewModel.retryOpen() }
            }
            .buttonStyle(.bordered)

        case .openFailed:
            Text(l10n.t("invite.contact.result.failed_title"))
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            Button(l10n.t("common.try_again")) {
                Task { await viewModel.retryOpen() }
            }
            .buttonStyle(.bordered)

        case .requiresReauth:
            message(title: l10n.t("chat.reauth.title"), body: l10n.t("contacts.reauth.body"))
        }
    }

    @ViewBuilder
    private func message(title: String, body: String) -> some View {
        Text(title)
            .font(.title2.weight(.semibold))
            .multilineTextAlignment(.center)
        Text(body)
            .font(.body)
            .foregroundStyle(Color.lakkoTextMuted)
            .multilineTextAlignment(.center)
        Button {
            viewModel.dismiss()
            onDone()
        } label: {
            Text(l10n.t("invite.contact.page.done")).frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
    }
}

/// State and actions of ``InviteLandingScreen``.
@MainActor
public final class InviteLandingViewModel: ObservableObject {

    /// Where the page closes into.
    public enum ChatTarget: Equatable {
        /// The chat with a person added through their link whose app has not
        /// answered yet (keyed on the inviter's account id hex).
        case pending(accountIdHex: String)
        /// The one-to-one with an established contact.
        case conversation(groupIdHex: String)
        /// An established contact this device holds no conversation with:
        /// their info screen (keyed on the accepted request's id).
        case contact(messageId: Int64)
    }

    /// Why Open chat's answer did not go (the line under the button).
    public enum AnswerFailure: Equatable {
        /// The ledger refused for too many attempts: the shared text.
        case rateLimited
        /// A network or server failure: `invite.contact.page.answer_failed`.
        case failed
    }

    public enum State: Equatable {
        case loading
        /// The default page. `adding` while Add runs; `failed` after a
        /// network failure (the line under the button).
        case ready(OpenedInvite, adding: Bool, failed: Bool)
        /// The inviter is already a contact. `invite` is the opened link,
        /// whose answer Open chat delivers; `answering` while it does;
        /// `failure` after it could not (amended 2026-10-24).
        case alreadyContacts(
            ChatTarget, statedName: String?, invite: OpenedInvite? = nil,
            answering: Bool = false, failure: AnswerFailure? = nil)
        case own
        /// Revoked, expired, used up, unknown or damaged: one state, which
        /// takes the person to Contacts (amended 2026-10-19).
        case dead
        case rateLimited
        case openFailed
        case requiresReauth
    }

    @Published public private(set) var state: State = .loading
    /// Set once Add has recorded the pending contact: the screen closes into
    /// that chat.
    @Published public private(set) var chatToOpen: ChatTarget?

    private let slug: String
    private let repository: ContactsRepository
    private let intake: InviteIntake
    private var didOpen = false
    /// The link as last opened, decided again when the lists change.
    private var opened: OpenedInvite?
    private var cancellables: Set<AnyCancellable> = []

    public init(slug: String, repository: ContactsRepository, intake: InviteIntake) {
        self.slug = slug
        self.repository = repository
        self.intake = intake
        // The contact list and the pending contacts can change while the page
        // is open (the inviter's answer to an earlier Add lands): the page's
        // decision is read again then, from this device's lists alone. Read
        // on the next run-loop pass, once `@Published` has stored the value.
        repository.$state.map(\.accepted).removeDuplicates().map { _ in () }
            .merge(with: repository.$invited.removeDuplicates().map { _ in () })
            .dropFirst(2)
            .receive(on: RunLoop.main)
            .sink { [weak self] in self?.decideAgain() }
            .store(in: &cancellables)
    }

    /// The page's state for the opened link again, when nothing runs on it.
    /// A change of kind replaces the page; the same kind keeps its line.
    private func decideAgain() {
        guard let opened, chatToOpen == nil else { return }
        switch state {
        case .ready(_, adding: false, _), .alreadyContacts(_, _, _, false, _): break
        default: return
        }
        let next = Self.state(for: repository.inviteOpenResult(for: opened))
        if Self.kind(of: next) != Self.kind(of: state) { state = next }
    }

    /// What tells two page states apart for ``decideAgain()``: the case, and
    /// for "already" where Open chat goes.
    private static func kind(of state: State) -> String {
        switch state {
        case .ready: return "ready"
        case .alreadyContacts(let target, _, _, _, _): return "already:\(target)"
        default: return "\(state)"
        }
    }

    /// What the page shows for each result of opening the link.
    public nonisolated static func state(for result: InviteOpenResult) -> State {
        switch result {
        case .opened(let invite): return .ready(invite, adding: false, failed: false)
        case .dead: return .dead
        case .own: return .own
        case .knownContact(let messageId, let groupIdHex, let statedName, let invite):
            let target: ChatTarget = groupIdHex.map { .conversation(groupIdHex: $0) }
                ?? .contact(messageId: messageId)
            return .alreadyContacts(target, statedName: statedName, invite: invite)
        case .pendingInviter(let accountIdHex, let statedName, _):
            // The same page as an established contact; Open chat opens the
            // waiting chat and sends nothing (no link to answer here).
            return .alreadyContacts(.pending(accountIdHex: accountIdHex), statedName: statedName)
        case .rateLimited: return .rateLimited
        case .requiresReauth: return .requiresReauth
        case .failed: return .openFailed
        }
    }

    /// Open the link once per screen.
    public func open() async {
        guard !didOpen else { return }
        didOpen = true
        await load()
    }

    /// Open again after a refusal or a network failure.
    public func retryOpen() async {
        state = .loading
        await load()
    }

    private func load() async {
        let result = await repository.openInviteLink(slug: slug)
        if case .failed(let message) = result { NSLog("[invite] open failed: \(message)") }
        switch result {
        case .opened(let invite): opened = invite
        case .knownContact(_, _, _, let invite), .pendingInviter(_, _, let invite): opened = invite
        default: opened = nil
        }
        state = Self.state(for: result)
    }

    /// **Add**. Runs to its end even when the screen goes away: stopped half
    /// way, the ledger use would be spent with no request sent.
    public func add() async {
        guard case .ready(let invite, false, _) = state else { return }
        state = .ready(invite, adding: true, failed: false)
        let repository = self.repository
        let result = await Cancellation.shielded {
            await repository.addThroughInviteLink(invite)
        }
        switch result {
        case .added(let contact):
            chatToOpen = .pending(accountIdHex: contact.accountIdHex)
        case .restored(let groupIdHex):
            chatToOpen = .conversation(groupIdHex: groupIdHex)
        case .dead:
            state = .dead
        case .rateLimited:
            state = .ready(invite, adding: false, failed: true)
        case .requiresReauth:
            state = .requiresReauth
        case .failed(let message):
            NSLog("[invite] add failed: \(message)")
            state = .ready(invite, adding: false, failed: true)
        }
    }

    /// **Open chat** on the "already in your contacts" page
    /// (`docs/ux/contact-invite-link.md` section 3, amended 2026-10-24): the
    /// link's answer is delivered first, so an inviter who removed this person
    /// gets them back, and the chat opens once it is written, or at once when
    /// the link no longer works. A failure keeps the page with its line and
    /// the button back. Runs to its end even when the screen goes away, as
    /// Add does.
    public func openChatAsKnownContact() async {
        guard case .alreadyContacts(let target, let statedName, let invite, false, _) = state else { return }
        guard let invite else {
            chatToOpen = target
            return
        }
        state = .alreadyContacts(target, statedName: statedName, invite: invite, answering: true)
        let repository = self.repository
        let result = await Cancellation.shielded {
            await repository.answerAsKnownContact(invite)
        }
        switch result {
        case .sent, .dead:
            chatToOpen = target
        case .rateLimited:
            state = .alreadyContacts(
                target, statedName: statedName, invite: invite, failure: .rateLimited)
        case .requiresReauth:
            state = .requiresReauth
        case .failed(let message):
            NSLog("[invite] answer failed: \(message)")
            state = .alreadyContacts(target, statedName: statedName, invite: invite, failure: .failed)
        }
    }

    /// Drop the parked slug so the next invite tap goes through cleanly.
    public func dismiss() {
        intake.clear()
    }
}
