// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import SwiftUI

/// `@main` entry point for the iOS / iPadOS app.
///
/// Mirror of:
///
///   * `apps/android`'s `MainActivity` (1.15c → 1.15d6)
///   * `apps/web`'s `App.tsx` + `routes.tsx` (1.16a/b)
///
/// Single ``WindowGroup`` hosting ``AppRoot``, which observes the
/// process-scoped ``AuthRepository`` and routes between the
/// loading / anonymous / authenticated surfaces:
///
///   * ``AuthState/loading``  — invisible in practice (rehydrate
///     is synchronous), kept for symmetry with the web side.
///   * ``AuthState/anon(cachedUsername:)`` — ``AnonRoot`` with a
///     local `Signup` / `Login` / `Recover` toggle.
///   * ``AuthState/session(_:)`` — ``HomeScreen``.
@main
struct LakkoApp: App {
    /// One ``AuthRepository`` per process — `@StateObject` so SwiftUI
    /// owns the lifecycle and the same instance survives every
    /// view-tree rebuild during a session.
    @StateObject private var authRepository = AuthRepository()

    var body: some Scene {
        WindowGroup {
            AppRoot()
                .environmentObject(authRepository)
        }
    }
}

/// Top-level router. Subscribes to ``AuthRepository/state`` via
/// `@EnvironmentObject` and picks the destination subtree.
struct AppRoot: View {
    @EnvironmentObject private var authRepository: AuthRepository

    var body: some View {
        switch authRepository.state {
        case .loading:
            // Practically invisible — rehydrate is synchronous. We
            // render a blank background so a slow tap on the launch
            // screen doesn't flash white.
            Color(.systemBackground)
                .ignoresSafeArea()
        case .anon(let cachedUsername):
            AnonRoot(
                repository: authRepository,
                cachedUsername: cachedUsername
            )
        case .session(let account):
            HomeScreen(
                account: account,
                onLogout: authRepository.logout,
                onForgetDevice: authRepository.forgetDevice
            )
        }
    }
}

/// Anonymous-state router — three destinations (Signup / Login /
/// Recover) selected via a local `@State` toggle. SwiftUI
/// `NavigationStack` would also work but adds a navigation bar +
/// stack-based back-button affordance we don't want during auth
/// (the back button on an OPAQUE flow would let the user park
/// halfway through a signup; the explicit "Already have an
/// account?" link is the only entry / exit we want surfaced).
///
/// Mirror of `apps/android`'s `AnonRoot` from 1.15d6 + the web's
/// `routes.tsx` `Anon` group.
private struct AnonRoot: View {
    /// All three VMs are owned via `@StateObject` so they survive
    /// view-tree rebuilds (which happen any time the outer
    /// ``AuthRepository`` publishes — e.g. when ``AuthState`` flips
    /// from `.loading` to `.anon`). Constructing the VMs inside the
    /// switch cases would orphan in-flight async Tasks the moment
    /// SwiftUI re-runs `body`; the `@StateObject` pattern pins them
    /// to the lifetime of `AnonRoot`.
    @StateObject private var signupVM: SignupViewModel
    @StateObject private var loginVM: LoginViewModel
    @StateObject private var recoverVM: RecoverViewModel

    @State private var destination: AnonDestination

    init(repository: AuthRepository, cachedUsername: String?) {
        // `StateObject(wrappedValue:)` accepts an autoclosure that
        // SwiftUI evaluates exactly once on first body evaluation.
        // The `_property = StateObject(...)` access shape is the
        // canonical way to seed a property wrapper from an init.
        self._signupVM = StateObject(
            wrappedValue: SignupViewModel(repository: repository)
        )
        self._loginVM = StateObject(
            wrappedValue: LoginViewModel(
                repository: repository,
                cachedUsername: cachedUsername
            )
        )
        self._recoverVM = StateObject(
            wrappedValue: RecoverViewModel(
                repository: repository,
                cachedUsername: cachedUsername
            )
        )
        // Default destination keyed on cache: a returning user
        // (cachedUsername non-nil) lands on Login; a fresh device
        // lands on Signup. Same posture as Android + web.
        self._destination = State(
            initialValue: cachedUsername == nil ? .signup : .login
        )
    }

    var body: some View {
        switch destination {
        case .signup:
            SignupScreen(
                viewModel: signupVM,
                onAuthenticated: {
                    // No-op — the auth state flip already drives the
                    // outer router to swap to ``HomeScreen``. The
                    // signup VM uses this hook for the
                    // recovery-phrase acknowledgement gate; once the
                    // user acknowledges, the `.session` value is
                    // already live.
                },
                onNavigateToLogin: { destination = .login }
            )
        case .login:
            LoginScreen(
                viewModel: loginVM,
                onNavigateToSignup: { destination = .signup },
                onNavigateToRecover: { destination = .recover }
            )
        case .recover:
            RecoverScreen(
                viewModel: recoverVM,
                onNavigateToLogin: { destination = .login },
                onNavigateToSignup: { destination = .signup }
            )
        }
    }
}

/// Three anon destinations. SwiftUI `NavigationStack` lands later
/// when the authenticated surface needs more than one route
/// (1.14e+); the local toggle is enough for the OPAQUE-auth phase.
private enum AnonDestination {
    case signup
    case login
    case recover
}
