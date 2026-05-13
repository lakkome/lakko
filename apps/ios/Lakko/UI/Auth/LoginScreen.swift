// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// SwiftUI login screen.
///
/// Mirror of:
///
///   * `apps/web/src/routes/Login.tsx` (1.17d7)
///   * `apps/android/.../ui/auth/LoginScreen.kt` (1.15d5)
///
/// Single-stage form: username + password. Pre-fills the username
/// field from the cached value passed into ``LoginViewModel``. On
/// submit, drives ``AuthRepository/login(username:password:)``
/// which flips the auth state — the app-level router then snaps to
/// the home destination.
///
/// `onNavigateToSignup` and `onNavigateToRecover` flip the local
/// "which anon screen" toggle the host root view owns; SwiftUI
/// `NavigationStack`-driven routing lands in 1.14e when the
/// authenticated surface needs more than one destination.
public struct LoginScreen: View {

    @ObservedObject public var viewModel: LoginViewModel
    public var onNavigateToSignup: () -> Void
    public var onNavigateToRecover: () -> Void

    public init(
        viewModel: LoginViewModel,
        onNavigateToSignup: @escaping () -> Void,
        onNavigateToRecover: @escaping () -> Void = {}
    ) {
        self.viewModel = viewModel
        self.onNavigateToSignup = onNavigateToSignup
        self.onNavigateToRecover = onNavigateToRecover
    }

    public var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                Text("Log in to Lakko")
                    .font(.largeTitle)
                    .fontWeight(.semibold)

                Text(
                    "OPAQUE-PAKE login. Wrong username and wrong password " +
                    "produce the same error — by design (the server cannot " +
                    "tell us which one you got wrong without leaking " +
                    "whether the account exists)."
                )
                .font(.body)
                .foregroundStyle(.secondary)

                TextField("Username", text: $viewModel.username)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .disabled(viewModel.busy)

                SecureField("Password", text: $viewModel.password)
                    .textFieldStyle(.roundedBorder)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled(true)
                    .disabled(viewModel.busy)

                if let error = viewModel.error {
                    Text(error)
                        .font(.body)
                        .foregroundStyle(.red)
                }

                Button(action: viewModel.onSubmit) {
                    HStack {
                        if viewModel.busy {
                            ProgressView()
                                .controlSize(.small)
                                .progressViewStyle(.circular)
                                .tint(.white)
                            Text("Signing in…")
                        } else {
                            Text("Log in")
                        }
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                .disabled(viewModel.busy)

                HStack(spacing: 4) {
                    Spacer()
                    Text("New to Lakko?")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button(action: onNavigateToSignup) {
                        Text("Create an account")
                            .font(.footnote)
                    }
                    Spacer()
                }

                HStack(spacing: 4) {
                    Spacer()
                    Text("Lost your password?")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    Button(action: onNavigateToRecover) {
                        Text("Recover with your 24-word phrase")
                            .font(.footnote)
                    }
                    Spacer()
                }
            }
            .padding(24)
        }
    }
}
