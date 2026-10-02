// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import SwiftUI

/// What a guest is shown outside a call (`docs/ux/call-links.md` section 9).
///
/// Mirror of `apps/web/src/auth/GuestAccountOffer.tsx`.
///
/// A guest account is made for a person who opened a call link without an
/// account. They were not asked to save a recovery phrase and they were not
/// asked to choose a username: they went into the call. This is the one
/// question, asked once the call is over:
///
///   * `Create account`: a username and a password, then the recovery phrase
///     (the app root shows it, as after any sign-up);
///   * `No, thanks`: the account is deleted at once, on the servers and here.
///     No confirmation dialog: the line under the button says what it does.
///
/// The model is one object for the question and the form, held by
/// ``GuestRoot``; the call-link landing draws the question, the guest screen
/// draws it too, and the form is always the whole screen.
@MainActor
final class GuestAccountOfferModel: ObservableObject {

    enum Step: Equatable {
        /// `Keep using Lakko?`
        case ask
        /// The username and password form.
        case form
    }

    enum Busy: Equatable {
        case none
        case creating
        case deleting
    }

    // MARK: - State

    @Published private(set) var step: Step = .ask
    @Published private(set) var busy: Busy = .none
    /// Kept as ``UiText`` so a message on screen follows a live language
    /// switch.
    @Published private var errorText: UiText?
    var error: String? { errorText?.text }
    /// The delete failed; the account is still there and still a guest.
    @Published private(set) var deleteFailed = false
    /// This device cannot show the account's phrase, so it cannot be kept.
    @Published private(set) var cannotKeep = false

    // MARK: - Form fields

    @Published var username = ""
    @Published var password = ""
    @Published var confirm = ""

    // MARK: - Wiring

    private let repository: AuthRepository
    private var guestWatch: AnyCancellable?
    /// Called once `Create account` succeeded: the account is kept and its
    /// recovery phrase is pending.
    var onKept: (() -> Void)?

    init(repository: AuthRepository) {
        self.repository = repository
        // The claimed username lives in the repository (the guest marker).
        guestWatch = repository.$guestAccount
            .sink { [weak self] _ in self?.objectWillChange.send() }
    }

    /// The username an earlier attempt already took: shown, never asked
    /// again.
    var takenName: String? { repository.guestClaimedUsername }

    /// The inline reason under the username field, or `nil`.
    var usernameHint: String? { checkUsername(username).inlineMessage }

    /// Whether the form may be submitted.
    var canSubmit: Bool {
        busy == .none && (takenName != nil || checkUsername(username).allowsLookup)
    }

    // MARK: - Steps

    func openForm() {
        guard busy == .none else { return }
        errorText = nil
        deleteFailed = false
        step = .form
    }

    func back() {
        guard busy == .none else { return }
        errorText = nil
        step = .ask
    }

    // MARK: - Create account

    func create() {
        guard busy == .none else { return } // ignore a double tap
        errorText = nil

        let name: String
        if let fixed = takenName {
            name = fixed
        } else {
            switch checkUsername(username) {
            case .empty:
                errorText = .key("auth.signup.error.enter_username")
                return
            case .invalid(let reason):
                errorText = UiText { reason.message }
                return
            case .valid(let canonical):
                name = canonical
            }
        }
        if password != confirm {
            errorText = .key("auth.password.error.mismatch")
            return
        }
        if password.count < SignupViewModel.minPasswordLength {
            errorText = .key(
                "auth.password.error.too_short",
                counts: ["count": UInt64(SignupViewModel.minPasswordLength)])
            return
        }

        busy = .creating
        let passwordBytes = Data(password.utf8)
        Task {
            do {
                // Resolving leaves the recovery phrase pending; the app root
                // switches to the phrase screen on that.
                try await repository.claimGuestAccount(username: name, password: passwordBytes)
                password = ""
                confirm = ""
                busy = .none
                onKept?()
            } catch {
                NSLog("[guest] keeping the account failed: %@", String(describing: type(of: error)))
                if error is GuestPhraseUnavailable {
                    cannotKeep = true
                    step = .ask
                }
                errorText = UiText { AuthErrorCopy.guestClaim(error) }
                busy = .none
            }
        }
    }

    // MARK: - No, thanks

    func decline() {
        guard busy == .none else { return }
        errorText = nil
        deleteFailed = false
        busy = .deleting
        Task {
            do {
                // Lands signed out; whoever drew this is replaced.
                try await repository.discardGuestAccount()
                busy = .none
            } catch {
                NSLog("[guest] deleting the account failed: %@", String(describing: type(of: error)))
                deleteFailed = true
                busy = .none
            }
        }
    }
}

// MARK: - The question

/// `Keep using Lakko?` with its two buttons. A block: the call-link landing
/// draws it under "The call has ended.", the guest screen draws it alone.
struct GuestOfferQuestion: View {
    @ObservedObject var model: GuestAccountOfferModel
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            AuthTitle(title: l10n.t("guest.offer.title"), subtitle: l10n.t("guest.offer.body"))
            if let error = model.error {
                AuthHint(text: error, isError: true)
            }
            if model.deleteFailed {
                AuthHint(text: l10n.t("guest.offer.delete_failed"), isError: true)
            }
            if !model.cannotKeep {
                AuthPrimaryButton(
                    title: l10n.t("auth.signup.submit"),
                    disabled: model.busy != .none,
                    action: model.openForm
                )
            }
            AuthSecondaryButton(
                title: l10n.t(model.busy == .deleting ? "guest.offer.deleting" : "guest.offer.decline"),
                disabled: model.busy != .none,
                action: model.decline
            )
            AuthHint(text: l10n.t("guest.offer.decline_note"))
        }
        .frame(maxWidth: 480)
    }
}

// MARK: - The form

/// The username and password a guest account is kept with. The same fields,
/// hints and rules as the sign-up form (``SignupScreen``).
struct GuestClaimFormScreen: View {
    @ObservedObject var model: GuestAccountOfferModel
    @ObservedObject private var l10n = Localization.shared

    private var working: Bool { model.busy != .none }

    var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("auth.signup.title"), subtitle: l10n.t("guest.claim.lede"))

            if let name = model.takenName {
                // A username cannot be changed and one was already taken for
                // this account: shown, not asked again.
                AuthHint(text: l10n.t("guest.claim.username_taken_for_you", texts: ["username": name]))
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    AuthTextField(
                        label: l10n.t("auth.username.label"),
                        text: $model.username,
                        contentType: .username,
                        disabled: working
                    )
                    if let hint = model.usernameHint {
                        AuthHint(text: hint, isError: true)
                    } else {
                        AuthHint(text: l10n.t("auth.signup.username.hint"))
                    }
                }
            }

            AuthSecureField(
                label: l10n.t("auth.password.label"),
                text: $model.password,
                contentType: .newPassword,
                disabled: working
            )
            AuthHint(text: l10n.t(
                "auth.password.hint", counts: ["count": UInt64(SignupViewModel.minPasswordLength)]))

            AuthSecureField(
                label: l10n.t("auth.password.confirm.label"),
                text: $model.confirm,
                contentType: .newPassword,
                disabled: working,
                submitLabel: .go,
                onSubmit: model.create
            )

            if let error = model.error {
                AuthHint(text: error, isError: true)
            }

            AuthPrimaryButton(
                title: l10n.t("auth.signup.submit"),
                busy: model.busy == .creating,
                busyTitle: l10n.t("guest.claim.working"),
                disabled: !model.canSubmit,
                action: model.create
            )
            AuthSecondaryButton(
                title: l10n.t("common.back"),
                disabled: working,
                action: model.back
            )
        }
    }
}

// MARK: - The whole screen

/// The question as a whole screen: what a guest sees in place of the app
/// when no call is running and no call link is open.
struct GuestOfferScreen: View {
    @ObservedObject var model: GuestAccountOfferModel

    var body: some View {
        AuthScreen {
            GuestOfferQuestion(model: model)
        }
    }
}

/// What a guest sees in place of the app while a call runs: the person
/// belongs in the call, so the screen says so and offers the way back. The
/// question is asked only when no call is running, so the button that deletes
/// the account is never one press away from a live call.
struct GuestInCallScreen: View {
    /// Bring the call back to the screen.
    let onReturn: () -> Void
    @ObservedObject private var l10n = Localization.shared

    var body: some View {
        AuthScreen {
            AuthTitle(title: l10n.t("guest.in_call.title"))
            AuthPrimaryButton(title: l10n.t("guest.in_call.return"), action: onReturn)
        }
    }
}
