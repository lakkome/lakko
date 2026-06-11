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
    /// 1.20c — `UIApplicationDelegate` adaptor that catches APNs
    /// registration callbacks the SwiftUI `App` protocol doesn't
    /// surface and forwards them into ``PushSubscriber`` for the
    /// actor-isolated hand-off. The adaptor is a tiny `NSObject`
    /// that lives for the entire process lifetime; SwiftUI keeps
    /// it alive via the property-wrapper machinery.
    @UIApplicationDelegateAdaptor(PushAppDelegate.self)
    private var pushDelegate

    /// One ``AuthRepository`` per process — `@StateObject` so SwiftUI
    /// owns the lifecycle and the same instance survives every
    /// view-tree rebuild during a session.
    @StateObject private var authRepository = AuthRepository()

    /// Process-scoped ``ContactsRepository`` — depends on the
    /// authRepository (it reads `identitySecrets` on every
    /// send/poll). 1.14e3 introduced this; 1.14e4 wires it into
    /// the SessionRoot.
    @StateObject private var contactsRepository: ContactsRepository

    /// Process-scoped ``ChatRepository`` (1.14f2). Owns the
    /// canonical MLS Identity for the session + per-group
    /// `Group` handles + snapshot persistence.
    @StateObject private var chatRepository = ChatRepository()

    /// Process-scoped ``DeviceGroupSyncRepository``
    /// (Phase 2a3e2b-iOS). Holds the per-session DeviceGroup
    /// handle + reactive maps for cross-device drafts /
    /// flags / read cursors / conversation order.
    @StateObject private var deviceGroupSync = DeviceGroupSyncRepository()

    /// Process-scoped ``InviteIntake`` — populated by
    /// `.onOpenURL` when iOS hands us a Universal Link.
    @StateObject private var inviteIntake = InviteIntake()

    init() {
        // G3-sframe-native N-4 — runtime probe for the non-public
        // `setFrameEncryptor:` / `setFrameDecryptor:` selectors
        // libwebrtc M125's RTCRtpSender / RTCRtpReceiver carry.
        //
        // We deliberately call this BEFORE any other side effect
        // because the probe's NO result becomes a process-wide
        // gate: `SFrameAvailability.shared.isCallingAvailable`
        // returns false, the call UI surfaces a disabled banner,
        // and `CallSession.start` aborts before opening any
        // RTCPeerConnection. There is no silent fallback to
        // non-E2EE media — security invariant #4 forbids it.
        //
        // The probe is cheap (one `class_respondsToSelector` per
        // class) and the result is cached for the process
        // lifetime; running it here means the very first call-UI
        // surface render already has the correct availability
        // state.
        SFrameAvailability.shared.runBootProbe()

        // `@StateObject` cannot reference another `@StateObject` in
        // its default initializer, so we hand-roll the wiring
        // inside `init`: build the AuthRepository first, then feed
        // it into ContactsRepository's init.
        let auth = AuthRepository()
        self._authRepository = StateObject(wrappedValue: auth)
        self._contactsRepository = StateObject(
            wrappedValue: ContactsRepository(authRepository: auth)
        )
    }

    var body: some Scene {
        WindowGroup {
            AppRoot()
                .environmentObject(authRepository)
                .environmentObject(contactsRepository)
                .environmentObject(chatRepository)
                .environmentObject(deviceGroupSync)
                .environmentObject(inviteIntake)
                // Universal Links — iOS invokes this whenever the
                // user taps a `https://lakko.me/i/<slug>` URL and
                // the on-server `apple-app-site-association` JSON
                // claims this bundle id. Fires for both cold-start
                // and hot deep-link paths on iOS 14+. ``InviteIntake``
                // parks the slug; ``AppRoot`` observes the
                // publisher and swaps the visible destination to
                // ``InviteLandingScreen``.
                .onOpenURL { url in
                    inviteIntake.handle(url)
                }
        }
    }
}

/// Top-level router. Subscribes to ``AuthRepository/state`` via
/// `@EnvironmentObject` and picks the destination subtree.
struct AppRoot: View {
    @EnvironmentObject private var authRepository: AuthRepository
    @EnvironmentObject private var contactsRepository: ContactsRepository
    @EnvironmentObject private var chatRepository: ChatRepository
    @EnvironmentObject private var deviceGroupSync: DeviceGroupSyncRepository
    @EnvironmentObject private var inviteIntake: InviteIntake

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
            // Pending invites land in `InviteIntake.pendingSlug`
            // but cannot consume in the anon state — the
            // ``acceptInviteLink`` flow needs `identitySecrets`.
            // We leave the slug parked; once the user signs in,
            // the session-state branch picks it up.
            .onAppear {
                // Defensive: returning to anon (logout /
                // forgetDevice) tears down the cross-device sync
                // runtime so the next login bootstraps fresh.
                deviceGroupSync.clear()
            }

        case .session(let account):
            SessionRoot(
                account: account,
                authRepository: authRepository,
                contactsRepository: contactsRepository,
                chatRepository: chatRepository,
                inviteIntake: inviteIntake
            )

        case .needsRecoveryPhrase(let account):
            // ADR-0017 fresh-device fail-safe. The user authenticated
            // (password / passkey) but this device has no cached
            // AccountIdentity (phrase) seed — the canonical identity
            // cannot be rebuilt without the phrase. We render the
            // restore-phrase prompt instead of `HomeScreen`; supplying
            // the phrase flips the state to `.session`.
            RestoreIdentityPromptScreen(
                viewModel: RestoreIdentityPromptViewModel(
                    repository: authRepository,
                    username: account.username,
                    accountIdB64: account.accountIdB64
                )
            )
            .onAppear {
                // Same defensive teardown as the anon branch — no live
                // session runtime should linger while we await the
                // phrase.
                deviceGroupSync.clear()
            }
        case .saveFreshPhrase:
            // ADR-0017 lost-everything fallback — the user minted a
            // brand-new identity (already published, no proof); show the
            // new recovery phrase for them to save before the session lands.
            SaveFreshPhraseScreen(
                phrase: authRepository.freshPhrase() ?? "",
                onConfirm: { try? authRepository.confirmFreshPhraseSaved() }
            )
            .onAppear { deviceGroupSync.clear() }
        }
    }
}

/// Authenticated-state router — local toggle between ``HomeScreen``
/// and the per-feature destinations that ship over 1.14e/f.
///
/// 1.14e4 introduces the first two-state toggle (Home / Contacts);
/// 1.14e9 adds InviteLanding; 1.14f adds Chats + per-Chat. The
/// pattern mirrors Android's `SessionRoot` from 1.15d6 + the
/// `SessionDestination` sealed hierarchy.
private struct SessionRoot: View {
    let account: SessionAccount
    let authRepository: AuthRepository
    let contactsRepository: ContactsRepository
    let chatRepository: ChatRepository
    @EnvironmentObject var deviceGroupSync: DeviceGroupSyncRepository
    @ObservedObject var inviteIntake: InviteIntake

    @StateObject private var contactsVM: ContactsViewModel
    @StateObject private var chatListVM: ChatListViewModel

    @State private var destination: SessionDestination = .home

    init(
        account: SessionAccount,
        authRepository: AuthRepository,
        contactsRepository: ContactsRepository,
        chatRepository: ChatRepository,
        inviteIntake: InviteIntake
    ) {
        self.account = account
        self.authRepository = authRepository
        self.contactsRepository = contactsRepository
        self.chatRepository = chatRepository
        self.inviteIntake = inviteIntake
        // Wire the chat bridge on the contacts repository so
        // refreshInbox can forward Welcomes + acceptInviteLink can
        // drive startGroupWith. The assignment is idempotent on
        // body re-evaluations (same `chatRepository` reference
        // each time).
        contactsRepository.chatRepository = chatRepository
        self._contactsVM = StateObject(
            wrappedValue: ContactsViewModel(
                repository: contactsRepository,
                chatRepository: chatRepository
            )
        )
        self._chatListVM = StateObject(
            wrappedValue: ChatListViewModel(repository: chatRepository)
        )
    }

    var body: some View {
        Group {
            // Pending invite slug takes priority over the
            // persistent destination toggle — when the user taps
            // a deep link mid-session, the landing screen shows up
            // over whatever they were doing.
            if let slug = inviteIntake.pendingSlug {
                InviteLandingScreen(
                    viewModel: InviteLandingViewModel(
                        slug: slug,
                        repository: contactsRepository,
                        intake: inviteIntake
                    ),
                    onDone: {
                        // InviteLandingViewModel.dismiss() already
                        // called intake.clear() — the publisher
                        // flips to nil + this view tree rebuild
                        // lands us back on the prior destination.
                    }
                )
            } else {
                switch destination {
                case .home:
                    HomeScreen(
                        account: account,
                        onOpenContacts: { destination = .contacts },
                        onOpenChats: { destination = .chats },
                        onOpenPasskeys: { destination = .passkeys },
                        onOpenDevices: { destination = .devices },
                        onLogout: authRepository.logout,
                        onForgetDevice: authRepository.forgetDevice
                    )
                case .contacts:
                    ContactsScreen(
                        viewModel: contactsVM,
                        onBack: { destination = .home },
                        onOpenChat: { groupIdHex in
                            destination = .chat(groupIdHex: groupIdHex)
                        }
                    )
                case .chats:
                    ChatListScreen(
                        viewModel: chatListVM,
                        onBack: { destination = .home },
                        onOpenChat: { groupIdHex in
                            destination = .chat(groupIdHex: groupIdHex)
                        }
                    )
                case .chat(let groupIdHex):
                    NavigationStack {
                        ChatScreen(
                            viewModel: ChatViewModel(
                                repository: chatRepository,
                                groupIdHex: groupIdHex,
                                sessionTokenProvider: {
                                    authRepository.identitySecrets?.sessionToken
                                }
                            ),
                            onBack: { destination = .chats }
                        )
                    }
                case .passkeys:
                    PasskeysView(
                        viewModel: PasskeysViewModel(
                            authRepository: authRepository
                        ),
                        onBack: { destination = .home }
                    )
                case .devices:
                    DevicesScreen(
                        authRepository: authRepository,
                        onBack: { destination = .home }
                    )
                }
            }
        }
        // Bootstrap the ChatRepository whenever the auth
        // repository has fresh `identitySecrets`. The publisher
        // fires on every login/signup/recover; bootstrap is
        // idempotent for the same account id, so re-running on a
        // body re-evaluation is harmless. **NOT** populated on a
        // session-restore from disk — the export key isn't
        // persisted, so the user has to re-login for MLS-side
        // operations to work. The chat screens render but
        // `sendMessage` will throw with a clear error until the
        // bootstrap lands.
        .task(id: authRepository.identitySecrets?.accountId) {
            if let secrets = authRepository.identitySecrets {
                chatRepository.bootstrap(secrets: secrets)
                // Phase 2a3e2b — wire the cross-device sync
                // runtime alongside the chat bootstrap so the
                // poll loop starts at the same moment the chat
                // surfaces become usable.
                deviceGroupSync.bootstrap(secrets: secrets)
            }
        }
    }
}

private enum SessionDestination: Equatable {
    case home
    case contacts
    case chats
    case chat(groupIdHex: String)
    case passkeys
    case devices
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
