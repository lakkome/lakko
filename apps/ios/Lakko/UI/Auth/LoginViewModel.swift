// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Foundation
import UIKit

/// State + dispatcher for ``LoginScreen``.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Login.tsx`'s local signals (1.17d7)
///   * `apps/android/.../ui/auth/LoginViewModel.kt` (1.15d5)
///
/// Simpler than ``SignupViewModel`` — single-stage form, no
/// recovery-phrase post-step.
///
/// The ``init(repository:cachedUsername:)`` `cachedUsername` arg
/// pre-fills the username field from
/// ``AuthState/anon(cachedUsername:)``. On a fresh device (no
/// cache) it's `nil` and the user types the handle from scratch.
@MainActor
public final class LoginViewModel: ObservableObject {

    // MARK: - Form fields

    @Published public var username: String
    @Published public var password: String = ""

    // MARK: - Internal status

    @Published public private(set) var busy: Bool = false
    /// Kept as ``UiText`` so a message on screen follows a live language switch.
    @Published private var errorText: UiText?
    public var error: String? { errorText?.text }

    // MARK: - Wiring

    private let repository: AuthRepository
    /// Ends text editing before the fields are read (``onSubmit()``).
    private let endEditing: @MainActor () -> Void

    public init(
        repository: AuthRepository,
        cachedUsername: String? = nil,
        endEditing: @escaping @MainActor () -> Void = LoginViewModel.resignKeyboard
    ) {
        self.repository = repository
        self.endEditing = endEditing
        self.username = cachedUsername ?? ""
        // ADR-0073 section 3.3 — the session was refused as expired on the
        // way here (the refresh credential is dead): say so, once.
        if repository.sessionExpiredNotice {
            self.errorText = .key("common.error.session_expired")
            repository.clearSessionExpiredNotice()
        }
    }

    /// Resign the first responder, so the keyboard applies every key press it
    /// still holds to the field before the form is read.
    public static func resignKeyboard() {
        UIApplication.shared.sendAction(
            #selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
    }

    // MARK: - Submit

    /// Submit the login form. Drives
    /// ``AuthRepository/login(username:password:)``, surfaces
    /// success as an ``AuthState/session(_:)`` transition (which
    /// the app-level router catches via `@Published` observation),
    /// and surfaces failures as inline form messages.
    ///
    /// Wrong-password / unknown-username collapse to the same
    /// generic copy — by design (OPAQUE indistinguishable failure
    /// + the same posture as the web side). Any other failure shows the
    /// generic failure, never the wrong-password sentence.
    ///
    /// The fields are read after editing has ended and one main-actor turn
    /// has passed, not at the tap: seen on the simulator (2026-11-22), a tap
    /// that landed while typed key presses were still being applied to the
    /// password field read a password missing its last characters, and the
    /// login was refused as a wrong password.
    public func onSubmit() {
        guard !busy else { return } // ignore double-tap

        busy = true
        errorText = nil
        endEditing()

        Task {
            await Task.yield()
            let usernameSnapshot = username
            let passwordSnapshot = password
            let passwordBytes = Data(passwordSnapshot.utf8)
            do {
                try await repository.login(
                    username: usernameSnapshot,
                    password: passwordBytes
                )
                // `repository.login` flipped the AuthState; the screen
                // will be replaced by the home destination on the
                // next SwiftUI render. Clear the password field so
                // the TextField buffer doesn't hold it past the
                // navigation.
                busy = false
                errorText = nil
                password = ""
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in, or called off:
                // not a failure.
                if error is StaleBindingError || Cancellation.isCancellation(error) { return }
                // Plain text, no identifiers: what failed, and whether the
                // fields changed after they were read.
                let changed = username != usernameSnapshot || password != passwordSnapshot
                NSLog("[auth] login failed: \(AuthErrorCopy.loginFailureLogLine(error)) fields_changed_after_submit=\(changed)")
                self.errorText = UiText { AuthErrorCopy.login(error) }
            }
        }
    }

    /// "Use passkey": the system sheet lists the passkeys saved for Lakko;
    /// no account has to be cached on this device. The username typed in
    /// the form (if any) is passed as a hint and kept only when it names
    /// the account the passkey signs in. A device that holds the keys lands
    /// signed in; otherwise the restore prompt opens. Dismissing the sheet
    /// is not an error.
    @MainActor
    public func onPasskeyLogin() {
        guard !busy else { return }
        busy = true
        errorText = nil
        let hint = username
        Task {
            do {
                try await repository.loginWithPasskey(usernameHint: hint)
                busy = false
                errorText = nil
            } catch {
                busy = false
                // Overtaken by a sign-out or another sign-in: not a failure.
                if error is StaleBindingError { return }
                if !AuthErrorCopy.isPasskeyCancelled(error) {
                    self.errorText = UiText { AuthErrorCopy.passkey(error) }
                }
            }
        }
    }
}
