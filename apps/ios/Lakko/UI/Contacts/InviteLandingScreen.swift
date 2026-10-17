// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

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
/// A signed-out person sees ``SignedOutInviteScreen`` instead (UX section 3,
/// "Signed out"); the deep link stays parked, and once a session lands the
/// page opens here in its default state.
public struct InviteLandingScreen: View {

    @ObservedObject public var viewModel: InviteLandingViewModel
    public var onDone: () -> Void
    /// The chat with the inviter: the pending chat after **Add**, the
    /// one-to-one when they are already a contact.
    public var onOpenChat: (InviteLandingViewModel.ChatTarget) -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    public init(
        viewModel: InviteLandingViewModel,
        onDone: @escaping () -> Void,
        onOpenChat: @escaping (InviteLandingViewModel.ChatTarget) -> Void
    ) {
        self.viewModel = viewModel
        self.onDone = onDone
        self.onOpenChat = onOpenChat
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

        case .alreadyContacts(let target, let statedName):
            Text(l10n.t("invite.contact.page.already_title", texts: ["name": name(statedName)]))
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            Button {
                viewModel.dismiss()
                onOpenChat(target)
            } label: {
                Text(l10n.t("invite.contact.page.open_chat")).frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

        case .own:
            message(title: l10n.t("invite.contact.page.own_title"), body: l10n.t("invite.contact.page.own_body"))

        case .dead:
            message(title: l10n.t("invite.contact.page.dead_title"), body: l10n.t("invite.contact.page.dead_body"))

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

/// The invite page for a person who is not signed in
/// (`docs/ux/contact-invite-link.md` section 3, "Signed out"; ADR-0079
/// reworked 2026-10-17): the same page, with **Sign in to add Anna** and
/// **Create an account**. Mirror of the web `Invite.tsx` signed-out state and
/// the Android `SignedOutInviteScreen`. Opening it spends nothing and sends
/// nothing; the link stays parked, so after either choice the person returns
/// to the invite page in its default state, not to the chat list.
public struct SignedOutInviteScreen: View {

    @StateObject private var viewModel: SignedOutInviteViewModel
    public var onSignIn: () -> Void
    public var onCreateAccount: () -> Void
    public var onDone: () -> Void
    /// ADR-0061 — every line follows the interface language.
    @ObservedObject private var l10n = Localization.shared

    public init(
        slug: String,
        preview: @escaping (String) async -> InvitePreviewResult,
        onSignIn: @escaping () -> Void,
        onCreateAccount: @escaping () -> Void,
        onDone: @escaping () -> Void
    ) {
        self._viewModel = StateObject(wrappedValue: SignedOutInviteViewModel(slug: slug, preview: preview))
        self.onSignIn = onSignIn
        self.onCreateAccount = onCreateAccount
        self.onDone = onDone
    }

    public var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 16) { content }
                    .frame(maxWidth: .infinity)
                    .padding(24)
            }
            .navigationTitle(l10n.t("invite.contact.page_title"))
            .navigationBarTitleDisplayMode(.inline)
            .task { await viewModel.open() }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch viewModel.state {
        case .loading:
            ProgressView().controlSize(.large).padding(.top, 40)
        case .preview(let preview):
            let name = preview.statedName ?? l10n.t("invite.contact.page.unnamed")
            InitialsAvatar(
                name: preview.statedName ?? "",
                identity: base64Encode(preview.ownerSignaturePubkey),
                size: 72,
                placeholderSymbol: "person.fill")
            Text(l10n.t("invite.contact.page.title", texts: ["name": name]))
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            if let username = preview.username, !username.isEmpty, preview.statedName != "@\(username)" {
                Text("@\(username)")
                    .font(.subheadline)
                    .foregroundStyle(Color.lakkoTextMuted)
            }
            if !preview.label.isEmpty {
                Text(preview.label)
                    .font(.body)
                    .foregroundStyle(Color.lakkoTextMuted)
                    .multilineTextAlignment(.center)
            }
            Button(action: onSignIn) {
                Text(l10n.t("invite.contact.page.sign_in", texts: ["name": name])).frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            Button(action: onCreateAccount) {
                Text(l10n.t("invite.contact.page.create_account")).frame(maxWidth: .infinity)
            }
            .buttonStyle(.bordered)
            .controlSize(.large)
        case .dead:
            Text(l10n.t("invite.contact.page.dead_title"))
                .font(.title2.weight(.semibold))
                .multilineTextAlignment(.center)
            Text(l10n.t("invite.contact.page.dead_body"))
                .font(.body)
                .foregroundStyle(Color.lakkoTextMuted)
                .multilineTextAlignment(.center)
            Button(action: onDone) {
                Text(l10n.t("invite.contact.page.done")).frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        case .rateLimited, .failed:
            Text(viewModel.state == .rateLimited
                 ? inviteRateLimitedMessage
                 : l10n.t("invite.contact.result.failed_title"))
                .font(.body)
                .multilineTextAlignment(.center)
            Button(l10n.t("common.try_again")) {
                Task { await viewModel.retry() }
            }
            .buttonStyle(.bordered)
        }
    }
}

/// State of ``SignedOutInviteScreen``.
@MainActor
final class SignedOutInviteViewModel: ObservableObject {
    enum State: Equatable {
        case loading
        case preview(InvitePreview)
        case dead
        case rateLimited
        case failed
    }

    @Published private(set) var state: State = .loading
    private let slug: String
    private let preview: (String) async -> InvitePreviewResult
    private var didOpen = false

    init(slug: String, preview: @escaping (String) async -> InvitePreviewResult) {
        self.slug = slug
        self.preview = preview
    }

    /// What the page shows for each result of opening the link.
    nonisolated static func state(for result: InvitePreviewResult) -> State {
        switch result {
        case .preview(let p): return .preview(p)
        case .dead: return .dead
        case .rateLimited: return .rateLimited
        case .failed: return .failed
        }
    }

    func open() async {
        guard !didOpen else { return }
        didOpen = true
        state = Self.state(for: await preview(slug))
    }

    func retry() async {
        state = .loading
        state = Self.state(for: await preview(slug))
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

    public enum State: Equatable {
        case loading
        /// The default page. `adding` while Add runs; `failed` after a
        /// network failure (the line under the button).
        case ready(OpenedInvite, adding: Bool, failed: Bool)
        case alreadyContacts(ChatTarget, statedName: String?)
        case own
        /// Revoked, expired, used up, unknown or damaged: one state.
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

    public init(slug: String, repository: ContactsRepository, intake: InviteIntake) {
        self.slug = slug
        self.repository = repository
        self.intake = intake
    }

    /// What the page shows for each result of opening the link.
    public nonisolated static func state(for result: InviteOpenResult) -> State {
        switch result {
        case .opened(let invite): return .ready(invite, adding: false, failed: false)
        case .dead: return .dead
        case .own: return .own
        case .knownContact(let messageId, let groupIdHex, let statedName):
            let target: ChatTarget = groupIdHex.map { .conversation(groupIdHex: $0) }
                ?? .contact(messageId: messageId)
            return .alreadyContacts(target, statedName: statedName)
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

    /// Drop the parked slug so the next invite tap goes through cleanly.
    public func dismiss() {
        intake.clear()
    }
}
