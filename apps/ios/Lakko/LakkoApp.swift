// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
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

    /// ADR-0049 section 2 (amended 2026-09-28) — the OLD device's side of
    /// "confirm on another device": looks for an open sign-in request when the
    /// app becomes active and shows the code modal.
    @StateObject private var deviceLinkResponder: DeviceLinkResponderFlow

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
        VoipPushRegistrar.shared.onWake = { [weak chat] target in
            // The registrar has already checked the wake against the account
            // binding (ADR-0042, amended 2026-09-30) and reverse-mapped its
            // opaque `push_gid` for that account. The chat repository serves
            // the signed-in session; a target for any other account is not
            // polled.
            guard let chat,
                  AcceptedContactsStore.boundAccountIdB64 == target.accountIdB64 else { return }
            Task { @MainActor [weak chat] in
                try? await chat?.pollGroup(groupIdHex: target.groupIdHex, waitSecs: 0)
            }
        }
        self._chatRepository = StateObject(wrappedValue: chat)
        self._callHost = StateObject(wrappedValue: host)
        self._groupCallHost = StateObject(wrappedValue: groupHost)
        self._linkCallHost = StateObject(wrappedValue: linkHost)
        // `docs/ux/feed.md` — the Feed's "Couldn't check for new messages."
        // state, fed by every check (the group sweep below, the channel post
        // sync and pull to refresh). In memory, per account.
        let feedRefresh = FeedRefreshCenter()
        self._foregroundReceive = StateObject(
            wrappedValue: ForegroundReceiveHolder(
                scheduler: ForegroundReceiveScheduler.live(
                    repository: chat, callHost: host, groupCallHost: groupHost,
                    feedRefresh: feedRefresh)
            )
        )
        let sync = DeviceGroupSyncRepository()
        // ADR-0060 section 7 — a local change of the "who can find me"
        // setting or of a reach-key version goes to the account's other
        // devices over the DeviceGroup.
        ReachCenter.shared.onDeviceEnvelope = { [weak sync] envelope in
            Task { @MainActor in await sync?.sendServiceEnvelope(envelope) }
        }
        // ADR-0048 section 4a — the own name saved or cleared here goes to the
        // account's other devices the same way, through the sync runtime's own
        // DeviceGroup handle.
        ProfileDirectory.shared.onOwnDeviceEnvelope = { [weak sync] envelope in
            Task { @MainActor in await sync?.sendServiceEnvelope(envelope) }
        }
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
        // ADR-0065 phase 2 (UX 16) — the contact facts the chat layer reads
        // (which keys a reset retired, which contact a message's new keys
        // belong to), and the one hook every identity fetch reports a
        // confirmed reset through.
        chat.contactKeyFactsProvider = { [weak contacts] in
            (contacts?.state.accepted ?? []).compactMap { contact in
                guard let accountIdB64 = contact.accountIdB64 else { return nil }
                return ChatRepository.ContactKeyFacts(
                    accountIdB64: accountIdB64,
                    messageId: contact.request.messageId,
                    currentKeyB64: base64Encode(contact.request.fromPubkey),
                    keyReset: contact.keyReset)
            }
        }
        ContactsClient.keyResetSink = { [weak contacts] accountId, previousKeyB64, pendingKeyB64 in
            Task { @MainActor in
                contacts?.noteKeyReset(
                    accountId: accountId, previousKeyB64: previousKeyB64, pendingKeyB64: pendingKeyB64)
            }
        }
        self._deviceLinkResponder = StateObject(wrappedValue: DeviceLinkResponderFlow(
            ports: .init(
                relay: DeviceLinkClient(),
                sessionToken: { [weak auth] in auth?.identitySecrets?.sessionToken },
                ownHandle: { [weak auth] in auth.flatMap { DeviceSession.current($0)?.loginHandle } },
                checkPayload: { payload, handle in
                    _ = try PairingCodec.parsePairingQrPayload(
                        wireBytes: payload, expectedLoginHandle: handle)
                },
                runPairing: { [weak auth, weak chat, weak sync] payload in
                    guard let auth, let session = DeviceSession.current(auth),
                          let handle = session.loginHandle else {
                        throw AuthClientError.argument("no session")
                    }
                    let token = session.sessionToken
                    // The hand-over carries the entropy of the recovery
                    // phrase this device holds (recovery payload v2); with
                    // no phrase stored here no keys are sent.
                    let recovery = PairingOrchestrator.ScanRecovery(
                        accountIdB64: session.accountIdB64,
                        deviceAuthorizeStart: {
                            try await AuthClient().deviceAuthorizeStart(sessionToken: token)
                        }
                    )
                    let scanned = try await PairingOrchestrator.runScanFlow(
                        scannedBytes: payload,
                        ownLoginHandle: handle,
                        accountIdBytes: session.accountIdBytes,
                        accountIdB64: session.accountIdB64,
                        recovery: recovery
                    )
                    // The add ran on a DeviceGroup handle of its own: the sync
                    // runtime reloads from the snapshot the pairing persisted,
                    // as the QR route does (`AddDeviceScreen.onDeviceAdded`).
                    // Without it, the long-lived handle stays an epoch behind,
                    // cannot read what the new device sends, and its next
                    // persist rolls the stored DeviceGroup back (seen live
                    // 2026-09-29: WrongEpoch on the new web device's folder).
                    if let secrets = auth.identitySecrets {
                        await MainActor.run { [weak sync] in
                            sync?.reloadAfterLocalMembershipChange(secrets: secrets)
                        }
                    }
                    // ADR-0050 section 2.8 — admit the new device to every chat.
                    let deviceKey = scanned.newDeviceKey
                    Task { @MainActor [weak chat] in
                        _ = try? await chat?.admitDevice(deviceKey: deviceKey)
                    }
                    return scanned.keysSent
                }
            )
        ))
        let services = AppServices(
            authRepository: auth,
            contactsRepository: contacts,
            chatRepository: chat,
            callHost: host,
            groupCallHost: groupHost,
            linkCallHost: linkHost,
            deviceGroupSync: sync,
            inviteIntake: intake,
            feedRefresh: feedRefresh
        )
        self.services = services
        // ADR-0065 section 3.5 — the signed-out screen's automatic wipe is the
        // full forget-device wipe: the runtimes let go first, then every
        // store, then the session ends (``AppServices/wipeAccountLocalData``).
        auth.wipeAfterKeyReset = { [weak services] accountIdB64 in
            services?.wipeAfterKeyReset(accountIdB64: accountIdB64)
        }
        // ADR-0065 section 2.3 step 5 — after a reset this device's KeyPackage
        // is re-published from the chat repository's long-lived identity,
        // bound by the NEW account key.
        auth.keyResetHooks.republishOwnKeyPackage = { [weak chat] secrets in
            guard let chat else { throw SessionNotReady() }
            try await chat.republishKeyPackageAfterKeyReset(secrets: secrets)
        }
    }

    var body: some Scene {
        WindowGroup {
            // ADR-0061 — the interface language: locale + layout direction
            // for everything below, switched live (no restart).
            LocalizedRoot {
                windowContent
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

    /// What the window shows, wrapped in ``LocalizedRoot`` by ``body``.
    @ViewBuilder
    private var windowContent: some View {
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
            // ADR-0061 section 5 — signed translation updates: use the bundle
            // kept from an earlier run if it still verifies, then fetch a
            // newer one on the release-notice cadence (once a day, through
            // the tunnel when armed). Dormant without a pinned key; any
            // failure keeps the current translations. Never blocks launch.
            .task {
                await TranslationUpdater.shared.check()
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
                    // ADR-0061 section 5 — the same throttled check on return
                    // to the foreground; a no-op until a day has passed.
                    Task { await TranslationUpdater.shared.check() }
                    // `docs/ux/calls.md` section 2.1 — refresh the names the
                    // notification extension may have to put on a ringing
                    // lock screen while this app is not running.
                    services.chatRepository.mirrorCallPeerNames()
                }
            }
            // ADR-0061 — the mirrored names carry words of the interface
            // language (a nameless group's title, the placeholder), so a
            // language switch rewrites them rather than leaving the lock
            // screen in the old language until the next activation.
            .onReceive(Localization.shared.$language.removeDuplicates()) { _ in
                services.chatRepository.mirrorCallPeerNames()
            }
            .task {
                foregroundReceive.scheduler.start()
                foregroundReceive.scheduler.setForeground(scenePhase == .active)
            }
            .task(id: scenePhase) {
                // ADR-0049 section 2 — a sign-in request waiting for this
                // device's answer. Polled for as long as the app is active,
                // not only when it becomes active: a request usually arrives
                // while this device is open in front of the person.
                guard scenePhase == .active else { return }
                while !Task.isCancelled {
                    await deviceLinkResponder.check()
                    try? await Task.sleep(nanoseconds: DeviceLinkResponderFlow.foregroundPollNanos)
                }
            }
            .sheet(isPresented: Binding(
                get: { deviceLinkResponder.modal != nil },
                set: { if !$0 { deviceLinkResponder.dismiss() } }
            )) {
                DeviceLinkRequestModal(flow: deviceLinkResponder)
                    .presentationDetents([.medium])
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
    /// ADR-0061 — the two backup alarms below read the catalog, so they
    /// follow a language switch (a rare event; nothing else here re-renders).
    @ObservedObject private var l10n = Localization.shared

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

    /// Mirror of ``DeviceGroupSyncRepository/deviceRemoved`` (UX spec
    /// second-device section 3.1). Received rather than observed: observing
    /// the sync repository would re-run this body on every draft and read
    /// cursor it publishes (see ``AppServices``).
    @State private var deviceRemoved = false

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
            .onReceive(services.deviceGroupSync.$deviceRemoved.removeDuplicates()) { removed in
                if deviceRemoved != removed { deviceRemoved = removed }
            }
            // ADR-0037 §6 (G-11) — a rolled-back / tampered backup was refused
            // at restore. User-visible alarm; the served (older / altered) state
            // was NOT loaded.
            .alert(
                l10n.t("backup.rollback_refused.title"),
                isPresented: Binding(
                    get: { authRepository.backupRollbackAlarm != nil },
                    set: { if !$0 { authRepository.dismissBackupRollbackAlarm() } }
                ),
                presenting: authRepository.backupRollbackAlarm
            ) { _ in
                Button(l10n.t("common.dismiss"), role: .cancel) {
                    authRepository.dismissBackupRollbackAlarm()
                }
            } message: { _ in
                Text(l10n.t("backup.rollback_refused.body"))
            }
            // ADR-0037 §6 (G-11) — a stale but authentic backup needs an
            // explicit override. The restore flow suspends until the user
            // decides; Cancel (or dismiss) aborts the restore (fail-closed).
            .confirmationDialog(
                l10n.t("backup.stale_restore.title"),
                isPresented: Binding(
                    get: { authRepository.pendingStaleBackupOverride != nil },
                    set: { if !$0 { authRepository.resolveStaleBackupOverride(false) } }
                ),
                titleVisibility: .visible,
                presenting: authRepository.pendingStaleBackupOverride
            ) { _ in
                Button(l10n.t("backup.stale_restore.confirm"), role: .destructive) {
                    authRepository.resolveStaleBackupOverride(true)
                }
                Button(l10n.t("common.cancel"), role: .cancel) {
                    authRepository.resolveStaleBackupOverride(false)
                }
            } message: { request in
                Text(l10n.t("backup.stale_restore.body", texts: [
                    "reason": request.reason,
                    "consequence": l10n.t("backup.stale_restore.consequence"),
                ]))
            }
    }

    @ViewBuilder private var content: some View {
        if let signOut = authRepository.keysSignOut {
            // ADR-0065 section 3.5 / UX 15.2 — this device's keys are not the
            // account's keys any more. One full-screen state instead of the
            // app, whatever the auth state behind it: the wipe runs by itself
            // (or on the tap for the mismatch anomaly), then "Sign in".
            RemovedDeviceScreen(
                keysReset: signOut,
                username: authRepository.keysSignOutUsername,
                onSignOut: { authRepository.signOutAfterKeysMismatch() },
                onSignIn: { authRepository.dismissKeysSignOut() }
            )
            .onAppear {
                services.deviceGroupSync.clear()
                services.callHost.reset()
                services.groupCallHost.reset()
                services.linkCallHost.reset()
            }
        } else {
            authContent
        }
    }

    @ViewBuilder private var authContent: some View {
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
                // No account, no feed checks to report.
                services.feedRefresh.reset()
                // ADR-0048 — no account, no own profile to publish.
                ProfileDirectory.shared.deactivate()
                // ADR-0060 — no account, no reach keys to register.
                ReachCenter.shared.deactivate()
                // No session, no call: end anything live and forget the
                // session's call bookkeeping.
                services.callHost.reset()
                services.groupCallHost.reset()
                services.linkCallHost.reset()
                // …and with it anything the phone is still showing as a call.
                // The lock-screen name mirror (``CallPeerNameMirror``) and the
                // muted set (``ConversationMuteMirror``) are per account since
                // 2026-09-30, so they stay for the next sign-in of the same
                // account and can never reach another; forget-device removes
                // the account's rows (``AccountLocalDataWipe``).
                CallKitProvider.shared.endAll()
                CallKitCallRegistry.shared.reset()
            }

        case .session(let account) where pendingSignupPhrase != nil:
            // The sign-up phrase step: the session is live, but the user has
            // not yet confirmed the phrase. Nothing else of the session
            // mounts until they have.
            SignupPhraseScreen(
                phrase: pendingSignupPhrase ?? "",
                authRepository: authRepository,
                onDone: { pendingSignupPhrase = nil }
            )
            .id(account.accountIdB64)

        case .session where deviceRemoved:
            // docs/ux/second-device.md section 3.1 — another device removed
            // this one. The removed screen replaces the whole signed-in app:
            // no tabs, no banners, no call overlay behind it.
            RemovedDeviceScreen(onSignOut: { services.forgetThisDevice() })

        case .session where authRepository.newPasswordOffered:
            // A forgot-password route succeeded on a device that holds the
            // keys: "Choose a new password" comes once, before the app.
            // Saving or "Not now" clears the offer and the session mounts.
            NewPasswordPromptScreen(authRepository: authRepository)

        case .session(let account):
            VStack(spacing: 0) {
                // ADR-0038 S4 — the "update available" notice, mounted above
                // the session content. Shown only when the verified manifest
                // resolves to an actionable update.
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
            // Signed in, keys not here: the canonical identity cannot be
            // rebuilt on this device yet. The restore prompt replaces the
            // tab shell and offers another device, a passkey that carries
            // keys, the 12-word phrase, or starting over; restoring the
            // keys flips the state to `.session`.
            RestoreIdentityPromptScreen(
                authRepository: authRepository,
                viewModel: RestoreIdentityPromptViewModel(
                    repository: authRepository,
                    username: account.username,
                    accountIdB64: account.accountIdB64
                )
            )
            .id(account.accountIdB64)
            .onAppear {
                // Same defensive teardown as the anon branch — no live
                // session runtime should linger while the keys are missing.
                services.deviceGroupSync.clear()
            }
        case .saveFreshPhrase(let account):
            // A fresh phrase to confirm: after a guest signup, or after the
            // person started over with a new identity (already published).
            // The phrase comes before the session lands; while the new
            // phrase is not yet the account's recovery credential the
            // screen withholds the words and offers "Try again".
            SaveFreshPhraseScreen(authRepository: authRepository, username: account.username)
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
    /// ADR-0063 — the on-device feed, second so it is one tap from the home
    /// without displacing it (`docs/ux/feed.md` section 2).
    case feed
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
    /// Settings > Security > Password: choose a new password from a device
    /// that holds the account's keys.
    case password
    case antiCensorship
    /// ADR-0059 section 6 — Settings -> Security -> Channels I share.
    case channelsIShare
    /// Settings > Language (ADR-0061).
    case language
    /// `docs/ux/chat-folders.md` section 4 — Settings > Chat folders, opened
    /// in its reorder state from a folder tab's Reorder folders.
    case chatFolders(reorder: Bool)
    /// Settings > Privacy > Who can find me (ADR-0060).
    case whoCanFindMe
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
    /// ADR-0063 — the foreground channel-post sync and the Feed's model.
    let channelPostSync: ChannelPostSync
    let feedVM: FeedViewModel
    /// `docs/ux/feed.md` section 5 — this device's feed reading state: hidden
    /// conversations, last looks, channel read markers and counts.
    let feedReading: FeedReadingModel
    /// ADR-0059 — Discover's model (owned here so the Feed's model and the
    /// shared-channels wiring below read one instance).
    let discoverModel: DiscoverModel
    private var chatModels: [String: ChatViewModel] = [:]
    private var sharedChannelsWatch: AnyCancellable?

    init(services: AppServices, account: SessionAccount) {
        let accountIdB64 = account.accountIdB64
        contactsVM = ContactsViewModel(
            repository: services.contactsRepository,
            chatRepository: services.chatRepository
        )
        chatListVM = ChatListViewModel(repository: services.chatRepository)
        // Bound at construction, so the persisted channel list and publisher
        // keys are loaded before the first body evaluation.
        channelsState = ChannelsUiState(accountIdB64: accountIdB64)
        // ADR-0065 phase 2 (UX 16.6) — the session's key generation, so a
        // channel created under earlier keys reads as orphaned on the device
        // that reset them.
        channelsState.currentKeyGeneration = services.authRepository.identitySecrets?.session.keyGeneration
        channelPostSync = ChannelPostSync(accountIdB64: accountIdB64, channels: channelsState)
        // `docs/ux/feed.md` — every channel sync reports into the app-wide
        // refresh state; another account's results are forgotten here.
        let feedRefresh = services.feedRefresh
        feedRefresh.bind(accountIdB64: accountIdB64)
        channelPostSync.onSyncDone = { [weak feedRefresh] tally in
            feedRefresh?.record(.channels, tally)
        }
        let reading = FeedReadingModel(accountIdB64: accountIdB64)
        feedReading = reading
        // A channel synced for the first time gets its read marker at the
        // newest post it holds, so a fresh subscription shows no count.
        channelPostSync.onSlotSynced = { [weak reading] channelId, slot in
            reading?.update { $0.seedingChannelReads([channelId: slot]) }
        }
        reading.bind(postSync: channelPostSync, channels: channelsState)
        let chatRepository = services.chatRepository
        let channels = channelsState
        let chatList = chatListVM
        let contacts = services.contactsRepository
        let hub = chatRepository.sharedChannels
        // ADR-0059 section 6 — the listed channels this device follows, as the
        // share rule reads them: subscribed (a subscriber key is held), listed
        // by the channel's own metadata v2, under the handle it was found by.
        hub.shareable = {
            channels.channels.map { c in
                SharedChannels.Shareable(
                    channelIdHex: c.id,
                    subscribed: channels.subscriberKeysByChannel[c.id] != nil,
                    listed: c.sharedListed,
                    handle: String(c.handle.trimmingCharacters(in: .whitespaces).drop(while: { $0 == "@" })),
                    name: c.sharedMeta?.name ?? c.localName)
            }
        }
        discoverModel = DiscoverModel(
            accountIdB64: accountIdB64,
            client: ChannelsClient(),
            hub: hub,
            deps: DiscoverModel.Deps(
                liveGroups: {
                    Set(chatRepository.state.threads
                        .filter { !$0.membershipEnded && chatList.kind(of: $0.groupIdHex) == .group }
                        .map(\.groupIdHex))
                },
                groupTitle: { chatList.title(groupIdHex: $0) },
                contactName: { key in
                    guard contacts.state.accepted.contains(where: { base64Encode($0.request.fromPubkey) == key })
                    else { return nil }
                    return ProfileDirectory.shared.resolve(signatureKeyB64: key)?.name
                },
                ownKeyB64: { chatRepository.ownPersonKeyB64() },
                following: { Set(channels.channels.map(\.id)) },
                publishing: { Set(channels.keysByChannel.keys) },
                resolveHandle: { try await ChannelDiscovery.resolveChannelHandle($0) },
                hiddenFromFeeds: { [weak reading] in Set(reading?.reading.hidden ?? []) }))
        feedVM = FeedViewModel(
            account: account,
            chatRepository: services.chatRepository,
            chatList: chatListVM,
            channels: channelsState,
            postSync: channelPostSync,
            deviceGroupSync: services.deviceGroupSync,
            contacts: services.contactsRepository,
            discover: discoverModel,
            reading: reading,
            refreshCenter: feedRefresh)
        // A change of the followed listed set (a subscribe, an unsubscribe, a
        // channel becoming listed or unlisted): re-send to every group the
        // switch is on in whose last list differs. Debounced, so a burst
        // becomes one list (chosen, the web client's 3 s).
        sharedChannelsWatch = channels.objectWillChange
            .debounce(for: .seconds(3), scheduler: RunLoop.main)
            .sink { _ in Task { @MainActor in await hub.resendChanged() } }
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

    /// ADR-0061 — the tab titles follow the interface language.
    @ObservedObject private var l10n = Localization.shared

    @State private var selectedTab: SessionTab = .chats
    @State private var chatsPath: [TabRoute] = []
    @State private var feedPath: [TabRoute] = []
    @State private var contactsPath: [TabRoute] = []
    @State private var callsPath: [TabRoute] = []
    @State private var settingsPath: [TabRoute] = []

    init(account: SessionAccount, services: AppServices) {
        self.account = account
        self.services = services
        self._inviteIntake = ObservedObject(wrappedValue: services.inviteIntake)
        self._models = StateObject(
            wrappedValue: SessionModels(services: services, account: account))
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
                    feedReading: models.feedReading,
                    onOpen: { openConversation($0) },
                    onCreate: { create($0) },
                    onFoundChannel: { channelId, handle in
                        push(.channelFound(channelId: channelId, handle: handle), on: .chats)
                    },
                    onFoundPerson: { handOffToContacts(username: $0) },
                    onOpenFolderSettings: { reorder in
                        push(.chatFolders(reorder: reorder), on: .chats)
                    },
                    peopleSearch: PeopleSearchActions(
                        search: { [contactsRepository, chatRepository] query in
                            // Local first (web `localPeople`): contacts and
                            // the members of this device's groups and
                            // channels, with or without a grant.
                            let own = ReachCenter.shared.ownAccountKey
                            let contacts = contactsRepository.state.accepted.map {
                                (accountKey: $0.request.fromPubkey, accountId: $0.request.fromAccountId)
                            }
                            let contactKeys = Set(contacts.map { $0.accountKey.base64EncodedString() })
                            let local = PeopleSearch.local(
                                query: query,
                                candidates: PeopleSearchWiring.localCandidates(
                                    contacts: contacts,
                                    groupPeopleKeys: chatRepository.allMemberAccountKeys(),
                                    profiles: ProfileDirectory.shared.receivedProfiles()),
                                ownAccountKey: own)
                            let network = await PeopleSearch.network(
                                query: query,
                                records: IntroductionBook.shared.relayedRecords()
                                    + IntroductionBook.shared.offeredRecords(),
                                known: {
                                    PeopleSearchWiring.known(
                                        keyB64: $0.base64EncodedString(), contactKeys: contactKeys)
                                },
                                ownAccountKey: own,
                                ports: contactsRepository.peopleSearchPorts())
                            if case .found(let people) = network {
                                return .found(PeopleSearch.merge([local, people], ownAccountKey: own))
                            }
                            return local.isEmpty ? network : .found(local)
                        },
                        sendRequest: { [contactsRepository] person, intro in
                            _ = try await contactsRepository.sendContactRequest(
                                to: person, introMessage: intro)
                        })
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .chats) }
            }
            .tabItem { Label(l10n.t("nav.chats"), systemImage: "bubble.left") }
            .tag(SessionTab.chats)

            NavigationStack(path: $feedPath) {
                FeedScreen(
                    model: models.feedVM,
                    onOpenGroup: { groupIdHex, messageId in
                        chatViewModel(for: groupIdHex).requestJump(to: messageId)
                        push(.chat(groupIdHex: groupIdHex), on: .feed)
                    },
                    onReplyGroup: { groupIdHex, messageId in
                        let chat = chatViewModel(for: groupIdHex)
                        chat.requestJump(to: messageId)
                        chat.requestReply(to: messageId)
                        push(.chat(groupIdHex: groupIdHex), on: .feed)
                    },
                    onOpenConversation: { push(.chat(groupIdHex: $0), on: .feed) },
                    onOpenChannel: { push(.channel(channelId: $0), on: .feed) },
                    onGoToChats: { selectedTab = .chats },
                    onOpenFoundChannel: { channelId, handle in
                        push(.channelFound(channelId: channelId, handle: handle), on: .feed)
                    }
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .feed) }
            }
            .tabItem { Label(l10n.t("nav.feed"), systemImage: "newspaper") }
            .tag(SessionTab.feed)

            NavigationStack(path: $contactsPath) {
                ContactsScreen(viewModel: contactsVM)
                    .navigationDestination(for: TabRoute.self) { routeView($0, tab: .contacts) }
            }
            .tabItem { Label(l10n.t("nav.contacts"), systemImage: "person.crop.circle") }
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
            .tabItem { Label(l10n.t("nav.calls"), systemImage: "phone") }
            .tag(SessionTab.calls)

            NavigationStack(path: $settingsPath) {
                SettingsScreen(
                    account: account,
                    authRepository: authRepository,
                    onOpen: { settingsPath.append($0) },
                    onLogout: authRepository.logout,
                    onForgetDevice: forgetDevice,
                    onAccountDeleted: { services.wipeAccountLocalData(accountIdB64: $0) }
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .settings) }
            }
            .tabItem { Label(l10n.t("nav.settings"), systemImage: "gearshape") }
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
        // ADR-0063 — a shared feed link opens the import on the Feed tab.
        .onChange(of: inviteIntake.pendingFeedImport) { _, _ in openPendingFeedImport() }
        // ADR-0063 section 4 — channel posts join the foreground poll for the
        // whole session, so the feed has them without opening each channel.
        .task(id: account.accountIdB64) {
            models.channelPostSync.start()
            openPendingFeedImport()
        }
        // ADR-0059 section 4 — let new subscribers into every channel this
        // device posts to while the app is in front, whatever screen is
        // showing, not only while that channel's Manage screen is open. The
        // task ends with the session root (sign-out, forget, account switch),
        // and the shared state is dropped with it.
        .task(id: account.accountIdB64) {
            let admission = SubscriberAdmission.shared
            admission.reset()
            defer { admission.reset() }
            await admission.runLoop(
                channels: models.channelsState,
                accountIdentitySeed: { authRepository.identitySecrets?.accountIdentitySeed })
        }
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
                snapshot: { chatRepository.currentStorageSnapshot() },
                // ADR-0058 section 5.2 — the folded synced state (folders,
                // pins, mutes, archive, read cursors, drafts).
                syncedState: {
                    authRepository.identitySecrets.flatMap {
                        SyncedStateStore.bytes(accountIdB64: base64Encode($0.accountId))
                    }
                }
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
                // ADR-0060 — reach keys and the "who can find me" setting,
                // bound before the chat bootstrap for the same reason: a join
                // publishes the own profile, which carries this account's
                // grants.
                if let accountIdentity = try? Identity.fromSeed(seed: secrets.accountIdentitySeed) {
                    ReachCenter.shared.activate(
                        accountIdB64: base64Encode(secrets.accountId),
                        accountIdentity: accountIdentity)
                }
                ReachCenter.shared.sessionTokenProvider = { [authRepository] in
                    authRepository.identitySecrets?.sessionToken
                }
                ReachCenter.shared.ownNameProvider = {
                    let profiles = ProfileDirectory.shared
                    let accountId = profiles.accountIdB64.flatMap { Data(base64Encoded: $0) }
                    return (profiles.ownProfile?.displayName, profiles.ownUsername, accountId)
                }
                ReachCenter.shared.channelEpochsProvider = { [channelsState] in
                    channelsState.channels.compactMap { channel -> (id: Data, epoch: UInt32)? in
                        guard let id = CoPublisher.hexToData(channel.id),
                              let epoch = channelsState.subscribedSeeds(channelId: channel.id).keys.max()
                        else { return nil }
                        return (id, epoch)
                    }
                }
                ReachCenter.shared.allScopesProvider = { [chatRepository, channelsState] in
                    let groups = chatRepository.state.threads
                        .filter { chatRepository.conversationKind(groupIdHex: $0.groupIdHex) == .group }
                        .compactMap { CoPublisher.hexToData($0.groupIdHex) }
                        .map { FfiScopeId(kind: .group, id: $0) }
                    let channels = channelsState.channels
                        .compactMap { CoPublisher.hexToData($0.id) }
                        .map { FfiScopeId(kind: .channel, id: $0) }
                    return groups + channels
                }
                // ADR-0060 section 5 — the username certificate every profile
                // v3 carries beside the username. Fetched once per device (the
                // server signs it on request); until it lands the profile
                // carries the name alone.
                if !account.username.isEmpty,
                   ReachCenter.shared.ownUsernameCertificate == nil {
                    let token = secrets.sessionToken
                    Task { @MainActor in
                        do {
                            let cert = try await DiscoveryClient(sessionToken: token)
                                .fetchUsernameCertificate()
                            ReachCenter.shared.setOwnUsernameCertificate(cert)
                            if cert != nil { await chatRepository.publishOwnProfileToAllGroups() }
                        } catch {
                            NSLog("ADR-0060: username certificate not fetched: \(error)")
                        }
                    }
                }
                // `docs/ux/navigation.md` section 6.2 — the flat Calls index.
                // Bound here for the same reason the profile directory is: the
                // writer is the call hosts' record port, built once at the
                // composition root, and it has to know whose calls it is
                // filing before the first one ends.
                CallHistoryStore.shared.bind(accountIdB64: account.accountIdB64)
                chatRepository.bootstrap(secrets: secrets)
                // ADR-0053 point 2 — claim the account and device mailboxes on
                // every session start (signup, login, restore). Fire and forget.
                chatRepository.claimOwnMailboxes()
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
    private func startNewCall() async -> UiText? {
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
            Text(l10n.t("channels.error.sign_in"))
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
                onOpenContact: { messageId in push(.contactDetail(messageId: messageId), on: tab) },
                // ADR-0052 decision 2 — after a forward, the conversation it
                // went to; on a tapped attribution, the origin, told which
                // message to show before it is on screen. Pop back to it when
                // it is already on this stack, else push it.
                onOpenConversation: { targetGroupIdHex, jumpTo in
                    if let jumpTo {
                        chatViewModel(for: targetGroupIdHex).requestJump(to: jumpTo)
                    }
                    returnToOrPush(.chat(groupIdHex: targetGroupIdHex), on: tab)
                }
            )
            // UX spec section 5.2 — a pushed chat fills the screen; the tab
            // bar returns with the list.
            .toolbar(.hidden, for: .tabBar)
        case .newChat:
            ContactPickerScreen(
                viewModel: contactsVM,
                title: l10n.t("chats.create.chat"),
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
                    accountIdentitySeed: seed,
                    onPostRead: { [feedReading = models.feedReading] in
                        feedReading.advanceChannelRead($0, cursor: $1)
                    })
            } else {
                channelSeedMissing(l10n.t("channels.fallback_name"))
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
                    foundHandle: handle,
                    onPostRead: { [feedReading = models.feedReading] in
                        feedReading.advanceChannelRead($0, cursor: $1)
                    })
            } else {
                channelSeedMissing(l10n.t("channels.fallback_name"))
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
                channelSeedMissing(l10n.t("channels.new.title"))
            }
        case .joinCoPublisher:
            if let seed = accountIdentitySeed {
                JoinCoPublisherScreen(state: channelsState, accountIdentitySeed: seed)
            } else {
                channelSeedMissing(l10n.t("channels.join.title"))
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
                    publishToAllGroups: {
                        await chatRepository.publishOwnProfileToAllGroups()
                        // ADR-0060 — a new name changes what Everyone publishes
                        // and what the introduction record is findable by.
                        ReachCenter.shared.sync()
                        await chatRepository.sendIntroductions()
                    }
                )
            )
        case .passkeys:
            PasskeysView(
                viewModel: PasskeysViewModel(
                    authRepository: authRepository,
                    wipeAfterDeletion: { services.wipeAccountLocalData(accountIdB64: $0) })
            )
        case .devices:
            DevicesScreen(
                authRepository: authRepository,
                onAddDevice: { push(.addDevice, on: tab) },
                onDeviceRemoved: { deviceKey in
                    // The removal ran on a DeviceGroup handle of its own; the
                    // sync runtime reloads from the snapshot it persisted.
                    if let secrets = authRepository.identitySecrets {
                        services.deviceGroupSync.reloadAfterLocalMembershipChange(secrets: secrets)
                    }
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
                    // ADR-0058 section 5.1 — the add ran on a DeviceGroup
                    // handle of its own; the sync runtime reloads, sees the
                    // group grew, and re-sends its state to the new device
                    // when this is the lowest prior leaf.
                    if let secrets = authRepository.identitySecrets {
                        services.deviceGroupSync.reloadAfterLocalMembershipChange(secrets: secrets)
                    }
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
            RecoverySettingsScreen(
                authRepository: authRepository,
                onOpen: { push($0, on: tab) }
            )
        case .password:
            PasswordSettingsScreen(authRepository: authRepository)
        case .antiCensorship:
            AntiCensorshipSettingsView(manager: AntiCensorshipManager.shared)
        case .channelsIShare:
            ChannelsIShareScreen(
                hub: chatRepository.sharedChannels,
                title: { groupIdHex in
                    chatRepository.state.threads.contains { $0.groupIdHex == groupIdHex }
                        ? chatListVM.title(groupIdHex: groupIdHex) : nil
                })
        case .language:
            LanguageScreen()
        case .chatFolders(let reorder):
            ChatFoldersSettingsScreen(
                startInReorder: reorder, viewModel: chatListVM, channels: channelsState,
                feedReading: models.feedReading)
        case .whoCanFindMe:
            WhoCanFindMeScreen(
                reach: ReachCenter.shared,
                groups: chatRepository.state.threads
                    .filter { chatRepository.conversationKind(groupIdHex: $0.groupIdHex) == .group }
                    .compactMap { thread -> VisibilityScopeRow? in
                        guard let id = CoPublisher.hexToData(thread.groupIdHex) else { return nil }
                        return VisibilityScopeRow(
                            id: thread.groupIdHex,
                            scope: FfiScopeId(kind: .group, id: id),
                            title: chatRepository.groupName(groupIdHex: thread.groupIdHex) ?? Localization.text("chat.group.fallback_name"))
                    },
                channels: channelsState.channels.compactMap { channel -> VisibilityScopeRow? in
                    guard let id = CoPublisher.hexToData(channel.id) else { return nil }
                    let name = channel.sharedName.isEmpty ? channel.localName : channel.sharedName
                    return VisibilityScopeRow(
                        id: channel.id,
                        scope: FfiScopeId(kind: .channel, id: id),
                        title: name.isEmpty ? "@" + channel.handle : name)
                })
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
                    linkKeyFragment: inviteIntake.pendingGroupJoinFragment,
                    repository: chatRepository,
                    intake: inviteIntake
                ),
                onDone: { pop(route, from: tab) },
                onOpenChat: { hex in
                    replaceTop(route, with: .chat(groupIdHex: hex), in: tab)
                }
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

    /// Settings' "Forget this device" (``AppServices/forgetThisDevice()``).
    private func forgetDevice() {
        services.forgetThisDevice()
    }

    // MARK: - Path plumbing

    private var allPaths: [[TabRoute]] {
        [chatsPath, feedPath, contactsPath, callsPath, settingsPath]
    }

    /// Hand a parked `/feed/import#...` link to the Feed tab's import sheet.
    private func openPendingFeedImport() {
        guard let request = inviteIntake.takeFeedImport() else { return }
        selectedTab = .feed
        models.feedVM.importing = request
    }

    private func path(for tab: SessionTab) -> Binding<[TabRoute]> {
        switch tab {
        case .chats: return $chatsPath
        case .feed: return $feedPath
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
            // ADR-0061 — the language button in the top corner. Welcome hides
            // its navigation bar, so it rides an overlay (trailing = the
            // corner that mirrors in right-to-left languages).
            .overlay(alignment: .topTrailing) {
                LanguageButton().padding(.horizontal, 16)
            }
            .navigationDestination(for: AnonRoute.self) { route in
                switch route {
                case .signup:
                    SignupScreen(
                        viewModel: signupVM,
                        onNavigateToLogin: { path = [.login] }
                    )
                    .toolbar { ToolbarItem(placement: .topBarTrailing) { LanguageButton() } }
                case .login:
                    LoginScreen(
                        viewModel: loginVM,
                        onNavigateToSignup: { path = [.signup] },
                        onNavigateToRecover: { path.append(.recover) }
                    )
                    .toolbar { ToolbarItem(placement: .topBarTrailing) { LanguageButton() } }
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
