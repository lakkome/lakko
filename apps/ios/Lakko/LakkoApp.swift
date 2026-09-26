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
///   * ``AuthState/anon(cachedUsername:)`` — ``AnonRoot``, a
///     `NavigationStack` rooted at the Welcome screen with Log in, Create
///     account and Recover pushed on it.
///   * ``AuthState/session(_:)`` — ``SessionRoot`` (the tab shell).
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
    @StateObject private var deviceGroupSync: DeviceGroupSyncRepository

    /// Process-scoped ``InviteIntake`` — populated by
    /// `.onOpenURL` when iOS hands us a Universal Link.
    @StateObject private var inviteIntake: InviteIntake

    /// `docs/ux/calls.md` section 2.1 — the adapter between the 1x1 call and
    /// the phone's own call machinery, so an incoming call rings over the lock
    /// screen, is answerable there, and lands in the call history. Held for the
    /// process lifetime (it is what keeps the Combine subscription on
    /// ``CallHost`` alive); nothing observes it, so it is a plain reference
    /// rather than a `@StateObject`.
    private let callKitBridge: CallKitBridge

    /// The same objects as plain references, for the shell views that hand
    /// them down without observing them (see ``AppServices``). Built once in
    /// `init` from the very instances the `@StateObject`s above own.
    private let services: AppServices

    /// Scene lifecycle — used to schedule a background contact-inbox refresh
    /// (G-29) when the app leaves the foreground.
    @Environment(\.scenePhase) private var scenePhase

    init() {
        // Before every other side effect here, because `RTCFieldTrials.h` says
        // "Must be called before any other call into WebRTC" and the SFrame
        // probe below is the first thing that touches the framework. Without
        // the generic frame descriptor this installs, a frame-encrypted video
        // stream never assembles a frame at the receiver — see
        // ``WebRTCFieldTrials`` for the sources that establish that.
        WebRTCFieldTrials.installOnce()
        // TEMPORARY, see ``WebRTCDiagnosticLogging``: libwebrtc announces both
        // frame-decryptor attachment gates at info level, which iOS suppresses.
        WebRTCDiagnosticLogging.installOnce()
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
        let chat = ChatRepository()
        let host = CallHost.live(
            repository: chat,
            sessionTokenProvider: { [weak auth] in auth?.identitySecrets?.sessionToken }
        )
        host.attach(to: chat)
        let groupHost = GroupCallHost.live(
            repository: chat,
            sessionTokenProvider: { [weak auth] in auth?.identitySecrets?.sessionToken }
        )
        groupHost.attach(to: chat)
        let linkHost = LinkCallHost.live(authRepository: auth)
        // One call at a time, across the three kinds.
        CallSlotWiring.wire(oneToOne: host, group: groupHost, link: linkHost)
        // `docs/ux/calls.md` section 2.1 — report the 1x1 call to the system,
        // and let the system's Answer / End / Mute drive the same host entry
        // points the in-app buttons use. Built here, next to the host it
        // adapts; the `CXProvider` and the call registry it talks to are
        // process-wide (see ``CallKitProvider``).
        self.callKitBridge = CallKitBridge(host: host)
        // A VoIP push rings the phone with a placeholder before anything is
        // decrypted (the payload carries no caller — invariants #2 and #5).
        // This is the other half: fetch the journal row the wake points at, so
        // the offer arrives on the ordinary receive path and the system screen
        // is corrected with the real caller and call kind.
        VoipPushRegistrar.shared.onWake = { [weak chat] wake in
            guard let chat, let wakeGroupHex = wake.groupHex else { return }
            // The wake's `g` is the ADR-0042 opaque routing id; the DS journal
            // is keyed on the real group id. A miss falls back to reading `g`
            // as a group id, exactly as the notification extension does.
            let groupIdHex = PushRoutingStore()
                .lookupRoute(pushGidHex: wakeGroupHex.lowercased())?.groupIdHex
                ?? wakeGroupHex.lowercased()
            Task { @MainActor [weak chat] in
                try? await chat?.pollGroup(groupIdHex: groupIdHex, waitSecs: 0)
            }
        }
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
        let sync = DeviceGroupSyncRepository()
        let intake = InviteIntake()
        self._deviceGroupSync = StateObject(wrappedValue: sync)
        self._inviteIntake = StateObject(wrappedValue: intake)
        // Wire the chat bridge on the contacts repository so refreshInbox can
        // forward Welcomes + acceptInviteLink can drive startGroupWith. Both
        // repositories live for the process, so this is wired once, here,
        // rather than on every evaluation of a view init.
        let contacts = ContactsRepository(authRepository: auth)
        self._contactsRepository = StateObject(wrappedValue: contacts)
        contacts.chatRepository = chat
        // ADR-0050 section 2.4 — the accounts the re-admission check can look
        // up: every accepted contact whose account id is known.
        chat.readmissionContactsProvider = { [weak contacts] in
            (contacts?.state.accepted ?? []).compactMap { contact in
                guard let accountId = contact.request.fromAccountId, accountId.count == 16 else {
                    return nil
                }
                return (personKey: contact.request.fromPubkey, accountId: accountId)
            }
        }
        self.services = AppServices(
            authRepository: auth,
            contactsRepository: contacts,
            chatRepository: chat,
            callHost: host,
            groupCallHost: groupHost,
            linkCallHost: linkHost,
            deviceGroupSync: sync,
            inviteIntake: intake
        )
    }

    var body: some Scene {
        WindowGroup {
            // A UI test launched with `-LakkoUITestFixture <name>` gets that
            // fixture INSTEAD of the app, so an XCUITest can reach a real
            // production screen without signing in. DEBUG only — see
            // ``UiTestFixture`` for why testing through login would be testing
            // the deployment rather than the screen.
#if DEBUG
            if let fixture = UiTestFixture.requested {
                UiTestFixtureRoot(fixture: fixture)
            } else {
                appRoot
            }
#else
            appRoot
#endif
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

    /// The real root of the app. Extracted from ``body`` unchanged so the
    /// fixture branch above has something to be an alternative to; every
    /// modifier below is exactly where it was.
    ///
    /// This view tree observes every `@StateObject` above, so it runs on each
    /// repository publish; `.equatable()` keeps that from re-evaluating the
    /// router, whose only input is the bundle.
    @ViewBuilder
    private var appRoot: some View {
        AppRoot(services: services)
            .equatable()
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
                if phase == .active {
                    // `docs/ux/calls.md` section 2.1 — refresh the names the
                    // notification extension may have to put on a ringing
                    // lock screen while this app is not running.
                    services.chatRepository.mirrorCallPeerNames()
                }
            }
            .task {
                foregroundReceive.scheduler.start()
                foregroundReceive.scheduler.setForeground(scenePhase == .active)
            }
    }
}

/// Top-level router. Observes ``AuthRepository/state`` and picks the
/// destination subtree.
///
/// Observes ONLY what its own body reads: the auth repository (the state
/// switch, the two backup alarms) and the invite intake (the anonymous `/c/`
/// branch). Every other repository is reached through ``AppServices`` and
/// never observed here — the chat repository publishes on every poll and the
/// sync repository on every draft broadcast, and an `@EnvironmentObject` for
/// either re-evaluated this body, and the whole tab shell under it, each time.
struct AppRoot: View, Equatable {
    let services: AppServices
    @ObservedObject private var authRepository: AuthRepository
    @ObservedObject private var inviteIntake: InviteIntake

    nonisolated static func == (lhs: AppRoot, rhs: AppRoot) -> Bool {
        lhs.services === rhs.services
    }

    /// ADR-0038 S4 — the "update available" notice controller. `@StateObject`
    /// wrapping the process-wide singleton so the banner mount below observes its
    /// published ``UpdateBannerDecision``.
    @StateObject private var releaseChecker = ReleaseUpdateChecker.shared

    /// The recovery phrase a sign-up just generated, until the user has
    /// confirmed three of its words (UX spec section 5.6). Sign-up lands the
    /// session inside the repository call, which replaces ``AnonRoot`` at
    /// the next render, so the phrase step is shown from here, over the
    /// session, rather than from the sign-up screen. Held in memory only and
    /// dropped on confirm.
    @State private var pendingSignupPhrase: String?

    init(services: AppServices) {
        self.services = services
        self._authRepository = ObservedObject(wrappedValue: services.authRepository)
        self._inviteIntake = ObservedObject(wrappedValue: services.inviteIntake)
    }

    /// Settings > Appearance (UX spec section 4): System / Light / Dark,
    /// persisted per device. Applied once here so every screen and every
    /// sheet follows it; Calls override to dark on their own screen.
    @AppStorage(AppearanceSetting.storageKey)
    private var appearanceRaw = AppearanceSetting.system.rawValue

    private var appearance: AppearanceSetting {
        AppearanceSetting(rawValue: appearanceRaw) ?? .system
    }

    var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("AppRoot")
#endif
        content
            .preferredColorScheme(appearance.colorScheme)
            // Design system section 1 — the brand accent, applied at the root
            // so the tab bar, buttons, toggles, pickers and links follow it
            // regardless of the catalog's global accent.
            .tint(Color.lakkoAccent)
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
            Color.lakkoBackground
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
                            linkCallHost: services.linkCallHost
                        ),
                        onDone: {}
                    )
                } else {
                    AnonRoot(
                        repository: authRepository,
                        cachedUsername: cachedUsername,
                        onSignupPhrase: { pendingSignupPhrase = $0 }
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
                services.deviceGroupSync.clear()
                // ADR-0048 — no account, no own profile to publish.
                ProfileDirectory.shared.deactivate()
                // No session, no call: end anything live and forget the
                // session's call bookkeeping.
                services.callHost.reset()
                services.groupCallHost.reset()
                services.linkCallHost.reset()
                // …and with it anything the phone is still showing as a call,
                // plus the names mirrored for a lock screen that now belongs to
                // no account (``CallPeerNameMirror``) and the muted set that
                // decided which of its calls were allowed to ring
                // (``ConversationMuteMirror``) — left behind, it would silence
                // the next account's calls in a conversation it never had.
                CallKitProvider.shared.endAll()
                CallKitCallRegistry.shared.reset()
                CallPeerNameMirror.clearAll()
                ConversationMuteMirror.clearAll()
            }

        case .session(let account) where pendingSignupPhrase != nil:
            // The sign-up phrase step: the session is live, but the user has
            // not yet confirmed the phrase. Nothing else of the session
            // mounts until they have.
            SignupPhraseScreen(
                phrase: pendingSignupPhrase ?? "",
                onDone: { pendingSignupPhrase = nil }
            )
            .id(account.accountIdB64)

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
                // `.equatable()` — the shell re-evaluates on its own state
                // (tab, paths), on the intake it observes, and when the
                // account changes; never merely because this router did.
                SessionRoot(account: account, services: services)
                    .equatable()
            }
            // Every call, app-wide: the 1x1 ringing banner and a call link's
            // status above every signed-in screen, and the one full-screen call
            // (1x1, group or link) over them. The modifier observes the hosts
            // itself.
            .callHostOverlay(
                oneToOne: services.callHost,
                group: services.groupCallHost,
                link: services.linkCallHost,
                inviteIntake: inviteIntake
            )

        case .needsRecoveryPhrase(let account):
            // ADR-0017 fresh-device fail-safe. The user authenticated
            // (password / passkey) but this device has no cached
            // AccountIdentity (phrase) seed — the canonical identity
            // cannot be rebuilt without the phrase. We render the
            // restore-phrase prompt instead of the tab shell; supplying
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
                services.deviceGroupSync.clear()
            }
        case .saveFreshPhrase:
            // ADR-0017 lost-everything fallback — the user minted a
            // brand-new identity (already published, no proof); show the
            // new recovery phrase for them to save before the session lands.
            SaveFreshPhraseScreen(
                phrase: authRepository.freshPhrase() ?? "",
                onConfirm: { try? authRepository.confirmFreshPhraseSaved() }
            )
            .onAppear { services.deviceGroupSync.clear() }
        }
    }
}

/// The four primary destinations (`docs/ux/navigation.md` section 1), in tab
/// order.
///
/// Groups and Channels are gone as DESTINATIONS. Both remain as kinds of
/// conversation inside Chats, and both keep their creation flows, reached from
/// the create menu; the channel screens are pushed destinations of the Chats
/// stack. Calls is back, with the recent-call history it never had.
enum SessionTab: Hashable {
    case chats
    case contacts
    case calls
    case settings
}

/// Every screen a tab can push. One route enum for all five stacks, so the
/// chat screen is the same destination whether it was reached from Chats,
/// Groups or Contacts, and back always returns to where it was opened from.
enum TabRoute: Hashable {
    case chat(groupIdHex: String)
    /// The contact picker behind the create menu's New chat.
    case newChat
    /// The create menu's Create group (see ``NewGroupScreen``).
    case newGroup
    /// A channel this device holds, opened from the merged conversation list.
    case channel(channelId: String)
    /// A channel the network lookup found by `@handle`, which this device may
    /// not hold yet — the reader screen offers Subscribe.
    case channelFound(channelId: String, handle: String)
    /// The create menu's Create channel.
    case newChannel
    /// ADR-0028 OQ3 — the joiner side of a co-publisher link.
    case joinCoPublisher
    /// A contact's detail (UX spec section 5.3), keyed on the accepted
    /// request's mailbox id — the same key the contacts list uses.
    case contactDetail(messageId: Int64)
    /// The safety number with that contact, pushed from the detail.
    case safetyNumber(messageId: Int64)
    case profile
    case passkeys
    case devices
    case addDevice
    case recoveryFactors
    case antiCensorship
    /// Deep links (`/i/`, `/g/`, `/c/`), pushed over the current tab rather
    /// than replacing the app (UX spec section 3).
    case inviteLanding(slug: String)
    case groupJoinLanding(slug: String)
    case callJoinLanding(slug: String)

    /// The chat this route shows, if it is a chat.
    var chatGroupIdHex: String? {
        if case .chat(let id) = self { return id }
        return nil
    }
}

/// The view models the signed-in shell owns: the two shared list models and
/// one `ChatViewModel` per OPEN chat, for as long as some tab's stack shows
/// that chat.
///
/// Held by `SessionRoot` as ONE `@StateObject` that never publishes. A
/// `@StateObject` both owns and OBSERVES its object, so `ChatListViewModel`
/// held directly as a `@StateObject` re-ran the whole shell body on every
/// chat poll (it republishes `threads` from the repository state), and
/// `ContactsViewModel` on every contacts publish. Wrapped here, each list
/// model is observed only by the screens that render it (`ChatListScreen`,
/// `ContactsScreen`, `ContactPickerScreen` take them as `@ObservedObject`).
///
/// The chat models: `SessionRoot` builds every pushed screen inside `body`
/// (the `navigationDestination` closures), and `body` still runs on every
/// navigation change — a tab switch, a push on another tab, a deep link. A
/// `ChatViewModel` built inline there would be replaced on each of those,
/// taking the composer's draft with it; a `@StateObject` cannot live inside a
/// destination closure. So the model's lifetime is keyed here to "open on a
/// stack" instead of to a body evaluation, and released by
/// `reconcileAfterNavigation` once no stack shows the chat any more.
///
/// Nothing here is `@Published`: swapping a chat model is never a reason to
/// re-render on its own, the navigation change that opens a chat already is.
///
/// Chat models are keyed by group id because a chat can be open on more than
/// one tab's stack at once (the same conversation pushed from Chats and from
/// Contacts, or two different chats on two tabs); a single "current" slot
/// would rebuild the other chat's model on the next navigation and take its
/// draft with it.
@MainActor
private final class SessionModels: ObservableObject {
    let contactsVM: ContactsViewModel
    let chatListVM: ChatListViewModel
    /// ADR-0010 channels. Hoisted out of the Channels tab's root screen, which
    /// used to own it as a `@StateObject`: with Channels gone as a destination
    /// the merged conversation list and the pushed channel screens have to read
    /// ONE store, and a `@StateObject` on a screen belongs to that screen.
    let channelsState: ChannelsUiState
    private var chatModels: [String: ChatViewModel] = [:]

    init(services: AppServices, accountIdB64: String) {
        contactsVM = ContactsViewModel(
            repository: services.contactsRepository,
            chatRepository: services.chatRepository
        )
        chatListVM = ChatListViewModel(repository: services.chatRepository)
        // Bound at construction, so the persisted channel list and publisher
        // keys are loaded before the first body evaluation.
        channelsState = ChannelsUiState(accountIdB64: accountIdB64)
    }

    func chatViewModel(for groupIdHex: String, make: () -> ChatViewModel) -> ChatViewModel {
        if let existing = chatModels[groupIdHex] {
            return existing
        }
        let made = make()
        chatModels[groupIdHex] = made
        return made
    }

    /// Drop the models of chats no stack shows any more.
    func retainChats(only open: Set<String>) {
        chatModels = chatModels.filter { open.contains($0.key) }
    }
}

/// Authenticated-state shell: a bottom `TabView` of Chats, Groups, Channels,
/// Contacts and Settings, each tab owning its own `NavigationStack` (UX spec
/// sections 2 and 3). The app opens on Chats; there is no home screen.
///
/// Its inputs are the account and the ``AppServices`` bundle, and it is
/// `Equatable` on exactly those (identity for the bundle), so a re-evaluation
/// of `AppRoot` does not re-run this body. What does re-run it: its own state
/// (the selected tab, the five paths) and the invite intake it observes for
/// deep links. No repository and no list model is observed here — a draft
/// broadcast, a read cursor, a sync poll or a chat poll used to re-evaluate
/// this whole shell, and with it every `navigationDestination` under it.
private struct SessionRoot: View, Equatable {
    let account: SessionAccount
    let services: AppServices
    @ObservedObject var inviteIntake: InviteIntake

    private var authRepository: AuthRepository { services.authRepository }
    private var contactsRepository: ContactsRepository { services.contactsRepository }
    private var chatRepository: ChatRepository { services.chatRepository }

    /// The shell's view models, in one never-publishing holder — see
    /// ``SessionModels`` for why neither the list models nor a chat model may
    /// be a `@StateObject` of this view directly.
    @StateObject private var models: SessionModels
    private var contactsVM: ContactsViewModel { models.contactsVM }
    private var chatListVM: ChatListViewModel { models.chatListVM }
    private var channelsState: ChannelsUiState { models.channelsState }

    @State private var selectedTab: SessionTab = .chats
    @State private var chatsPath: [TabRoute] = []
    @State private var contactsPath: [TabRoute] = []
    @State private var callsPath: [TabRoute] = []
    @State private var settingsPath: [TabRoute] = []

    init(account: SessionAccount, services: AppServices) {
        self.account = account
        self.services = services
        self._inviteIntake = ObservedObject(wrappedValue: services.inviteIntake)
        self._models = StateObject(
            wrappedValue: SessionModels(
                services: services, accountIdB64: account.accountIdB64))
    }

    nonisolated static func == (lhs: SessionRoot, rhs: SessionRoot) -> Bool {
        lhs.account == rhs.account && lhs.services === rhs.services
    }

    var body: some View {
#if DEBUG
        let _ = BodyEvaluationCounter.tick("SessionRoot")
#endif
        TabView(selection: $selectedTab) {
            NavigationStack(path: $chatsPath) {
                ChatListScreen(
                    viewModel: chatListVM,
                    channels: channelsState,
                    onOpen: { openConversation($0) },
                    onCreate: { create($0) },
                    onFoundChannel: { channelId, handle in
                        push(.channelFound(channelId: channelId, handle: handle), on: .chats)
                    },
                    onFoundPerson: { handOffToContacts(username: $0) }
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .chats) }
            }
            .tabItem { Label("Chats", systemImage: "bubble.left") }
            .tag(SessionTab.chats)

            NavigationStack(path: $contactsPath) {
                ContactsScreen(viewModel: contactsVM)
                    .navigationDestination(for: TabRoute.self) { routeView($0, tab: .contacts) }
            }
            .tabItem { Label("Contacts", systemImage: "person.crop.circle") }
            .tag(SessionTab.contacts)

            NavigationStack(path: $callsPath) {
                CallsScreen(
                    history: CallHistoryStore.shared,
                    conversations: chatListVM,
                    onNewCall: { await startNewCall() },
                    onOpenConversation: { groupIdHex, _ in
                        push(.chat(groupIdHex: groupIdHex), on: .calls)
                    },
                    onCallBack: { groupIdHex, kind, video in
                        callBack(groupIdHex: groupIdHex, kind: kind, video: video)
                    },
                    onOpenLink: { inviteIntake.handle($0) }
                )
                // Calls placed before the index existed live only in their
                // conversations. Filled once per account, so this screen never
                // opens empty while the conversation list next to it is
                // visibly showing calls.
                .task { services.chatRepository.backfillCallHistory() }
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .calls) }
            }
            .tabItem { Label("Calls", systemImage: "phone") }
            .tag(SessionTab.calls)

            NavigationStack(path: $settingsPath) {
                SettingsScreen(
                    account: account,
                    authRepository: authRepository,
                    onOpen: { settingsPath.append($0) },
                    onLogout: authRepository.logout,
                    onForgetDevice: forgetDevice
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .settings) }
            }
            .tabItem { Label("Settings", systemImage: "gearshape") }
            .tag(SessionTab.settings)
        }
        // Deep links: park on the current tab's stack when a slug arrives
        // (a tap while signed in), and pick up one already parked (a tap
        // while anonymous that the sign-in just completed).
        .onAppear { pushPendingDeepLinks() }
        // The inbox long-poll for the whole session, not one screen: contact
        // requests, acknowledgements and the Welcomes that create chats and
        // groups arrive on the account and device mailboxes, and a user who
        // never opens Contacts must still see a group they were added to.
        // SwiftUI cancels the task with the session root (logout, forget).
        .task {
            while !Task.isCancelled {
                let started = Date()
                await contactsRepository.refreshInbox(
                    waitSecs: ContactsViewModel.defaultLongPollSecs)
                // A long-poll holds the connection itself; a poll that returned
                // at once (no secrets yet, a refused request) must not spin.
                if Date().timeIntervalSince(started) < 1 {
                    try? await Task.sleep(nanoseconds: 2_000_000_000)
                }
                await Task.yield()
            }
        }
        .onChange(of: inviteIntake.pendingSlug) { _, _ in pushPendingDeepLinks() }
        .onChange(of: inviteIntake.pendingGroupJoinSlug) { _, _ in pushPendingDeepLinks() }
        .onChange(of: inviteIntake.pendingCallJoinSlug) { _, _ in pushPendingDeepLinks() }
        // A landing popped with the back gesture (not its own Done) drops the
        // parked slug, so the next tap of the same link opens it again; and a
        // chat popped from every stack releases its view model.
        .onChange(of: allPaths) { old, new in
            reconcileAfterNavigation(old: old, new: new)
        }
        // Bootstrap the ChatRepository for the signed-in account. Keyed on the
        // account this shell was built for (an input, so it is current on
        // every evaluation); every path that lands `.session` adopts
        // `identitySecrets` first, so the read below sees them. Runs on every
        // login/signup/recover AND on a relaunch that restored the session from
        // the Keychain; bootstrap is idempotent for the same account id.
        // Keep the encrypted backup current. Until this ran, the blob was
        // written exactly twice in an account's life - an empty baseline at
        // signup and one at recover - so a restore on a new device landed the
        // person back at registration day. Inert while nothing has changed;
        // see `Backup/AutoBackup.swift` for what counts as a change.
        .task(id: account.accountIdB64) {
            let driver = AutoBackupDriver(inputs: .init(
                accountId: { authRepository.identitySecrets?.accountId },
                accountIdB64: { authRepository.identitySecrets.map { base64Encode($0.accountId) } },
                sessionToken: { authRepository.identitySecrets?.sessionToken },
                accountIdentitySeed: { authRepository.identitySecrets?.accountIdentitySeed },
                // `nil` until the chat layer has bootstrapped, so an empty
                // list during startup is never mistaken for "left every group".
                knownGroupIds: {
                    chatRepository.state.bootstrapped
                        ? chatRepository.state.threads.map(\.groupIdHex)
                        : nil
                },
                snapshot: { chatRepository.currentStorageSnapshot() }
            ))
            driver.start()
            // The task is cancelled when the account changes or the shell goes
            // away; the loop observes that, and this stops the timer with it.
            defer { driver.stop() }
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_000_000_000)
            }
        }
        .task(id: account.accountIdB64) {
            if let secrets = authRepository.identitySecrets {
                // ADR-0048 — the profile directory first: the chat bootstrap
                // can join groups, and a join publishes the own profile.
                ProfileDirectory.shared.activate(
                    accountIdB64: base64Encode(secrets.accountId),
                    username: account.username
                )
                // `docs/ux/navigation.md` section 6.2 — the flat Calls index.
                // Bound here for the same reason the profile directory is: the
                // writer is the call hosts' record port, built once at the
                // composition root, and it has to know whose calls it is
                // filing before the first one ends.
                CallHistoryStore.shared.bind(accountIdB64: account.accountIdB64)
                chatRepository.bootstrap(secrets: secrets)
                // `docs/ux/calls.md` section 2.1 — the names a background
                // process may have to show on a ringing lock screen.
                chatRepository.mirrorCallPeerNames()
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
                services.deviceGroupSync.bootstrap(secrets: secrets)
            }
        }
    }

    // MARK: - The conversation list's four actions

    /// Open a row, routed by the ROW's own kind rather than by which list it
    /// came from: one list holds all three, so there is no list kind left to
    /// ask.
    private func openConversation(_ row: ConversationListRow) {
        switch row.kind {
        case .channel:
            push(.channel(channelId: row.id), on: .chats)
        case .chat, .group:
            push(.chat(groupIdHex: row.id), on: .chats)
        }
    }

    /// The three create-menu items (`docs/ux/navigation.md` section 3).
    private func create(_ choice: ChatListScreen.CreateChoice) {
        switch choice {
        case .chat: push(.newChat, on: .chats)
        case .group: push(.newGroup, on: .chats)
        case .channel: push(.newChannel, on: .chats)
        }
    }

    /// Section 4.3 — a person the lookup found hands their name to Contacts,
    /// which is where the contact request lives, because that flow asks for an
    /// introduction the search field has no room for.
    private func handOffToContacts(username: String) {
        contactsVM.prefillAddByUsername(username)
        selectedTab = .contacts
    }

    /// Section 5.1 — call back in the SAME mode the row records, through the
    /// host that owns that kind of call.
    private func callBack(groupIdHex: String, kind: CallHistoryKind, video: Bool) {
        switch kind {
        case .group:
            services.groupCallHost.start(groupIdHex: groupIdHex, video: video, joinCallId: nil)
        case .chat:
            services.callHost.startOutgoingCall(
                groupIdHex: groupIdHex, button: CallMediaChoice.button(video: video))
        }
    }

    /// `docs/ux/call-links.md` section 2 — `New call` founds a group video
    /// call and lands the person in it. Nothing is asked first, and no link is
    /// made: one is minted from inside the call if somebody presses `Invite`.
    private func startNewCall() async -> String? {
        switch await services.linkCallHost.startNewCall() {
        case .started, .alreadyInThisCall:
            return nil
        case .busy(let message), .failed(let message):
            return message
        }
    }

    /// ADR-0017: the canonical AccountIdentity (phrase) seed a channel screen
    /// needs to build the MLS PublisherGroup. A session always carries it (the
    /// relaunch restore lands it too); an absent seed presents one line rather
    /// than a screen that cannot publish.
    private var accountIdentitySeed: Data? {
        authRepository.identitySecrets?.accountIdentitySeed
    }

    @ViewBuilder
    private func channelSeedMissing(_ title: String) -> some View {
        VStack(spacing: 16) {
            Text("Sign in again to use channels.")
                .foregroundStyle(.secondary)
        }
        .padding()
        .navigationTitle(title)
    }

    // MARK: - Routes

    @ViewBuilder
    private func routeView(_ route: TabRoute, tab: SessionTab) -> some View {
        switch route {
        case .chat(let groupIdHex):
            ChatScreen(
                viewModel: chatViewModel(for: groupIdHex),
                contacts: contactsVM,
                onBack: { pop(route, from: tab) },
                // One contact card: the chat's header opens the same pushed
                // screen the Contacts list does (`docs/ux/conversation-info.md`
                // section 3, "Except a person").
                onOpenContact: { messageId in push(.contactDetail(messageId: messageId), on: tab) }
            )
            // UX spec section 5.2 — a pushed chat fills the screen; the tab
            // bar returns with the list.
            .toolbar(.hidden, for: .tabBar)
        case .newChat:
            ContactPickerScreen(
                viewModel: contactsVM,
                title: "New chat",
                onPick: { groupIdHex in
                    replaceTop(route, with: .chat(groupIdHex: groupIdHex), in: tab)
                }
            )
        case .newGroup:
            // docs/ux/new-group.md — a name (ADR-0051, shared with every
            // member) above a checkable contact list; the group is founded with
            // the first pick and the rest are added through the chat's normal
            // add path, then the chat opens.
            NewGroupScreen(
                contacts: contactsVM,
                chatRepository: chatRepository,
                onCreated: { groupIdHex, notice in
                    let model = chatViewModel(for: groupIdHex)
                    // UX spec section 5.1 — made through the new-group flow:
                    // a group, even while it still has two people.
                    model.markConversationAsGroup()
                    if let notice { model.noteSetupNotice(notice) }
                    replaceTop(route, with: .chat(groupIdHex: groupIdHex), in: tab)
                }
            )
        case .channel(let channelId):
            // ADR-0010 — a channel opened from the merged conversation list.
            // Pushed on the Chats stack: Channels is no longer a destination,
            // so the screens it used to root now live under the list that holds
            // the row.
            if let seed = accountIdentitySeed {
                ChannelDetailScreen(
                    channelId: channelId,
                    state: channelsState,
                    client: ChannelsClient(),
                    accountIdentitySeed: seed)
            } else {
                channelSeedMissing("Channel")
            }
        case .channelFound(let channelId, let handle):
            // A channel the ONE network lookup resolved by `@handle`. This
            // device may hold no seed for it yet, so the reader screen offers
            // Subscribe.
            if let seed = accountIdentitySeed {
                ChannelDetailScreen(
                    channelId: channelId,
                    state: channelsState,
                    client: ChannelsClient(),
                    accountIdentitySeed: seed,
                    foundHandle: handle)
            } else {
                channelSeedMissing("Channel")
            }
        case .newChannel:
            if let seed = accountIdentitySeed {
                NewChannelScreen(
                    state: channelsState,
                    client: ChannelsClient(),
                    accountIdentitySeed: seed,
                    sessionTokenProvider: {
                        authRepository.identitySecrets?.sessionToken
                    },
                    onJoinAsCoPublisher: { push(.joinCoPublisher, on: tab) })
            } else {
                channelSeedMissing("New channel")
            }
        case .joinCoPublisher:
            if let seed = accountIdentitySeed {
                JoinCoPublisherScreen(state: channelsState, accountIdentitySeed: seed)
            } else {
                channelSeedMissing("Join as co-publisher")
            }
        case .contactDetail(let messageId):
            ContactDetailScreen(
                viewModel: contactsVM,
                messageId: messageId,
                // Message returns to the chat when the card was opened from
                // it, and opens the chat otherwise: one card, one control.
                onOpenChat: { groupIdHex in returnToOrPush(.chat(groupIdHex: groupIdHex), on: tab) },
                onCall: { groupIdHex, video in
                    // The 1x1 entry point the chat header uses
                    // (`docs/ux/calls.md` sections 3.1 and 3.2): the phone
                    // button dials there and then, the camera button opens the
                    // camera check first. No preparation screen either way for
                    // a voice call.
                    services.callHost.startOutgoingCall(
                        groupIdHex: groupIdHex, button: CallMediaChoice.button(video: video))
                }
            )
        case .safetyNumber(let messageId):
            SafetyNumberScreen(viewModel: contactsVM, messageId: messageId)
        case .profile:
            // ADR-0048 — Settings -> Profile. A saved name is
            // published into every group this device is in.
            ProfileSettingsScreen(
                viewModel: ProfileSettingsViewModel(
                    publishToAllGroups: { await chatRepository.publishOwnProfileToAllGroups() }
                )
            )
        case .passkeys:
            PasskeysView(
                viewModel: PasskeysViewModel(authRepository: authRepository)
            )
        case .devices:
            DevicesScreen(
                authRepository: authRepository,
                onAddDevice: { push(.addDevice, on: tab) },
                onDeviceRemoved: { deviceKey in
                    // ADR-0050 section 2.10 — retire the removed device
                    // from every chat, then from the directory.
                    Task { @MainActor in
                        await chatRepository.retireDevice(deviceKey: deviceKey)
                    }
                }
            )
        case .addDevice:
            AddDeviceScreen(
                authRepository: authRepository,
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
                }
            )
        case .recoveryFactors:
            RecoveryFactorsScreen(authRepository: authRepository)
        case .antiCensorship:
            AntiCensorshipSettingsView(manager: AntiCensorshipManager.shared)
        case .inviteLanding(let slug):
            InviteLandingScreen(
                viewModel: InviteLandingViewModel(
                    slug: slug,
                    repository: contactsRepository,
                    intake: inviteIntake
                ),
                onDone: { pop(route, from: tab) }
            )
        case .groupJoinLanding(let slug):
            // ADR-0028 (G-3) — a tapped `/g/<slug>` group join-ticket link.
            // The landing screen requests to join + polls for the admin's
            // approval; dismiss() flips the publisher back to nil.
            JoinGroupLandingScreen(
                viewModel: JoinGroupLandingViewModel(
                    slug: slug,
                    repository: chatRepository,
                    intake: inviteIntake
                ),
                onDone: { pop(route, from: tab) }
            )
        case .callJoinLanding(let slug):
            // ADR-0032 (G-21/G-22) — a tapped `/c/<slug>` multi-joiner call
            // link while signed in. The landing decodes the capability then
            // hosts (owner) or joins (relays its KeyPackage + waits for the
            // committer's Welcome); dismiss() clears the parked slug.
            JoinCallLandingScreen(
                viewModel: JoinCallLandingViewModel(
                    slug: slug,
                    authRepository: authRepository,
                    intake: inviteIntake,
                    linkCallHost: services.linkCallHost
                ),
                onDone: { pop(route, from: tab) }
            )
        }
    }

    private func chatViewModel(for groupIdHex: String) -> ChatViewModel {
        models.chatViewModel(for: groupIdHex) {
            let services = services
            let model = ChatViewModel(
                repository: services.chatRepository,
                groupIdHex: groupIdHex,
                callHost: services.callHost,
                groupCallHost: services.groupCallHost,
                sessionTokenProvider: {
                    services.authRepository.identitySecrets?.sessionToken
                },
                resolveRecipient: { username in
                    try await services.contactsRepository
                        .resolveVerifiedRecipient(username: username)
                }
            )
            // Phase 2a3e2b — the cross-device draft / read-cursor sync is
            // attached where the model is made, once per model, so the chat
            // screen does not need to observe the sync repository to reach it.
            model.attachSync(services.deviceGroupSync)
            return model
        }
    }

    /// ADR-0050 section 2.10 — take this device's KeyPackage out of the
    /// directory BEFORE the wipe, while the bearer token still exists: the
    /// account's other devices retire this device's leaves when their
    /// scheduled check sees the row gone.
    private func forgetDevice() {
        // This device's calls go with the device. The transcript rows in each
        // conversation are the message log's to wipe, not this index's.
        CallHistoryStore.shared.clear()
        Task { @MainActor in
            if let own = chatRepository.ownDeviceLeafKey {
                await chatRepository.deleteDirectoryKeyPackage(deviceKey: own)
            }
            authRepository.forgetDevice()
        }
    }

    // MARK: - Path plumbing

    private var allPaths: [[TabRoute]] {
        [chatsPath, contactsPath, callsPath, settingsPath]
    }

    private func path(for tab: SessionTab) -> Binding<[TabRoute]> {
        switch tab {
        case .chats: return $chatsPath
        case .contacts: return $contactsPath
        case .calls: return $callsPath
        case .settings: return $settingsPath
        }
    }

    /// Pop back to `route` when it is already on `tab`'s stack, else push it.
    /// What lets the contact card's Message be the same control wherever the
    /// card was opened from: reached from the chat it goes back; reached from
    /// the list it goes forward.
    private func returnToOrPush(_ route: TabRoute, on tab: SessionTab) {
        let binding = path(for: tab)
        if let index = binding.wrappedValue.lastIndex(of: route) {
            binding.wrappedValue = Array(binding.wrappedValue.prefix(index + 1))
        } else {
            push(route, on: tab)
        }
    }

    private func push(_ route: TabRoute, on tab: SessionTab) {
        path(for: tab).wrappedValue.append(route)
    }

    /// Remove `route` from `tab`'s stack (the last occurrence, and everything
    /// above it). A no-op when it is not there, so a landing's Done after a
    /// back gesture cannot pop an unrelated screen.
    private func pop(_ route: TabRoute, from tab: SessionTab) {
        let binding = path(for: tab)
        guard let index = binding.wrappedValue.lastIndex(of: route) else { return }
        binding.wrappedValue.removeSubrange(index...)
    }

    /// Swap the picker for the chat it picked, so back from the chat returns
    /// to the list rather than to the picker.
    private func replaceTop(_ route: TabRoute, with replacement: TabRoute, in tab: SessionTab) {
        let binding = path(for: tab)
        if let index = binding.wrappedValue.lastIndex(of: route) {
            binding.wrappedValue.removeSubrange(index...)
        }
        binding.wrappedValue.append(replacement)
    }

    /// Push a landing for every parked deep link that no stack shows yet, on
    /// the tab the user is looking at.
    private func pushPendingDeepLinks() {
        var wanted: [TabRoute] = []
        if let slug = inviteIntake.pendingSlug { wanted.append(.inviteLanding(slug: slug)) }
        if let slug = inviteIntake.pendingGroupJoinSlug { wanted.append(.groupJoinLanding(slug: slug)) }
        if let slug = inviteIntake.pendingCallJoinSlug { wanted.append(.callJoinLanding(slug: slug)) }
        let shown = Set(allPaths.flatMap { $0 })
        for route in wanted where !shown.contains(route) {
            push(route, on: selectedTab)
        }
    }

    private func reconcileAfterNavigation(old: [[TabRoute]], new: [[TabRoute]]) {
        let before = Set(old.flatMap { $0 })
        let after = Set(new.flatMap { $0 })
        for route in before.subtracting(after) {
            switch route {
            case .inviteLanding(let slug) where inviteIntake.pendingSlug == slug:
                inviteIntake.clear()
            case .groupJoinLanding(let slug) where inviteIntake.pendingGroupJoinSlug == slug:
                inviteIntake.clearGroupJoin()
            case .callJoinLanding(let slug) where inviteIntake.pendingCallJoinSlug == slug:
                inviteIntake.clearCallJoin()
            default:
                break
            }
        }
        models.retainChats(only: Set(after.compactMap(\.chatGroupIdHex)))
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

/// Anonymous-state router (UX spec section 5.6): a `NavigationStack`
/// rooted at ``WelcomeScreen``, with Log in, Create account and Recover
/// pushed on it so the system back button and edge swipe work everywhere.
/// A returning device (a cached username) starts on Log in with Welcome
/// underneath.
///
/// Mirror of `apps/android`'s `AnonRoot` + the web's `routes.tsx` `Anon`
/// group.
private struct AnonRoot: View {
    /// The three VMs are owned via `@StateObject` so they survive
    /// view-tree rebuilds (which happen any time the outer
    /// ``AuthRepository`` publishes). Constructing them inside the
    /// destinations would orphan in-flight async Tasks the moment SwiftUI
    /// re-runs `body`.
    @StateObject private var signupVM: SignupViewModel
    @StateObject private var loginVM: LoginViewModel
    @StateObject private var recoverVM: RecoverViewModel

    @State private var path: [AnonRoute]

    init(
        repository: AuthRepository,
        cachedUsername: String?,
        onSignupPhrase: @escaping (String) -> Void
    ) {
        self._signupVM = StateObject(
            wrappedValue: SignupViewModel(repository: repository, onPhraseReady: onSignupPhrase)
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
        // A returning user lands on Log in; a fresh device on Welcome.
        self._path = State(initialValue: cachedUsername == nil ? [] : [.login])
    }

    var body: some View {
        NavigationStack(path: $path) {
            WelcomeScreen(
                onCreateAccount: { path = [.signup] },
                onLogIn: { path = [.login] }
            )
            .navigationDestination(for: AnonRoute.self) { route in
                switch route {
                case .signup:
                    SignupScreen(
                        viewModel: signupVM,
                        onNavigateToLogin: { path = [.login] }
                    )
                case .login:
                    LoginScreen(
                        viewModel: loginVM,
                        onNavigateToSignup: { path = [.signup] },
                        onNavigateToRecover: { path.append(.recover) }
                    )
                case .recover:
                    RecoverScreen(
                        viewModel: recoverVM,
                        onNavigateToLogin: { path = [.login] },
                        onNavigateToSignup: { path = [.signup] }
                    )
                }
            }
        }
        .tint(Color.lakkoAccent)
    }
}

/// The screens the anonymous stack can push over Welcome.
private enum AnonRoute: Hashable {
    case signup
    case login
    case recover
}
