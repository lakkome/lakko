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
    @StateObject private var chatRepository: ChatRepository

    /// Process-scoped 1x1 call host. Claims the repository's call-signaling
    /// feed and is presented from the signed-in root, so an incoming call rings
    /// on every screen and a call survives leaving its chat.
    @StateObject private var callHost: CallHost

    /// Process-scoped group (SFU) call host — the group call outlives the chat
    /// it was started from and rekeys on every epoch advance, whatever screen
    /// is showing.
    @StateObject private var groupCallHost: GroupCallHost

    /// Process-scoped call-link (ADR-0032) call host — the call outlives its
    /// landing screen.
    @StateObject private var linkCallHost: LinkCallHost

    /// Process-scoped foreground receive for every group whose chat is not
    /// open. Without it nothing but the open chat was ever polled.
    @StateObject private var foregroundReceive: ForegroundReceiveHolder

    /// Process-scoped ``DeviceGroupSyncRepository``
    /// (Phase 2a3e2b-iOS). Holds the per-session DeviceGroup
    /// handle + reactive maps for cross-device drafts /
    /// flags / read cursors / conversation order.
    @StateObject private var deviceGroupSync = DeviceGroupSyncRepository()

    /// Process-scoped ``InviteIntake`` — populated by
    /// `.onOpenURL` when iOS hands us a Universal Link.
    @StateObject private var inviteIntake = InviteIntake()

    /// Scene lifecycle — used to schedule a background contact-inbox refresh
    /// (G-29) when the app leaves the foreground.
    @Environment(\.scenePhase) private var scenePhase

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

        // ADR-0047 — decrypted media is written to a per-chat temp directory
        // that is removed when the chat closes; a crash skips that, so remove
        // anything left behind before any chat can open.
        MediaTempFiles.sweepStale()

        // `@StateObject` cannot reference another `@StateObject` in
        // its default initializer, so we hand-roll the wiring
        // inside `init`: build the AuthRepository first, then feed
        // it into ContactsRepository's init.
        let auth = AuthRepository()
        self._authRepository = StateObject(wrappedValue: auth)
        self._contactsRepository = StateObject(
            wrappedValue: ContactsRepository(authRepository: auth)
        )
        let chat = ChatRepository()
        let host = CallHost.live(repository: chat)
        host.attach(to: chat)
        let groupHost = GroupCallHost.live(
            repository: chat,
            sessionTokenProvider: { [weak auth] in auth?.identitySecrets?.sessionToken }
        )
        groupHost.attach(to: chat)
        let linkHost = LinkCallHost.live(authRepository: auth)
        // One call at a time, across the three kinds.
        CallSlotWiring.wire(oneToOne: host, group: groupHost, link: linkHost)
        self._chatRepository = StateObject(wrappedValue: chat)
        self._callHost = StateObject(wrappedValue: host)
        self._groupCallHost = StateObject(wrappedValue: groupHost)
        self._linkCallHost = StateObject(wrappedValue: linkHost)
        self._foregroundReceive = StateObject(
            wrappedValue: ForegroundReceiveHolder(
                scheduler: ForegroundReceiveScheduler.live(
                    repository: chat, callHost: host, groupCallHost: groupHost)
            )
        )
    }

    var body: some Scene {
        WindowGroup {
            AppRoot()
                .environmentObject(authRepository)
                .environmentObject(contactsRepository)
                .environmentObject(chatRepository)
                .environmentObject(callHost)
                .environmentObject(groupCallHost)
                .environmentObject(linkCallHost)
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
                // ADR-0026 Phase 4 — apply the persisted anti-censorship
                // preference at boot: when on, fetch + verify the signed
                // bootstrap config and publish the armed ambient config so the
                // clients route through the obfuscated tunnel. Disarmed when
                // off, so this is a no-op by default. Runs once on AppRoot
                // appear; the routine is idempotent.
                .task {
                    AntiCensorshipManager.shared.armFromSettings()
                }
                // ADR-0041 Phase 1 — one-time, idempotent App Group
                // storage migration for the rehydrated session account, so
                // the notification-service extension can read the five
                // decrypt inputs. Copy-not-move + never-delete-old +
                // read-new-with-fallback-to-old, so an un-provisioned build
                // or an interrupted migration degrades to today's behaviour
                // rather than stranding the foreground app. No-op when
                // there is no session. Idempotent — safe every launch.
                .task {
                    if case let .session(account) = authRepository.state {
                        SharedStorageMigration.runIfNeeded(accountIdB64: account.accountIdB64)
                    }
                }
                // G-29 — queue a background contact-inbox refresh as we leave
                // the foreground, so a request that arrives while backgrounded
                // is picked up by the next opportunistic `BGAppRefreshTask`
                // wake (handler below) rather than waiting for a reopen. The
                // foreground long-poll loop covers the active case.
                .onChange(of: scenePhase) { _, phase in
                    if phase == .background {
                        ContactBackgroundRefresh.schedule()
                    }
                    // The foreground receive runs only while the app is active;
                    // becoming active again sweeps at once, so an offer that
                    // arrived while backgrounded rings if it is still fresh.
                    foregroundReceive.scheduler.setForeground(phase == .active)
                }
                .task {
                    foregroundReceive.scheduler.start()
                    foregroundReceive.scheduler.setForeground(scenePhase == .active)
                }
        }
        // G-29 — `BGAppRefreshTask` handler. iOS launches this opportunistically;
        // run one quick (non-long-poll) inbox fetch, then chain the next
        // request. `refreshInbox` is a no-op when there is no live session, so
        // an unauthenticated wake costs nothing.
        .backgroundTask(.appRefresh(ContactBackgroundRefresh.taskIdentifier)) {
            await contactsRepository.refreshInbox()
            ContactBackgroundRefresh.schedule()
        }
    }
}

/// Top-level router. Subscribes to ``AuthRepository/state`` via
/// `@EnvironmentObject` and picks the destination subtree.
struct AppRoot: View {
    @EnvironmentObject private var authRepository: AuthRepository
    @EnvironmentObject private var contactsRepository: ContactsRepository
    @EnvironmentObject private var chatRepository: ChatRepository
    @EnvironmentObject private var callHost: CallHost
    @EnvironmentObject private var groupCallHost: GroupCallHost
    @EnvironmentObject private var linkCallHost: LinkCallHost
    @EnvironmentObject private var deviceGroupSync: DeviceGroupSyncRepository
    @EnvironmentObject private var inviteIntake: InviteIntake

    /// ADR-0038 S4 — the "update available" notice controller. `@StateObject`
    /// wrapping the process-wide singleton so the banner mount below observes its
    /// published ``UpdateBannerDecision``.
    @StateObject private var releaseChecker = ReleaseUpdateChecker.shared

    var body: some View {
        content
            // ADR-0038 S4 — fetch + verify the signed release manifest at launch
            // (throttled to once per 24h internally), then publish the banner
            // decision. Dormant when no release key is pinned; fail-safe toward
            // showing no banner on any fetch / verify failure. Runs once on
            // AppRoot appear (view identity is stable across auth-state flips).
            .task {
                await releaseChecker.checkOnLaunch()
            }
            // ADR-0037 §6 (G-11) — a rolled-back / tampered backup was refused
            // at restore. User-visible alarm; the served (older / altered) state
            // was NOT loaded.
            .alert(
                "Backup could not be restored safely",
                isPresented: Binding(
                    get: { authRepository.backupRollbackAlarm != nil },
                    set: { if !$0 { authRepository.dismissBackupRollbackAlarm() } }
                ),
                presenting: authRepository.backupRollbackAlarm
            ) { _ in
                Button("OK", role: .cancel) {
                    authRepository.dismissBackupRollbackAlarm()
                }
            } message: { _ in
                Text(
                    "The server returned an altered or outdated backup. Your "
                    + "account is safe; we continued without restoring the older "
                    + "data."
                )
            }
            // ADR-0037 §6 (G-11) — a stale but authentic backup needs an
            // explicit override. The restore flow suspends until the user
            // decides; Cancel (or dismiss) aborts the restore (fail-closed).
            .confirmationDialog(
                "Restore an older backup?",
                isPresented: Binding(
                    get: { authRepository.pendingStaleBackupOverride != nil },
                    set: { if !$0 { authRepository.resolveStaleBackupOverride(false) } }
                ),
                titleVisibility: .visible,
                presenting: authRepository.pendingStaleBackupOverride
            ) { _ in
                Button("Restore anyway", role: .destructive) {
                    authRepository.resolveStaleBackupOverride(true)
                }
                Button("Cancel", role: .cancel) {
                    authRepository.resolveStaleBackupOverride(false)
                }
            } message: { request in
                Text(
                    "This backup is older than expected (\(request.reason)). "
                    + "Restoring it will roll your account back to older state — "
                    + "older messages, and devices you removed may reappear."
                )
            }
    }

    @ViewBuilder private var content: some View {
        switch authRepository.state {
        case .loading:
            // Practically invisible — rehydrate is synchronous. We
            // render a blank background so a slow tap on the launch
            // screen doesn't flash white.
            Color(.systemBackground)
                .ignoresSafeArea()
        case .anon(let cachedUsername):
            Group {
                if let callSlug = inviteIntake.pendingCallJoinSlug {
                    // ADR-0032 (G-21/G-22) — a tapped `/c/<slug>` multi-joiner
                    // call link from an anonymous visitor. Guest join
                    // (`signupGuest`) is offered inline; no login required.
                    JoinCallLandingScreen(
                        viewModel: JoinCallLandingViewModel(
                            slug: callSlug,
                            authRepository: authRepository,
                            intake: inviteIntake,
                            linkCallHost: linkCallHost
                        ),
                        onDone: {}
                    )
                } else {
                    AnonRoot(
                        repository: authRepository,
                        cachedUsername: cachedUsername
                    )
                }
            }
            // Pending contact / group invites land in
            // `InviteIntake` but cannot consume in the anon state —
            // those flows need `identitySecrets`. We leave the slug
            // parked; once the user signs in, the session-state
            // branch picks it up. (A `/c/` call link is the
            // exception — it offers a guest signup inline, above.)
            .onAppear {
                // Defensive: returning to anon (logout /
                // forgetDevice) tears down the cross-device sync
                // runtime so the next login bootstraps fresh.
                deviceGroupSync.clear()
                // ADR-0048 — no account, no own profile to publish.
                ProfileDirectory.shared.deactivate()
                // No session, no call: end anything live and forget the
                // session's call bookkeeping.
                callHost.reset()
                groupCallHost.reset()
                linkCallHost.reset()
            }

        case .session(let account):
            VStack(spacing: 0) {
                // ADR-0017 defence-in-depth — surface the identity-changed
                // security alert above the session content.
                if authRepository.identityAlert != nil {
                    IdentitySecurityBanner(
                        onDismiss: { authRepository.dismissIdentityAlert() }
                    )
                }
                // ADR-0038 S4 — the "update available" notice, mounted BELOW the
                // security banner (severity ordering: security alert first, the
                // informational update notice second — UX §3). Shown only when the
                // verified manifest resolves to an actionable update.
                if case .shown(let showUpdateButton) = releaseChecker.decision {
                    UpdateAvailableBanner(
                        showUpdateButton: showUpdateButton,
                        onUpdate: { releaseChecker.openStore() },
                        onDismiss: { releaseChecker.dismiss() }
                    )
                }
                SessionRoot(
                    account: account,
                    authRepository: authRepository,
                    contactsRepository: contactsRepository,
                    chatRepository: chatRepository,
                    callHost: callHost,
                    groupCallHost: groupCallHost,
                    linkCallHost: linkCallHost,
                    inviteIntake: inviteIntake
                )
            }
            // Every call, app-wide: the 1x1 ringing banner and a call link's
            // status above every signed-in screen, and the one full-screen call
            // (1x1, group or link) over them.
            .callHostOverlay(
                oneToOne: callHost,
                group: groupCallHost,
                link: linkCallHost,
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
/// Keeps one `ChatViewModel` per open chat alive across `SessionRoot` body
/// re-evaluations. Not `@Published`: swapping the model is never a reason to
/// re-render on its own, the destination change that opens a chat already is.
@MainActor
private final class ChatViewModelHolder: ObservableObject {
    private var current: (groupIdHex: String, viewModel: ChatViewModel)?

    func viewModel(for groupIdHex: String, make: () -> ChatViewModel) -> ChatViewModel {
        if let current, current.groupIdHex == groupIdHex {
            return current.viewModel
        }
        let made = make()
        current = (groupIdHex, made)
        return made
    }
}

private struct SessionRoot: View {
    let account: SessionAccount
    let authRepository: AuthRepository
    let contactsRepository: ContactsRepository
    let chatRepository: ChatRepository
    let callHost: CallHost
    let groupCallHost: GroupCallHost
    let linkCallHost: LinkCallHost
    @EnvironmentObject var deviceGroupSync: DeviceGroupSyncRepository
    @ObservedObject var inviteIntake: InviteIntake

    @StateObject private var contactsVM: ContactsViewModel
    @StateObject private var chatListVM: ChatListViewModel
    /// The open chat's view model, held OUTSIDE `body`. `SessionRoot` re-renders
    /// on every publish of the observed repositories (a draft broadcast, a read
    /// cursor, the sync poll), and a `ChatViewModel` built inline in `body`
    /// was replaced on each of them, taking the composer's draft with it.
    @StateObject private var chatVMHolder = ChatViewModelHolder()

    @State private var destination: SessionDestination = .home

    init(
        account: SessionAccount,
        authRepository: AuthRepository,
        contactsRepository: ContactsRepository,
        chatRepository: ChatRepository,
        callHost: CallHost,
        groupCallHost: GroupCallHost,
        linkCallHost: LinkCallHost,
        inviteIntake: InviteIntake
    ) {
        self.account = account
        self.authRepository = authRepository
        self.contactsRepository = contactsRepository
        self.chatRepository = chatRepository
        self.callHost = callHost
        self.groupCallHost = groupCallHost
        self.linkCallHost = linkCallHost
        self.inviteIntake = inviteIntake
        // Wire the chat bridge on the contacts repository so
        // refreshInbox can forward Welcomes + acceptInviteLink can
        // drive startGroupWith. The assignment is idempotent on
        // body re-evaluations (same `chatRepository` reference
        // each time).
        contactsRepository.chatRepository = chatRepository
        // ADR-0050 section 2.4 — the accounts the re-admission check can look
        // up: every accepted contact whose account id is known.
        chatRepository.readmissionContactsProvider = { [weak contactsRepository] in
            (contactsRepository?.state.accepted ?? []).compactMap { contact in
                guard let accountId = contact.request.fromAccountId, accountId.count == 16 else {
                    return nil
                }
                return (personKey: contact.request.fromPubkey, accountId: accountId)
            }
        }
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
            } else if let groupSlug = inviteIntake.pendingGroupJoinSlug {
                // ADR-0028 (G-3) — a tapped `/g/<slug>` group join-ticket link.
                // The landing screen requests to join + polls for the admin's
                // approval; dismiss() flips the publisher back to nil.
                JoinGroupLandingScreen(
                    viewModel: JoinGroupLandingViewModel(
                        slug: groupSlug,
                        repository: chatRepository,
                        intake: inviteIntake
                    ),
                    onDone: {}
                )
            } else if let callSlug = inviteIntake.pendingCallJoinSlug {
                // ADR-0032 (G-21/G-22) — a tapped `/c/<slug>` multi-joiner call
                // link while signed in. The landing decodes the capability then
                // hosts (owner) or joins (relays its KeyPackage + waits for the
                // committer's Welcome); dismiss() clears the parked slug.
                JoinCallLandingScreen(
                    viewModel: JoinCallLandingViewModel(
                        slug: callSlug,
                        authRepository: authRepository,
                        intake: inviteIntake,
                        linkCallHost: linkCallHost
                    ),
                    onDone: {}
                )
            } else {
                switch destination {
                case .home:
                    HomeScreen(
                        account: account,
                        onOpenContacts: { destination = .contacts },
                        onOpenChats: { destination = .chats },
                        onOpenChannels: { destination = .channels },
                        onOpenPasskeys: { destination = .passkeys },
                        onOpenDevices: { destination = .devices },
                        onOpenAntiCensorship: { destination = .antiCensorship },
                        onOpenProfile: { destination = .profile },
                        onLogout: authRepository.logout,
                        onForgetDevice: {
                            // ADR-0050 section 2.10 — take this device's
                            // KeyPackage out of the directory BEFORE the wipe,
                            // while the bearer token still exists: the
                            // account's other devices retire this device's
                            // leaves when their scheduled check sees the row
                            // gone.
                            Task { @MainActor in
                                if let own = chatRepository.ownDeviceLeafKey {
                                    await chatRepository.deleteDirectoryKeyPackage(deviceKey: own)
                                }
                                authRepository.forgetDevice()
                            }
                        }
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
                case .channels:
                    // ADR-0010 — broadcast channels. `ChannelsRootScreen`
                    // needs the canonical AccountIdentity (phrase) seed to
                    // build the MLS PublisherGroup (same seed the chat +
                    // devices surfaces read from `identitySecrets`). A
                    // session always carries the seed now (the relaunch
                    // restore lands it too); the hint is a defensive
                    // fallback rather than presenting a screen that cannot
                    // publish.
                    if let seed = authRepository.identitySecrets?.accountIdentitySeed {
                        ChannelsRootScreen(
                            accountIdentitySeed: seed,
                            sessionTokenProvider: {
                                authRepository.identitySecrets?.sessionToken
                            },
                            onBack: { destination = .home }
                        )
                    } else {
                        VStack(spacing: 16) {
                            Text("Sign in again to use Channels.")
                                .foregroundStyle(.secondary)
                            Button("Back") { destination = .home }
                        }
                        .padding()
                    }
                case .chat(let groupIdHex):
                    NavigationStack {
                        ChatScreen(
                            viewModel: chatVMHolder.viewModel(for: groupIdHex) {
                                ChatViewModel(
                                    repository: chatRepository,
                                    groupIdHex: groupIdHex,
                                    callHost: callHost,
                                    groupCallHost: groupCallHost,
                                    sessionTokenProvider: {
                                        authRepository.identitySecrets?.sessionToken
                                    },
                                    resolveRecipient: { username in
                                        try await contactsRepository
                                            .resolveVerifiedRecipient(username: username)
                                    }
                                )
                            },
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
                        onBack: { destination = .home },
                        onDeviceAdded: { deviceKey in
                            // ADR-0050 section 2.8 — admit the new device to
                            // every chat once its KeyPackage is published.
                            Task { @MainActor in
                                do {
                                    let tally = try await chatRepository.admitDevice(deviceKey: deviceKey)
                                    NSLog("ADR-0050 admit device: offered %d, skipped %d, failed %d",
                                          tally.offered, tally.skipped, tally.failed)
                                } catch {
                                    NSLog("ADR-0050 admit device: fan-out failed: \(error)")
                                }
                            }
                        },
                        onDeviceRemoved: { deviceKey in
                            // ADR-0050 section 2.10 — retire the removed device
                            // from every chat, then from the directory.
                            Task { @MainActor in
                                await chatRepository.retireDevice(deviceKey: deviceKey)
                            }
                        }
                    )
                case .profile:
                    // ADR-0048 — Settings -> Profile. A saved name is
                    // published into every group this device is in.
                    ProfileSettingsScreen(
                        viewModel: ProfileSettingsViewModel(
                            publishToAllGroups: { await chatRepository.publishOwnProfileToAllGroups() }
                        ),
                        onBack: { destination = .home }
                    )
                case .antiCensorship:
                    NavigationStack {
                        AntiCensorshipSettingsView(
                            manager: AntiCensorshipManager.shared,
                            onBack: { destination = .home }
                        )
                    }
                }
            }
        }
        // Bootstrap the ChatRepository whenever the auth
        // repository has `identitySecrets`. The publisher fires on
        // every login/signup/recover AND on a relaunch that restored
        // the session from the Keychain (`AuthRepository.rehydrate`),
        // so this runs at every session start; bootstrap is
        // idempotent for the same account id, so re-running on a
        // body re-evaluation is harmless.
        .task(id: authRepository.identitySecrets?.accountId) {
            if let secrets = authRepository.identitySecrets {
                // ADR-0048 — the profile directory first: the chat bootstrap
                // can join groups, and a join publishes the own profile.
                ProfileDirectory.shared.activate(
                    accountIdB64: base64Encode(secrets.accountId),
                    username: account.username
                )
                chatRepository.bootstrap(secrets: secrets)
                // Publish a KeyPackage minted on the long-lived identity
                // the bootstrap just built, so a Welcome sent to it can be
                // opened here. Every session start: discovery holds one
                // KeyPackage per account and a Welcome consumes it. Fire
                // and forget — it persists before uploading and coalesces
                // with the republish every Welcome join requests.
                chatRepository.republishKeyPackage()
                // Phase 2a3e2b — wire the cross-device sync
                // runtime alongside the chat bootstrap so the
                // poll loop starts at the same moment the chat
                // surfaces become usable.
                deviceGroupSync.bootstrap(secrets: secrets)
            }
        }
    }
}

/// Owns the foreground receive scheduler for the process. A class, so the
/// `App` can hold it as a `@StateObject` and it is built exactly once.
@MainActor
final class ForegroundReceiveHolder: ObservableObject {
    let scheduler: ForegroundReceiveScheduler

    init(scheduler: ForegroundReceiveScheduler) {
        self.scheduler = scheduler
    }
}

private enum SessionDestination: Equatable {
    case home
    case contacts
    case chats
    case channels
    case chat(groupIdHex: String)
    case passkeys
    case devices
    case antiCensorship
    case profile
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
