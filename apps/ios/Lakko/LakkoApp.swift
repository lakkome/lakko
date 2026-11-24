// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import Combine
import LakkoCore
import SwiftUI

/// The iOS / iPadOS app (started by ``LakkoMain``).
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
///
/// The process entry is ``LakkoMain``, which runs this app everywhere except
/// as the host of the unit tests.
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
    /// UX 17.4 — the proof block of the "Is this you?" modal.
    @StateObject private var deviceLinkProof: FreshAuthProofModel

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
        NewDeviceAttentionTabBadge.installAppearance()
        // The back chevron and the search magnifier UIKit draws, from the
        // Lakko icon set (design system section 8).
        SystemGlyphAppearance.install()
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
                // The row behind a ringing placeholder that rang nothing here
                // ends the placeholder now, not at its timeout.
                VoipPushRegistrar.shared.wakeFetched(
                    groupIdHex: target.groupIdHex,
                    appliedThrough: chat?.journalCursor(groupIdHex: target.groupIdHex))
            }
        }
        // A wake for a conversation this device is already in a 1x1 call in
        // (its own echo, a renegotiation, the end of the call on screen)
        // rings nothing.
        // A group call never rings a closed device, so a call wake for a
        // GROUP conversation, or for one whose group call this device is in or
        // just left, rings nothing either (older senders woke for group ends).
        // ADR-0099 section 6 — no notification of a blocked person's over a
        // running app.
        PushAppDelegate.conversationSuppressed = { [weak chat] groupIdHex in
            guard let peer = chat?.oneToOnePeerSignatureKey(groupIdHex: groupIdHex) else { return false }
            return BlockList.shared.verdict(.notification, accountKey: peer) == .drop
        }
        VoipPushRegistrar.shared.callWakeSuppressed = {
            [weak host, weak groupHost, weak chat] groupIdHex in
            // ADR-0099 section 6 — a call from a person this account blocked
            // is declined without a ring (`blockVerdict(.incomingCall)`); the
            // row itself is dropped by the receive filter, so no missed-call
            // row is written either.
            if let peer = chat?.oneToOnePeerSignatureKey(groupIdHex: groupIdHex),
               BlockList.shared.verdict(.incomingCall, accountKey: peer) == .declineSilently {
                return true
            }
            // ADR-0099 step 3 — a suspended account takes no calls.
            if SessionTokenProvider.shared.isSuspended { return true }
            return CallWakeSuppression.anySource(
                oneToOne: host?.suppressesCallWake(groupIdHex: groupIdHex) ?? false,
                groupCall: groupHost?.suppressesCallWake(groupIdHex: groupIdHex) ?? false,
                kind: chat?.groupKind(groupIdHex: groupIdHex))
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
        GroupLinkRegister.sync = sync
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
        // forward Welcomes + an invite-link accept can drive startGroupWith. Both
        // repositories live for the process, so this is wired once, here,
        // rather than on every evaluation of a view init.
        let contacts = ContactsRepository(authRepository: auth)
        self._contactsRepository = StateObject(wrappedValue: contacts)
        contacts.chatRepository = chat
        // ADR-0079 section 4 — the link-record register the inbox asks about
        // a request's link answer.
        contacts.deviceGroupSync = sync
        // ADR-0073 R.2 — the one seam between the identity gate, which accepts
        // a contact's attested reset wherever a bundle is fetched, and the
        // layers that follow it up. Nothing that listens shows anything.
        ResetEvents.subscribe { [weak contacts] event in
            Task { @MainActor in
                contacts?.applyAcceptedReset(event)
            }
        }
        // `docs/ux/call-links.md` section 3 — `Invite contacts` on a call
        // sends the call's link to a contact the person picks: into the
        // one-to-one with them, founded first when there is none.
        linkHost.wireInvitations(chat: chat, contacts: contacts)
        // ADR-0050 section 2.4 — the accounts the re-admission check can look
        // up: every accepted contact whose account id is known.
        chat.readmissionContactsProvider = { [weak contacts] in
            let proven = ProvenPredecessorStore().load()
            return (contacts?.state.accepted ?? []).map { contact in
                let key = base64Encode(contact.request.fromPubkey)
                return DeviceReadmission.LookupContact(
                    signaturePubkeyB64: key,
                    previousKeysB64: DeviceReadmission.keysARowAnswersTo(
                        contactKeyB64: key, previousKeysB64: contact.previousKeysB64 ?? [], proven: proven),
                    accountId: contact.request.fromAccountId)
            }
        }
        // UX 17.4 — the proof block of "Is this you?": one model shared by
        // the flow (which asks it before anything is sent) and the modal
        // (which draws it).
        let deviceLinkProof = FreshAuthProofModel(surface: .addDeviceRequest, repository: auth)
        self._deviceLinkProof = StateObject(wrappedValue: deviceLinkProof)
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
                    // The grant the result was just posted with.
                    let grant = auth.liveFreshGrant()
                    // The hand-over carries the entropy of the recovery
                    // phrase this device holds (recovery payload v2); with
                    // no phrase stored here no keys are sent.
                    let recovery = PairingOrchestrator.ScanRecovery(
                        accountIdB64: session.accountIdB64,
                        deviceAuthorizeStart: {
                            guard let grant else { throw FreshAuthRequired() }
                            return try await AuthClient().deviceAuthorizeStart(
                                sessionToken: token, freshGrant: grant)
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
                    // UX 17.5 — this device let it in: no notice about it here.
                    NewDeviceNoticeCenter.shared.letIn(
                        accountIdB64: session.accountIdB64, signaturePublicKey: scanned.newDeviceKey)
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
                },
                proof: .init(
                    ensure: { await deviceLinkProof.ensure() },
                    grant: { [weak auth] in auth?.liveFreshGrant() },
                    expired: { [weak auth] in
                        auth?.dropFreshAuth()
                        deviceLinkProof.proofExpired()
                    },
                    closed: { deviceLinkProof.closed() }
                )
            )
        ))
        let deviceRemoval = DeviceRemovalRunner(ports: .live(auth: auth, chat: chat, sync: sync))
        let services = AppServices(
            authRepository: auth,
            contactsRepository: contacts,
            chatRepository: chat,
            callHost: host,
            groupCallHost: groupHost,
            linkCallHost: linkHost,
            deviceGroupSync: sync,
            inviteIntake: intake,
            feedRefresh: feedRefresh,
            deviceRemoval: deviceRemoval,
            keyRotation: KeyRotationRunner(
                ports: .live(
                    auth: auth, chat: chat, contacts: contacts, sync: sync, removal: deviceRemoval)),
            accountProtection: AccountProtectionCenter(ports: .live(auth, sync: sync))
        )
        self.services = services
        // ADR-0065 — the blob routes' delete keys come from the ACCOUNT seed;
        // the delete list and the keep tick are per account; saved messages
        // live in the synced state.
        BlobAccess.deleteKeySeed = { [weak auth] in
            guard let secrets = auth?.identitySecrets else { return nil }
            return BlobDeleteKeys.seed(
                accountIdB64: base64Encode(secrets.accountId), currentSeed: secrets.accountIdentitySeed)
        }
        BlobLifecycle.shared.accountIdB64 = { [weak auth] in
            auth?.identitySecrets.map { base64Encode($0.accountId) }
        }
        BlobLifecycle.shared.chat = chat
        BlobLifecycle.shared.sync = sync
        SavedMessagesCenter.shared.bind(sync: sync, chat: chat)
        // ADR-0092 section 3.6 — the own poll choices and the ballot queue,
        // which resumes every queued ballot (posted once the session landed).
        PollChoiceCenter.shared.bind(sync: sync, chat: chat)
        if let secrets = auth.identitySecrets {
            BallotQueue.shared.bind(chat: chat, accountIdB64: base64Encode(secrets.accountId))
        }
        // ADR-0075 — at every landing of a session with keys: what protects
        // the account (the recovery email, a saved phrase) and what its
        // devices are to be told.
        let accountProtection = services.accountProtection
        auth.onProtectedSessionLanded = { accountIdB64 in
            accountProtection.landed(accountIdB64: accountIdB64)
        }
        auth.onProtectionChanged = {
            Task { await accountProtection.refresh() }
        }
        // G-56 — the chat layer refused to bind a device whose account's
        // earlier keys have not reached it: the session parks.
        chat.onEarlierKeysMissing = { [weak auth] accountIdB64 in
            auth?.parkForEarlierKeys(accountIdB64: accountIdB64)
        }
        // A guest chose a username: the profile directory learns it, the
        // username certificate is fetched, and the profile (username +
        // certificate) goes out through the existing publishers.
        auth.onUsernameClaimed = { [weak auth, weak chat] account in
            ProfileDirectory.shared.activate(accountIdB64: account.accountIdB64, username: account.username)
            guard let token = auth?.identitySecrets?.sessionToken else { return }
            Task { @MainActor in
                do {
                    let cert = try await DiscoveryClient(sessionToken: token).fetchUsernameCertificate()
                    ReachCenter.shared.setOwnUsernameCertificate(cert)
                } catch {
                    NSLog("ADR-0060: username certificate not fetched after the claim")
                }
                ReachCenter.shared.sync()
                await chat?.publishOwnProfileToAllGroups()
                await chat?.sendIntroductions()
                await accountProtection.refresh()
            }
        }
        // ADR-0075 (amended 2026-10-13) — the account's devices keep the
        // recovery address in the synced state: a write that waited for the
        // state runs once it is loaded, and every change of the register
        // (this device, a sibling, a restored backup) re-evaluates the
        // automatic re-seal.
        sync.onSyncedStateReady = { [weak auth, weak sync, weak chat] in
            accountProtection.syncedStateReady()
            // ADR-0087 section 9 — step 2 of an owed end waits for this.
            chat?.applyHeardEnds()
            chat?.resumeOwedEnds()
            // ADR-0087 section 18 — the group photos drawn by the avatar
            // component, and any photo set here but not yet sent.
            chat?.refreshGroupPhotos()
            Task { await chat?.sendOwedGroupPhotos() }
            // ADR-0087 section 4.5.4 — a public group's record republish (a
            // name or photo change, a knock bump) owed from an earlier session.
            Task { await chat?.flushOwedHandleWrites() }
            // ADR-0087 section 3 — renew this account's names before their
            // lease lapses, quietly, once per session.
            Task { @MainActor in
                await HandleLeaseRenewal.shared.runIfDue(
                    accountIdB64: chat?.currentAccountIdB64,
                    accountIdentity: { chat?.accountIdentity },
                    groupFacts: { chat?.publicGroupFacts(journalIdHex: $0) })
            }
            // ADR-0076 — the own-username register: seeded once from the
            // local name, or a name written elsewhere adopted.
            Task { await auth?.ownUsernameRegisterChanged(sync?.ownUsername) }
        }
        auth.storeOwnUsername = { [weak sync] name in await sync?.setOwnUsername(name) ?? false }
        // A session that landed with no username learned it (the backup, or
        // the username prompt): a rejoin waiting for a login handle knocks now.
        auth.onUsernameLearned = { [weak sync] _ in sync?.loginHandleArrived() }
        // Reactions-and-emoji spec section 6 (D3) — the quick reaction is the
        // account's, written through its synced state.
        QuickReactionSetting.shared.onChoose = { [weak sync] emoji in
            Task { _ = await sync?.setQuickReaction(emoji) }
        }
        // ADR-0099 section 6 — a block is the account's, written through its
        // synced state.
        BlockList.shared.setter = { [weak sync] key, blocked in
            sync?.setBlocked(accountKey: key, blocked: blocked) ?? false
        }
        BlockList.shared.blockAction = { [weak contacts] key in
            contacts?.block(accountKey: key) ?? BlockList.shared.set(key, blocked: true)
        }
        auth.heldRecoveryAddress = { [weak sync] in sync?.recoveryEmail }
        accountProtection.usernameWatch = sync.$ownUsername
            .removeDuplicates()
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { [weak auth] name in
                Task { await auth?.ownUsernameRegisterChanged(name) }
            }
        // ADR-0076 — the username changed (here, or on another device):
        // the profile directory takes the name, the cached certificate is
        // replaced (the one the rename answered, else a fetch of the
        // current name's), and what carries the name goes out again: the
        // profile, the Everyone record and terms, the introduction offer,
        // the member cards (the reach sync's inputs changed).
        auth.onUsernameRenamed = { [weak auth, weak chat] account, certificate in
            ProfileDirectory.shared.activate(accountIdB64: account.accountIdB64, username: account.username)
            ProfileDirectory.shared.restampOwnProfile()
            ReachCenter.shared.setOwnUsernameCertificate(certificate)
            let token = auth?.identitySecrets?.sessionToken
            Task { @MainActor in
                if certificate == nil, let token {
                    do {
                        let cert = try await DiscoveryClient(sessionToken: token).fetchUsernameCertificate()
                        ReachCenter.shared.setOwnUsernameCertificate(cert)
                    } catch {
                        NSLog("ADR-0076: the new username's certificate was not fetched")
                    }
                }
                ReachCenter.shared.sync()
                await chat?.publishOwnProfileToAllGroups()
                await chat?.sendIntroductions()
            }
        }
        accountProtection.addressWatch = sync.$recoveryEmail
            .removeDuplicates()
            .dropFirst()
            .receive(on: RunLoop.main)
            .sink { _ in accountProtection.addressChanged() }
        // ADR-0074 section 3.3 — a kept device: the account's own
        // `key-rotation` envelope arrives in the DeviceGroup and nowhere
        // else, and is kept as pending until the server side shows its key
        // as the account's.
        sync.onKeyRotationEnvelope = { [weak auth] envelope in
            auth?.receiveKeyRotationEnvelope(envelope, arrival: .ownDeviceGroup)
        }
        // ADR-0091 phase 2 route 1 — a re-entry request between this
        // account's own devices rides the DeviceGroup both ways.
        sync.onConversationReentry = { [weak chat] envelope in
            Task { @MainActor in await chat?.receiveConversationReentry(envelope: envelope, route: .ownDevice) }
        }
        chat.reentryOwnDeviceSender = { [weak sync] envelope in
            await sync?.sendServiceEnvelope(envelope)
        }
        // ADR-0081 decision 4 — a device that lost or fell behind its
        // DeviceGroup knocks under this account's login handle, after its own
        // directory KeyPackage (the signed device binding a sibling checks)
        // is published; a sibling admits only knocks naming the handle.
        sync.loginHandleProvider = { [weak auth] in
            guard let auth else { return nil }
            return DeviceSession.current(auth)?.loginHandle
        }
        sync.republishDirectoryKeyPackage = { [weak chat] in
            await chat?.republishKeyPackage().value
        }
        // A superseded device with no pending envelope reads its DeviceGroup
        // journal to the head once before it treats the change as a reset.
        // When the sync runtime is not running (a launch whose refresh was
        // refused before any session landed), or did not reach the head, the
        // journal is read once on a handle of its own and every service
        // envelope goes to the key-rotation intake (ADR-0074 section 3.3).
        auth.readDeviceGroupJournalToHead = { [weak sync, weak auth] accountIdB64 in
            if await sync?.waitForJournalHead() == true { return .reached }
            guard let accountIdBytes = base64Decode(accountIdB64) else { return .noState }
            let reader = await DeviceGroupJournalOneShot.live(
                accountIdBytes: accountIdBytes, accountIdB64: accountIdB64)
            return await DeviceGroupJournalOneShot.drain(reader: reader) { envelope in
                guard DeviceGroupSyncRepository.isKeyRotation(envelope) else { return }
                auth?.receiveKeyRotationEnvelope(envelope, arrival: .ownDeviceGroup)
            }
        }
        // ADR-0074 sections 3.2 step 6, 3.3 — the account's keys were
        // rotated and this device holds the new ones: what derives from the
        // account seed outside MLS moves (the reach keys and the mailbox, the
        // journal roots the chat repository derives); every conversation, the
        // device identity and its binding stay.
        let keyRotation = services.keyRotation
        auth.onAccountKeysRotated = { [weak auth, weak chat, weak contacts] secrets, predecessorSeed in
            let accountIdB64 = base64Encode(secrets.accountId)
            if let rotated = try? Identity.fromSeed(seed: secrets.accountIdentitySeed) {
                ReachCenter.shared.rekey(accountIdB64: accountIdB64, accountIdentity: rotated)
            }
            chat?.adoptRotatedAccount(secrets: secrets, predecessorSeed: predecessorSeed)
            // ADR-0075 — a rotation deletes the email vault of the old phrase
            // (and "Save a recovery phrase" marks the account
            // phrase-protected): read the protection again, on the rotating
            // and a kept device.
            Task { await accountProtection.refresh() }
            // Rule 5 — the links this session generated name the old mailbox:
            // they leave the list at once, on the rotating and a kept device.
            contacts?.dropSpentLinksFromList()
            ReachCenter.shared.sync()
            // The rotating device publishes in its own order (the directory
            // upload deletes every KeyPackage row first). A KEPT device has
            // nothing else to wait for: its row goes back under the same
            // device id, and the channels it created follow the new key.
            guard keyRotation.stage == nil else { return }
            Task { @MainActor in
                do {
                    try await chat?.republishKeyPackageAfterKeyRotation()
                } catch {
                    NSLog("ADR-0074: the KeyPackage re-publish after adopting the new keys is retried at the next session start")
                }
                if let auth {
                    try? await KeyRotationChannels.handOverAll(
                        auth: auth, keyGeneration: secrets.session.keyGeneration)
                }
                // ADR-0074 section 3.5a rule 3 — once the reach keys are on
                // the new mailbox, the profile (with grants for it) goes out
                // again through the throttled publisher.
                // Durable: what does not land now is retried at each
                // session start until every conversation took it.
                await chat?.resendProfileAfterAdoptingKeyRotation()
                // Rule 5 — what this device granted before the rotation is
                // spent too: its own links and unanswered requests.
                await contacts?.spendGrantsAfterAdoptingKeyRotation()
                NotificationCenter.default.post(name: .lakkoAccountKeysRotated, object: nil)
            }
        }
        // ADR-0073 section 3.5 — the signed-out screen's automatic wipe is the
        // full forget-device wipe: the runtimes let go first, then every
        // store, then the session ends (``AppServices/wipeAccountLocalData``).
        auth.wipeAfterKeyReset = { [weak services] accountIdB64 in
            services?.wipeAfterKeyReset(accountIdB64: accountIdB64)
        }
        // ADR-0074 section 1.1 — the staged wipe of a device whose account
        // keys were reset elsewhere: the same, except the recall record.
        auth.stagedWipeAfterKeyReset = { [weak services] accountIdB64 in
            services?.stagedWipeAfterKeyReset(accountIdB64: accountIdB64)
        }
        // ADR-0073 section 2.3 step 5 — after a reset this device's KeyPackage
        // is re-published from the chat repository's long-lived identity,
        // bound by the NEW account key.
        auth.keyResetHooks.republishOwnKeyPackage = { [weak chat] secrets in
            guard let chat else { throw SessionNotReady() }
            try await chat.republishKeyPackageAfterKeyReset(secrets: secrets)
        }
        // ADR-0074 section 2.2 — a KeyPackage publish needs a session that
        // names this device; once the bind landed, publish again.
        auth.onSessionDeviceBound = { [weak chat] in
            chat?.republishKeyPackage()
        }
        // ADR-0074 section 2.3 — the account's own DeviceGroup is the record
        // the conversations' own-device admission is filtered by: the live
        // handle's roster, else the snapshot it persists.
        // ADR-0073 R.5 — the conversation lineage is a register of the
        // DeviceGroup synced state: the chat repository reads it to fold the
        // groups it holds into conversations, and writes it through the sync
        // repository when a conversation continues in a successor group.
        chat.lineageWatch = sync.$lineage
            .receive(on: RunLoop.main)
            .sink { [weak chat] entries in chat?.applyLineage(entries) }
        chat.applyLineage(sync.lineage)
        // ADR-0087 section 9 — the end of a membership travels between the
        // account's devices as a register of the same state. Read after every
        // change of the fold; delivered on the main run loop, so the property
        // holds the new value by then.
        chat.conversationEndSync = sync
        chat.conversationEndsWatch = sync.$endedConversations
            .receive(on: RunLoop.main)
            .sink { [weak chat] _ in
                chat?.applyHeardEnds()
                chat?.resumeOwedEnds()
            }
        chat.lineageWriter = { [weak sync] successor, root in
            await sync?.setLineage(successorGroupId: successor, rootConversationId: root) ?? false
        }
        // One person, one one-to-one row (``DuplicateOneToOne``): folded once
        // the synced state is loaded, and again on every lineage change.
        chat.lineageReady = { [weak sync] in sync?.ready ?? false }
        chat.lineageReadyWatch = sync.$ready
            .receive(on: RunLoop.main)
            .sink { [weak chat] ready in if ready { chat?.foldDuplicateOneToOnes() } }
        // ADR-0090 — a channel's PublisherGroup leaf is this device: the
        // channel paths open, found and join it in the chat provider (the one
        // holding the KeyPackage a sibling admits) and persist through it.
        ChannelPublisherIdentity.shared.deviceIdentityProvider = { [weak chat] in chat?.canonicalIdentity }
        ChannelPublisherIdentity.shared.devicePersister = { [weak chat] in
            guard let chat else { throw ChannelPublisherIdentity.NotBound() }
            try chat.persistDeviceProviderOrThrow()
        }
        // ADR-0090 — the account's DeviceGroup roster decides which devices an
        // owner device admits to a channel's PublisherGroup; a device that
        // founded the DeviceGroup alone re-founds owned channels (decision A);
        // the owner account's admission seeds ride the synced state.
        ChannelDeviceAdmission.deviceGroupRoster = { [weak chat] in chat?.ownDeviceGroupRosterProvider() }
        ChannelDeviceAdmission.foundedDeviceGroupAlone = { [weak sync] in sync?.rejoinAnswer == .founded }
        ChannelDeviceAdmission.seedRegister = sync
        chat.ownDeviceGroupRosterProvider = { [weak sync, weak auth] in
            if let live = sync?.rosterDeviceKeys() { return live }
            guard let secrets = auth?.identitySecrets else { return nil }
            return DeviceRemoval.rosterKeys(
                accountIdBytes: secrets.accountId, accountIdB64: base64Encode(secrets.accountId))
        }
        // `docs/ux/call-links.md` section 9 — ending a guest account needs the
        // one local wipe Forget device and Delete account share. (Keeping a
        // guest account does NOT delete this device's directory KeyPackage:
        // a deleted row is tombstoned for 30 days, ADR-0074, and the kept
        // account publishes under the same device id once the shell mounts.)
        auth.guestEnvironment.wipeLocalData = { accountIdB64 in
            services.wipeAccountLocalData(accountIdB64: accountIdB64)
        }
    }

    var body: some Scene {
        WindowGroup {
            // ADR-0061 — the interface language: locale + layout direction
            // for everything below, switched live (no restart).
            LocalizedRoot {
                // `docs/ux/text-size.md` — the chat text size, live.
                windowContent.chatTextScaleRoot()
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
        // `docs/ux/channel-notifications.md` section 3 — channel posts read
        // while Lakko is in the background: one channel post sync within the
        // task's budget, then the next request. No session: nothing runs.
        .backgroundTask(.appRefresh(ChannelPostBackgroundRefresh.taskIdentifier)) {
            ChannelPostBackgroundRefresh.schedule()
            await ChannelPostBackgroundRefresh.runLive(
                authRepository: authRepository, deviceGroupSync: deviceGroupSync)
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
                    ChannelPostBackgroundRefresh.schedule()
                }
                // The foreground receive runs only while the app is active;
                // becoming active again sweeps at once, so an offer that
                // arrived while backgrounded rings if it is still fresh.
                foregroundReceive.scheduler.setForeground(phase == .active)
                // ADR-0081 third amendment — only a device in the foreground
                // listens for, and answers, a rejoining device's knock.
                services.deviceGroupSync.setForeground(phase == .active)
                if phase == .background {
                    // What the extension may need while the app is not
                    // running, written as it leaves.
                    services.chatRepository.mirrorCallPeerNames()
                }
                if phase == .active {
                    // ADR-0061 section 5 — the same throttled check on return
                    // to the foreground; a no-op until a day has passed.
                    Task { await TranslationUpdater.shared.check() }
                    // `docs/ux/calls.md` section 2.1 — refresh the names the
                    // notification extension may have to put on a ringing
                    // lock screen while this app is not running.
                    services.chatRepository.mirrorCallPeerNames()
                    // ADR-0080 — the person's touches refresh this device's
                    // card (the recogniser is put on any new window).
                    UserActionObserver.install()
                    // ADR-0080 — the first foreground counts as an action,
                    // and a growth re-send owed from the background goes.
                    DeviceCardCenter.shared.noteForeground()
                    // ADR-0087 section 9 — an end a wake only recorded.
                    services.chatRepository.resumeOwedEnds()
                    // ADR-0060 section 4 — a `fof` rotation that came due
                    // while the app was closed.
                    ReachCenter.shared.noteForeground()
                    // ADR-0099 step 3 — a suspended account asks auth again
                    // on every return to the foreground.
                    Task { await SuspensionCenter.shared.noteForeground() }
                }
            }
            // ADR-0061 — the mirrored names carry words of the interface
            // language (a nameless group's title, the placeholder), so a
            // language switch rewrites them rather than leaving the lock
            // screen in the old language until the next activation.
            .onReceive(Localization.shared.$language.removeDuplicates()) { _ in
                services.chatRepository.mirrorCallPeerNames()
            }
            // A member's profile that arrived while the app is open is a
            // name the extension may have to show (a reaction, a group
            // message): mirrored when it lands, not only at the next
            // activation. Debounced, since a group's profiles arrive in a
            // burst.
            .onReceive(
                ProfileDirectory.shared.$revision.removeDuplicates()
                    .debounce(for: .seconds(1), scheduler: RunLoop.main)
            ) { _ in
                services.chatRepository.mirrorCallPeerNames()
            }
            .task {
                foregroundReceive.scheduler.start()
                foregroundReceive.scheduler.setForeground(scenePhase == .active)
                services.deviceGroupSync.setForeground(scenePhase == .active)
                // ADR-0080 — `onChange(of: scenePhase)` does not run for the
                // initial value, so a cold launch installed no recogniser and
                // took no foreground. Both are idempotent.
                if scenePhase == .active {
                    UserActionObserver.install()
                    DeviceCardCenter.shared.noteForeground()
                }
            }
            .task(id: scenePhase) {
                // ADR-0049 section 2 — a sign-in request waiting for this
                // device's answer. Polled for as long as the app is active,
                // not only when it becomes active: a request usually arrives
                // while this device is open in front of the person.
                guard scenePhase == .active else { return }
                while !Task.isCancelled {
                    await deviceLinkResponder.check()
                    try? await Task.sleep(nanoseconds: deviceLinkResponder.pollNanos)
                }
            }
            .sheet(isPresented: Binding(
                get: { deviceLinkResponder.modal != nil },
                set: { if !$0 { deviceLinkResponder.dismiss() } }
            )) {
                DeviceLinkRequestModal(flow: deviceLinkResponder, proof: deviceLinkProof)
                    .presentationDetents([.medium, .large])
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
    /// ADR-0099 step 3 — suspended mode: the suspended screen in place of
    /// the app, or the read-only app. Publishes rarely.
    @ObservedObject private var suspension = SuspensionCenter.shared
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
    /// ADR-0075 D2 — a sign-up just landed: "Protect your account" comes
    /// before the app. The phrase is not shown at sign-up.
    @State private var pendingSignupProtect = false

    /// The choice made on the signed-out invite page (UX contact-invite-link
    /// section 3, "Signed out"): the sign-in or sign-up screen it opened.
    /// `nil` while that page is shown. The link stays parked either way, so
    /// the session that lands opens the invite page in its default state.
    /// A dead link is dropped instead (amended 2026-10-26).
    @State private var signedOutInviteChoice: AnonRoute?

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
                // ADR-0097 — the supporters list, at most once a day.
                await SupportersListCenter.shared.refreshIfDue()
            }
            // ADR-0099 step 3 — while suspended, ask auth again every 10
            // minutes; a normal pair ends suspended mode silently.
            .task(id: suspension.account) {
                await suspension.runChecks()
            }
            .onReceive(services.deviceGroupSync.$deviceRemoved.removeDuplicates()) { removed in
                if deviceRemoved != removed { deviceRemoved = removed }
                // ADR-0074 section 2.3 — this device's own DeviceGroup turned
                // inactive: proof of removal by a device of the account. The
                // removed screen comes up and the wipe runs by itself.
                if removed { authRepository.deviceGroupReportedRemoval() }
            }
            // ADR-0037 §6 (G-11) — a rolled-back / tampered backup was refused
            // at restore. User-visible alarm; the served (older / altered) state
            // was NOT loaded.
            .lakkoDialog(
                isPresented: Binding(
                    get: { authRepository.backupRollbackAlarm != nil },
                    set: { if !$0 { authRepository.dismissBackupRollbackAlarm() } }
                ),
                title: l10n.t("backup.rollback_refused.title"),
                message: l10n.t("backup.rollback_refused.body")
            ) {
                [.primary(l10n.t("common.dismiss")) { authRepository.dismissBackupRollbackAlarm() }]
            }
            // ADR-0037 §6 (G-11) — a stale but authentic backup needs an
            // explicit override. The restore flow suspends until the user
            // decides; Cancel (or dismiss) aborts the restore (fail-closed).
            .lakkoDialog(item: Binding(
                get: { authRepository.pendingStaleBackupOverride },
                set: { if $0 == nil { authRepository.resolveStaleBackupOverride(false) } }
            )) { request in
                LakkoDialogContent(
                    title: l10n.t("backup.stale_restore.title"),
                    message: l10n.t("backup.stale_restore.body", texts: [
                        "reason": request.reason,
                        "consequence": l10n.t("backup.stale_restore.consequence"),
                    ]),
                    actions: [
                        .danger(l10n.t("backup.stale_restore.confirm")) {
                            authRepository.resolveStaleBackupOverride(true)
                        },
                        .cancel { authRepository.resolveStaleBackupOverride(false) },
                    ])
            }
    }

    @ViewBuilder private var content: some View {
        if let signOut = authRepository.keysSignOut {
            // ADR-0073 section 3.5 / UX 15.2 — this device's keys are not the
            // account's keys any more. One full-screen state instead of the
            // app, whatever the auth state behind it: the wipe runs by itself
            // (or on the tap for the mismatch anomaly), then "Sign in".
            RemovedDeviceScreen(
                signOut,
                cause: authRepository.keysSignOutCause,
                username: authRepository.keysSignOutUsername,
                noticeKey: authRepository.keysSignOutNoticeKey,
                onSignOut: { authRepository.signOutAfterKeysMismatch() },
                onSignIn: { authRepository.dismissKeysSignOut() }
            )
            .onAppear {
                services.deviceGroupSync.clear()
                services.callHost.reset()
                services.groupCallHost.reset()
                services.linkCallHost.reset()
            }
        } else if let recall = authRepository.recall {
            // ADR-0074 section 1.1 / UX 18.4 — "Was it you?": the keys were
            // reset elsewhere, what the account kept here is deleted, and
            // this device can still take the account back. Instead of the
            // app, at launch too; driven by the recall record.
            RecallScreen(authRepository: authRepository, state: recall)
                .onAppear {
                    services.deviceGroupSync.clear()
                    services.callHost.reset()
                    services.groupCallHost.reset()
                    services.linkCallHost.reset()
                }
        } else if suspension.active, !(suspension.reading && sessionWithKeys) {
            // ADR-0099 step 3 / UX `reporting-and-moderation.md` section 4 —
            // the one screen that says why. A device without the account's
            // keys has no chats to read and shows it alone.
            SuspendedScreen(
                center: suspension,
                keysHere: sessionWithKeys,
                onSignOut: { authRepository.logout() },
                authRepository: authRepository,
                onAccountDeleted: { services.wipeAccountLocalData(accountIdB64: $0) }
            )
            .onAppear {
                services.callHost.reset()
                services.groupCallHost.reset()
                services.linkCallHost.reset()
            }
        } else {
            authContent
                .environment(\.accountSuspended, suspension.active)
        }
    }

    /// The account is signed in with its keys on this device.
    private var sessionWithKeys: Bool {
        if case .session = authRepository.state { return true }
        return false
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
                    // The guest session it lands is shown by ``GuestRoot``
                    // below, never by the tab shell.
                    JoinCallLandingScreen(
                        viewModel: JoinCallLandingViewModel(
                            slug: callSlug,
                            authRepository: authRepository,
                            intake: inviteIntake,
                            linkCallHost: services.linkCallHost
                        ),
                        onDone: {}
                    )
                    // A second link opened while this landing shows gets its
                    // own model: its slug, and a new guest name
                    // (`docs/ux/call-links.md` section 9.1).
                    .id(callSlug)
                } else if let inviteSlug = inviteIntake.pendingSlug, signedOutInviteChoice == nil {
                    // ADR-0079 amendment of 2026-10-26 — a tapped `/i/` link
                    // from a person who is not signed in: who invites, with
                    // Create an account / I already have an account. A live
                    // link stays parked; the session that lands opens the
                    // signed-in page. A dead link is dropped on either button.
                    SignedOutInviteScreen(
                        slug: inviteSlug,
                        preview: { [contacts = services.contactsRepository] in
                            await contacts.previewInviteLink(slug: $0)
                        },
                        dropLink: { inviteIntake.clear() },
                        onSignIn: { signedOutInviteChoice = .login },
                        onCreateAccount: { signedOutInviteChoice = .signup }
                    )
                    .id(inviteSlug)
                } else {
                    AnonRoot(
                        repository: authRepository,
                        cachedUsername: cachedUsername,
                        accountProtection: services.accountProtection,
                        onSignedUp: { pendingSignupProtect = true },
                        initialRoute: signedOutInviteChoice
                    )
                }
            }
            // A new link tapped while signed out shows its card again.
            .onChange(of: inviteIntake.pendingSlug) { _, slug in
                if slug != nil { signedOutInviteChoice = nil }
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
                // ADR-0075 — no account, no protection or notices to show.
                services.accountProtection.reset()
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

        case .session(let account) where pendingSignupProtect || authRepository.protectStepOwed:
            // The step after sign-up (UX 19.1a), and after a guest account was
            // kept (`docs/ux/call-links.md` section 9): the session is live,
            // and "Protect your account" comes before the app. Nothing else
            // of the session mounts until the person is through it. The
            // recovery phrase is not shown on either route (ADR-0075 D2).
            SignupProtectScreen(
                authRepository: authRepository,
                center: services.accountProtection,
                rotation: services.keyRotation,
                onDone: {
                    pendingSignupProtect = false
                    authRepository.dismissProtectStep()
                }
            )
            .id(account.accountIdB64)

        case .session where deviceRemoved:
            // UX 18.2 — another device removed this one. `content` shows the
            // removed screen the moment the repository took the signal; this
            // is the frame before it, with nothing of the app behind it.
            Color.lakkoBackground
                .ignoresSafeArea()

        case .session where authRepository.newPasswordOffered:
            // A forgot-password route succeeded on a device that holds the
            // keys: "Choose a new password" comes once, before the app.
            // Saving or "Not now" clears the offer and the session mounts.
            NewPasswordPromptScreen(authRepository: authRepository)

        case .session(let account) where authRepository.isGuest:
            // `docs/ux/call-links.md` section 9 — a guest account (made to
            // join a call from a link) never lands in the app: it is in its
            // call, or it is asked whether to keep the account.
            GuestRoot(account: account, services: services)

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
            // A sign-in that named no username (a passkey on a new device),
            // and the backup gave none of this account's: ask for it. A sheet
            // over the app, never a blocking step; closing it asks again at
            // the next app start.
            .sheet(isPresented: Binding(
                get: { authRepository.usernamePromptAccountIdB64 == account.accountIdB64 },
                set: { if !$0 { authRepository.dismissUsernamePrompt() } }
            )) {
                UsernamePromptSheet(authRepository: authRepository)
                    .presentationDetents([.medium, .large])
            }

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
                    accountIdB64: account.accountIdB64,
                    noteOwnAction: { [center = services.accountProtection] in center.noteOwnAction($0) },
                    keepAddress: { [center = services.accountProtection] in center.keepAddress($0) }
                )
            )
            .id(account.accountIdB64)
            .onAppear {
                // Same defensive teardown as the anon branch — no live
                // session runtime should linger while the keys are missing.
                services.deviceGroupSync.clear()
            }
        case .needsEarlierKeys(let account):
            // G-56 (UX identity-recovery section 8a) — signed in, keys here,
            // but not the account's earlier keys: the variant of the restore
            // prompt (another device, or a reset). The device looks for the
            // keys itself and the state flips to `.session` when they come.
            RestoreIdentityPromptScreen(
                authRepository: authRepository,
                viewModel: RestoreIdentityPromptViewModel(
                    repository: authRepository,
                    username: account.username,
                    accountIdB64: account.accountIdB64,
                    earlierKeys: true,
                    noteOwnAction: { [center = services.accountProtection] in center.noteOwnAction($0) },
                    keepAddress: { [center = services.accountProtection] in center.keepAddress($0) }
                )
            )
            .id("earlier-keys-" + account.accountIdB64)
            .onAppear {
                // No session runtime runs while parked.
                services.deviceGroupSync.clear()
            }
        case .saveFreshPhrase(let account):
            // A fresh phrase to confirm: after the person started over with
            // a new identity (already published), or took the account back.
            // Not after a sign-up (ADR-0075 D2), a guest's included.
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
    /// Section 9.5 — `path` once the channel `channelId` is left: every
    /// screen of it goes, whichever route opened it (the list's `.channel`,
    /// search's `.channelFound` / `.channelSubscribe`, its info), so the tab
    /// is back where the channel was opened from.
    static func leavingChannel(_ path: [TabRoute], channelId: String) -> [TabRoute] {
        path.filter {
            switch $0 {
            case .channel(let id), .channelInfo(let id, _), .channelFound(let id, _),
                 .channelSubscribe(let id, _, _):
                return id.caseInsensitiveCompare(channelId) != .orderedSame
            default:
                return true
            }
        }
    }

    case chat(groupIdHex: String)
    /// A group's info, pushed over its conversation
    /// (`docs/ux/conversation-info.md` section 3). Keyed on the same id the
    /// `.chat` route below it was opened with, so both share one view model.
    case groupInfo(groupIdHex: String)
    /// The Add members screen, pushed over a group's info (section 2.4).
    case addMembers(groupIdHex: String)
    /// A screen pushed over a group's info (invite links, reactions, group
    /// type, ownership transfer); same view model as the info.
    case groupInfoSubscreen(groupIdHex: String, screen: GroupInfoSubscreen)
    /// The contact picker behind the create menu's New chat.
    case newChat
    /// The create menu's Create group (see ``NewGroupScreen``).
    case newGroup
    /// ADR-0065 — the account's saved messages.
    case saved
    /// A channel this device holds, opened from the merged conversation list.
    case channel(channelId: String)
    /// A channel the network lookup found by `@handle`, which this device may
    /// not hold yet — the reader screen offers Subscribe.
    case channelFound(channelId: String, handle: String)
    /// `Subscribe` on the card of a public channel found by search
    /// (`docs/ux/group-and-channel-privacy.md` section 5.1): the channel,
    /// subscribed at once and named by its record.
    case channelSubscribe(channelId: String, handle: String, name: String)
    /// A channel's info, pushed over its conversation (section 12.1).
    case channelInfo(channelId: String, foundHandle: String)
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
    /// ADR-0075 — "Protect your account" behind the banner's `Protect it`.
    case protectAccount
    /// Settings > Account > Recovery email: add or replace, or (`remove`)
    /// the removal confirmation.
    case recoveryEmail(remove: Bool)
    /// Settings > Account > Save a recovery phrase.
    case savePhrase
    /// `It was not me` on an account notice.
    case secureAccount
    /// Settings > Security > Password: choose a new password from a device
    /// that holds the account's keys.
    case password
    case antiCensorship
    /// Settings > About > Support Lakko (`docs/ux/donations.md` section 4).
    case donations
    /// ADR-0059 section 6 — Settings -> Security -> Channels I share.
    case channelsIShare
    /// Settings > Language (ADR-0061).
    case language
    /// `docs/ux/chat-folders.md` section 4 — Settings > Chat folders, opened
    /// in its reorder state from a folder tab's Reorder folders.
    case chatFolders(reorder: Bool)
    /// Settings > Privacy > Who can find me (ADR-0060).
    case whoCanFindMe
    /// Settings > Privacy > Blocked people (ADR-0099, UX 1.1).
    case blockedPeople
    /// Settings > Privacy > Your reports (ADR-0099, UX 2.3).
    case yourReports
    /// Settings > About > Supporters (ADR-0097 section 2).
    case supporters
    /// Deep links (`/i/`, `/g/`, `/c/`), pushed over the current tab rather
    /// than replacing the app (UX spec section 3).
    case inviteLanding(slug: String)
    case groupJoinLanding(slug: String)
    case callJoinLanding(slug: String)
    /// ADR-0079 section 5 — the chat with a person added through their
    /// invite link before their app has answered (keyed on their account id
    /// hex); replaced by `.chat` once the one-to-one exists.
    case pendingChat(accountIdHex: String)

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
    /// `docs/ux/who-can-find-me.md` section 6.1 — one search session for the
    /// field at the top of Chats and the one at the top of Contacts: a text
    /// asked in one is not asked again in the other, and "Request sent"
    /// reads the same in both.
    let peopleSearchSession: PeopleSearchSession
    let chatsPeopleSearch: PeopleSearchModel
    let contactsPeopleSearch: PeopleSearchModel
    private var chatModels: [String: ChatViewModel] = [:]
    private var sharedChannelsWatch: AnyCancellable?
    private var contactsWatch: AnyCancellable?

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
        // 2026-11-20 (ADR-0074) — the channel proof mapping holds from the
        // first request of the session: owner proofs by the current key,
        // co-publisher proofs by the lineage key.
        if let seed = services.authRepository.identitySecrets?.accountIdentitySeed {
            ChannelPublisherIdentity.installSession(accountIdB64: accountIdB64, currentSeed: seed)
        }
        // ADR-0073 phase 2 (UX 16.6) — the session's key generation, so a
        // channel created under earlier keys reads as orphaned on the device
        // that reset them.
        channelsState.currentKeyGeneration = services.authRepository.identitySecrets?.session.keyGeneration
        channelPostSync = ChannelPostSync(accountIdB64: accountIdB64, channels: channelsState)
        channelPostSync.accountIdentitySeed = { [authRepository = services.authRepository] in
            authRepository.identitySecrets?.accountIdentitySeed
        }
        // G-50 — resume the channel PublisherGroup commits whose steps had not
        // all landed before the app was closed.
        if let seed = services.authRepository.identitySecrets?.accountIdentitySeed {
            let channels = channelsState
            Task { @MainActor in
                await ChannelPublisherCommits.replayAll(
                    accountIdB64: accountIdB64, currentSeed: seed, state: channels)
            }
        }
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
        // `docs/ux/channel-notifications.md` — every channel read the sync
        // makes may show a notification (``ChannelPostNotifier``).
        let deviceGroupSync = services.deviceGroupSync
        ChannelPostNotifier.attach(
            to: channelPostSync, channels: channelsState, reading: reading,
            flags: { [weak deviceGroupSync] in
                guard let deviceGroupSync, deviceGroupSync.ready else { return nil }
                return deviceGroupSync.flags
            })
        let chatRepository = services.chatRepository
        let channels = channelsState
        let chatList = chatListVM
        let contacts = services.contactsRepository
        let searchSession = PeopleSearchSession(ports: Self.peopleSearchPorts(
            contacts: contacts, chatRepository: chatRepository))
        peopleSearchSession = searchSession
        chatsPeopleSearch = PeopleSearchModel(session: searchSession)
        contactsPeopleSearch = PeopleSearchModel(session: searchSession)
        // A search row says what is true now: someone who became a contact, or
        // who left the outgoing list, changes it with the text still standing.
        // `$state` fires before the property is set, so the read comes after.
        contactsWatch = Self.contactsChanges(contacts)
            .sink { [weak searchSession] in
                MainActor.assumeIsolated { searchSession?.noteContactsChanged() }
            }
        // ADR-0087 section 9 — a channel's end, in both directions.
        channels.onUnsubscribedHere = { [weak chatRepository, weak channels] channelId in
            chatRepository?.noteChannelUnsubscribed(channelIdHex: channelId.lowercased())
            channels?.onRelationshipEnded?(channelId)
        }
        channels.onRemovedHere = { [weak chatRepository, weak channels] channelId, name in
            chatRepository?.noteChannelRemoved(channelIdHex: channelId.lowercased(), name: name)
            channels?.onRelationshipEnded?(channelId)
        }
        // ADR-0050 section 2.9 (amended 2026-11-13) — the channel subscription
        // register: this device's channels reach the account's other devices,
        // and theirs reach this one (`docs/ux/second-device.md` section 4a).
        ChannelSubscriptionSync.shared.bind(
            accountIdB64: accountIdB64, channels: channels, port: deviceGroupSync,
            changes: Publishers.MergeMany(
                deviceGroupSync.$channelSubscriptions.map { _ in () }.eraseToAnyPublisher(),
                deviceGroupSync.$ready.map { _ in () }.eraseToAnyPublisher(),
                deviceGroupSync.$endedConversations.map { _ in () }.eraseToAnyPublisher())
            .eraseToAnyPublisher())
        channels.onSubscribedHere = { [weak chatRepository] channelId in
            chatRepository?.noteChannelSubscribed(channelIdHex: channelId.lowercased())
        }
        // ADR-0087 section 4.5.4 — a newer channel name or description owes
        // the owner's device a republish of the public record; the owed flag
        // is durable, and the force run retries it now (later, at every
        // session start and every further change).
        channels.onSharedMetaAccepted = { [weak chatRepository] channelId, meta in
            guard let chat = chatRepository, let accountIdB64 = chat.currentAccountIdB64,
                  HeldHandleStore.noteChannelMeta(
                      channelIdHex: channelId, name: meta.name, description: meta.description, accountIdB64)
            else { return }
            Task { @MainActor [weak chat] in
                await HandleLeaseRenewal.shared.runIfDue(
                    accountIdB64: chat?.currentAccountIdB64,
                    accountIdentity: { chat?.accountIdentity },
                    groupFacts: { chat?.publicGroupFacts(journalIdHex: $0) },
                    force: true)
            }
        }
        chatRepository.onChannelEndedElsewhere = { [weak channels] channelIdHex in
            guard let channels else { return }
            ChannelUnsubscribe.endedElsewhere(state: channels, channelIdHex: channelIdHex)
        }
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

    /// Fires on the main queue after the contacts or the outgoing list changed.
    private static func contactsChanges(_ contacts: ContactsRepository) -> AnyPublisher<Void, Never> {
        struct Seen: Equatable {
            let keys: [Data]
            let sent: [Int64]
        }
        let seen: AnyPublisher<Seen, Never> = contacts.$state
            .map { state in
                Seen(keys: state.accepted.map { $0.request.fromPubkey }, sent: state.sent.map { $0.messageId })
            }
            .eraseToAnyPublisher()
        return seen
            .removeDuplicates()
            .dropFirst()
            .map { _ in () }
            .receive(on: DispatchQueue.main)
            .eraseToAnyPublisher()
    }

    /// The search field's ports (web `search/wiring.ts`): no rule of its own
    /// lives here.
    private static func peopleSearchPorts(
        contacts: ContactsRepository, chatRepository: ChatRepository
    ) -> PeopleSearchSession.Ports {
        func contactPairs() -> [(accountKey: Data, accountId: Data?)] {
            contacts.state.accepted.map {
                (accountKey: $0.request.fromPubkey, accountId: $0.request.fromAccountId)
            }
        }
        return PeopleSearchSession.Ports(
            candidates: {
                // Contacts and the members of this device's groups and
                // channels, with or without a grant (web `localPeople`).
                PeopleSearchWiring.localCandidates(
                    contacts: contactPairs(),
                    groupPeopleKeys: chatRepository.allMemberAccountKeys(),
                    profiles: ProfileDirectory.shared.receivedProfiles())
            },
            ownAccountKey: { ReachCenter.shared.ownAccountKey },
            network: { query in
                let contactKeys = Set(contactPairs().map { $0.accountKey.base64EncodedString() })
                return await PeopleSearch.answer(
                    query: query,
                    records: IntroductionBook.shared.relayedRecords()
                        + IntroductionBook.shared.offeredRecords(),
                    known: {
                        PeopleSearchWiring.known(
                            keyB64: $0.base64EncodedString(), contactKeys: contactKeys)
                    },
                    ownAccountKey: ReachCenter.shared.ownAccountKey,
                    ports: contacts.peopleSearchPorts(),
                    lookupChannel: { try await ChannelDiscovery.resolveChannelHandle($0) },
                    searchConversations: { query in
                        try await ConversationHandles.search(discovery: DiscoveryClient(), query: query)
                    })
            },
            reachCheck: { mailbox, seed in
                try await contacts.peopleSearchPorts().reachCheck(mailbox, seed)
            },
            sendRequest: { person in
                _ = try await contacts.sendContactRequest(to: person)
            },
            alreadyRequested: { person in
                SentRequests.holds(
                    contacts.state.sent, accountKey: person.accountKey, accountId: person.accountId)
                    || PendingContacts.holds(
                        contacts.invited, accountKey: person.accountKey, accountId: person.accountId)
            },
            sentRow: { person in
                SentRequests.row(
                    contacts.state.sent, accountKey: person.accountKey, accountId: person.accountId)
            },
            dismissSent: { contacts.dismissSentRequest($0) },
            heldGrants: { ReachCenter.shared.searchGrants(forAccountKeyB64: $0.base64EncodedString()) },
            isContact: { person in
                contacts.state.accepted.contains {
                    $0.request.fromPubkey == person.accountKey
                        || (person.accountId != nil && $0.request.fromAccountId == person.accountId)
                }
            },
            searchIdentity: {
                // Base scenario 9 — every key of this account: the current
                // one, its own predecessors, the key its chats are bound to.
                let resolver = ReachCenter.shared.keyResolver
                var own: [Data] = resolver.ownPredecessorKeys
                for key in [
                    resolver.ownCurrentKey, ReachCenter.shared.ownAccountKey,
                    chatRepository.accountIdentity?.signaturePublicKey(),
                    chatRepository.conversationAccountIdentity?.signaturePublicKey(),
                ] {
                    if let key { own.append(key) }
                }
                let idB64 = AcceptedContactsStore.boundAccountIdB64
                if let idB64,
                   let bound = KeychainOwnConversationKeyStore().load(accountIdB64: idB64)?.conversationKey {
                    own.append(bound)
                }
                return PeopleSearch.SearchIdentity(
                    ownKeys: own, ownAccountId: idB64.flatMap { Data(base64Encoded: $0) },
                    personOf: { resolver.current(of: $0) })
            },
            searchContacts: {
                contactPairs().map { PeopleSearch.SearchContact(accountKey: $0.accountKey, accountId: $0.accountId) }
            })
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

    /// The contacts of "Only the ones I choose", sorted as the Contacts screen
    /// sorts them. A contact with no account id cannot be chosen.
    private func visibilityContactRows() -> [VisibilityContactRow] {
        let accepted = contactsRepository.state.accepted.filter { $0.request.fromAccountId != nil }
        let byId = Dictionary(
            accepted.map { (base64Encode($0.request.fromAccountId ?? Data()), $0) },
            uniquingKeysWith: { first, _ in first })
        let items = byId.map { id, contact in
            let resolved = ProfileDirectory.shared.resolve(signatureKey: contact.request.fromPubkey)
            return ContactListPresentation.Item(
                id: id, name: resolved.name, username: resolved.verifiedUsername,
                isPlaceholder: resolved.isPlaceholder)
        }
        return ContactListPresentation.sections(items).flatMap(\.ids).compactMap { id in
            guard let contact = byId[id], let accountId = contact.request.fromAccountId else { return nil }
            let resolved = ProfileDirectory.shared.resolve(signatureKey: contact.request.fromPubkey)
            return VisibilityContactRow(
                accountId: accountId, accountKey: contact.request.fromPubkey, name: resolved.name)
        }
    }

    /// The incoming requests waiting, for the Contacts tab's badge. State of
    /// its own, so the shell re-runs when the count changes and not on every
    /// change of the contacts state.
    @State private var contactRequestsWaiting = 0

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
        TabView(selection: guardedTab) {
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
                    onSubscribeFoundChannel: { channelId, handle, name in
                        push(.channelSubscribe(channelId: channelId, handle: handle, name: name), on: .chats)
                    },
                    onOpenPerson: { openFoundPerson($0, on: .chats) },
                    onOpenFolderSettings: { reorder in
                        push(.chatFolders(reorder: reorder), on: .chats)
                    },
                    accountNotice: AccountNoticeBanner(center: services.accountProtection),
                    recalledNotice: RecalledNoticeBanner(
                        authRepository: authRepository,
                        // UX 18.3 — `Show new phrase` opens Settings >
                        // Security > Recovery, which asks for the proof as it
                        // always does.
                        onShowNewPhrase: {
                            selectedTab = .settings
                            setSettingsPath([.recoveryFactors])
                        }),
                    keysUpdatingLabel: KeysUpdatingLabel(authRepository: authRepository),
                    peopleSearch: models.chatsPeopleSearch,
                    accountIdB64: account.accountIdB64,
                    onOpenContacts: { selectedTab = .contacts }
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .chats) }
            }
            .tabItem { Label { Text(l10n.t("nav.chats")) } icon: { (selectedTab == .chats ? LkIcon.chatFill : LkIcon.chat).fixedImage } }
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
                    onSubscribeFoundChannel: { channelId, handle, name in
                        push(.channelSubscribe(channelId: channelId, handle: handle, name: name), on: .feed)
                    }
                )
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .feed) }
            }
            .tabItem { Label { Text(l10n.t("nav.feed")) } icon: { (selectedTab == .feed ? LkIcon.feedFill : LkIcon.feed).fixedImage } }
            .tag(SessionTab.feed)

            NavigationStack(path: $contactsPath) {
                ContactsScreen(
                    viewModel: contactsVM,
                    peopleSearch: models.contactsPeopleSearch,
                    onOpenPerson: { openFoundPerson($0, on: .contacts) },
                    onOpenLink: { inviteIntake.handle($0) })
                    .navigationDestination(for: TabRoute.self) { routeView($0, tab: .contacts) }
            }
            .tabItem { Label { Text(l10n.t("nav.contacts")) } icon: { (selectedTab == .contacts ? LkIcon.personSquareFill : LkIcon.personSquare).fixedImage } }
            .badge(ContactsViewModel.requestsBadge(waiting: contactRequestsWaiting).map { Text($0) })
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
                    }
                )
                // Calls placed before the index existed live only in their
                // conversations. Filled once per account, so this screen never
                // opens empty while the conversation list next to it is
                // visibly showing calls.
                .task { services.chatRepository.backfillCallHistory() }
                .navigationDestination(for: TabRoute.self) { routeView($0, tab: .calls) }
            }
            .tabItem { Label { Text(l10n.t("nav.calls")) } icon: { (selectedTab == .calls ? LkIcon.phoneFill : LkIcon.phone).fixedImage } }
            .tag(SessionTab.calls)

            SettingsTabAttention(selected: selectedTab == .settings) {
                NavigationStack(path: guardedSettingsPath) {
                    SettingsScreen(
                        account: account,
                        authRepository: authRepository,
                        accountProtection: services.accountProtection,
                        onOpen: { settingsPath.append($0) },
                        onLogout: authRepository.logout,
                        onForgetDevice: forgetDevice,
                        onAccountDeleted: { services.wipeAccountLocalData(accountIdB64: $0) },
                        announceDeletion: { [chat = services.chatRepository] in
                            try await chat.announceAccountDeleted()
                        }
                    )
                    .navigationDestination(for: TabRoute.self) { routeView($0, tab: .settings) }
                }
            }
            .tag(SessionTab.settings)
        }
        // UX group-and-channel-privacy 9.1 — "You are no longer in {name}."
        // at the bottom of whatever screen is up, for 4 seconds.
        .overlay(alignment: .bottom) {
            MembershipEndToast(center: chatRepository.membershipEndNotices)
                .animation(.easeOut(duration: 0.2), value: chatRepository.membershipEndNotices.shown)
        }
        // ADR-0099 / UX 2.3 — "Report sent. Moderators will review it."
        .overlay(alignment: .bottom) {
            ReportSentToast()
                .animation(.easeOut(duration: 0.2), value: ReportNoticeCenter.shared.shown)
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
        .onReceive(contactsVM.$repositoryState.map(\.pending.count).removeDuplicates()) {
            contactRequestsWaiting = $0
        }
        .task {
            while !Task.isCancelled {
                let started = Date()
                await contactsRepository.refreshInbox(
                    waitSecs: ContactsViewModel.defaultLongPollSecs)
                // A long-poll holds the connection itself; a poll that returned
                // at once (no secrets yet, a refused request, no network) must
                // not spin. The wait ends when the network comes back
                // (`docs/ux/connection-state.md` section 3).
                if Date().timeIntervalSince(started) < 1 {
                    await ConnectionState.sleepUnlessNetworkReturns(ms: 2_000)
                }
                await Task.yield()
            }
        }
        // UX 19.6 — `It was not me` on an account notice opens the one
        // screen that secures the account.
        .onReceive(services.accountProtection.$secureRequested) { requested in
            guard requested else { return }
            services.accountProtection.secureRequested = false
            // Reached from a notice: the password form of that one screen is
            // open for ten minutes without another proof.
            authRepository.openPasswordChangeForNotice()
            selectedTab = .settings
            setSettingsPath([.secureAccount])
        }
        .onReceive(services.accountProtection.$savePhraseRequested) { requested in
            guard requested else { return }
            services.accountProtection.savePhraseRequested = false
            selectedTab = .settings
            setSettingsPath([.savePhrase])
        }
        .onReceive(NewDeviceNoticeCenter.shared.$openDevicesRequested) { requested in
            guard requested else { return }
            NewDeviceNoticeCenter.shared.openDevicesRequested = false
            openDevices()
        }
        // `docs/ux/channel-notifications.md` section 1 — a tap on a channel
        // post notification opens the channel, at its newest end, where the
        // newest notified post is. Also the tap that launched the app: the
        // request waits in the shared center until this root reads it.
        .onReceive(ChannelNotificationTaps.shared.$openChannelRequested) { requested in
            guard let channelId = requested else { return }
            ChannelNotificationTaps.shared.openChannelRequested = nil
            openNotifiedChannel(channelId)
        }
        // `docs/ux/group-and-channel-privacy.md` section 8.1 — a tap on a
        // request-to-join notification opens the group's info at `Join
        // requests`, over its conversation.
        .onReceive(JoinRequestNotificationTaps.shared.$openGroupInfoRequested) { requested in
            guard let conversationIdHex = requested else { return }
            JoinRequestNotificationTaps.shared.openGroupInfoRequested = nil
            openJoinRequests(conversationIdHex)
        }
        // ADR-0074 section 3.5 — after a rotation of the account keys the
        // channels this device created follow the new owner key; the
        // session's key generation moves with them, so none reads as
        // orphaned by a reset.
        .onReceive(NotificationCenter.default.publisher(for: .lakkoAccountKeysRotated)) { _ in
            channelsState.keysRotated(
                currentKeyGeneration: authRepository.identitySecrets?.session.keyGeneration)
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
        // ADR-0065 sections 3 and 6 — owed blob deletes and the monthly keep
        // of pinned / saved / profile-photo files, on a slow foreground tick.
        .task(id: account.accountIdB64) {
            await BlobLifecycle.shared.runForegroundLoop()
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
        // ADR-0087 section 12 — the rotation, the public seed and the
        // co-publisher journal read of every channel this device publishes,
        // app-wide, so a channel rotates whether or not its owner opens it.
        .task(id: account.accountIdB64) {
            let loops = ChannelPublisherLoops.shared
            loops.reset()
            defer { loops.reset() }
            await loops.runLoop(
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
        // A restore that finished after the session started (it was owed).
        .onReceive(NotificationCenter.default.publisher(for: .lakkoBackupRestored)) { _ in
            Task { await services.chatRepository.republishProfileAfterRestore() }
        }
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
                // list during startup is never mistaken for "left every
                // group"; the driver uploads nothing until it is non-nil.
                knownGroupIds: {
                    chatRepository.state.bootstrapped
                        // Every GROUP, not every conversation (ADR-0073
                        // R.5): the older segments are the history.
                        ? chatRepository.knownGroupIdHexes
                        : nil
                },
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
                ReachCenter.shared.channelIdsProvider = { [channelsState] in
                    channelsState.channels.compactMap { CoPublisher.hexToData($0.id) }
                }
                // ADR-0087 section 14 — a channel invitation card in a chat:
                // `Open` when this device holds the channel, else `Subscribe`.
                ChannelInviteActions.shared.isHeld = { [channelsState] id in
                    channelsState.channels.contains { $0.id == id }
                        && (channelsState.subscriberKeysByChannel[id] != nil || channelsState.keysByChannel[id] != nil)
                }
                // An invitation that carries no channel name reads by the
                // name this device holds for it (``HeldChannelNames``).
                HeldChannelNames.shared.follow(channelsState.$channels.eraseToAnyPublisher())
                ChannelInviteActions.shared.subscribe = { channel in
                    openChannelInvitation(channel, on: selectedTab)
                }
                // ADR-0087 section 15.2 — a publisher card's Accept joins in place.
                CoPublisherAcceptance.shared.reset()
                CoPublisherAcceptance.shared.state = channelsState
                CoPublisherAcceptance.shared.seedProvider = { accountIdentitySeed }
                CoPublisherAcceptance.shared.openChannel = { channelId in
                    push(.channel(channelId: channelId), on: selectedTab)
                }
                // ADR-0045 decision 2 — a publisher request's Welcome lands
                // through the mailbox loop whenever the owner approves.
                contactsRepository.publisherWelcomeLander = { welcome, accountIdB64 in
                    // ADR-0090 section 2.3 — an owner device admitted this one
                    // (or a co-publisher's other device) to a channel's
                    // PublisherGroup: joined into the waiting channel.
                    if AcceptedContactsStore.boundAccountIdB64 == accountIdB64,
                       ChannelDeviceAdmission.landWelcome(welcome) != nil {
                        return true
                    }
                    return try CoPublisherAcceptance.shared.landPendingWelcome(welcome, accountIdB64: accountIdB64)
                }
                ChannelDeviceAdmission.waitingState = channelsState
                ChannelDeviceAdmission.currentSeed = { accountIdentitySeed }
                ReachCenter.shared.contactsProvider = { [contactsRepository] in
                    contactsRepository.state.accepted.compactMap { contact in
                        guard let id = contact.request.fromAccountId else { return nil }
                        return (accountKey: contact.request.fromPubkey, accountId: id)
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
                    // The certificate names THIS account's username. A fetch
                    // that returns after a sign-out, or after another account
                    // signed in, stores nothing: the reach record it would be
                    // written into is by then that account's, and every
                    // profile it sends would carry this one's certificate.
                    let bound = authRepository.binding.capture()
                    Task { @MainActor in
                        do {
                            let cert = try await DiscoveryClient(sessionToken: token)
                                .fetchUsernameCertificate()
                            guard bound.isCurrent else { return }
                            ReachCenter.shared.setOwnUsernameCertificate(cert)
                            // Everyone was published without the username
                            // until the certificate landed.
                            ReachCenter.shared.sync()
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
                // G-56 — the bootstrap refused (the account's earlier keys
                // are missing here) and the session parked: nothing below
                // runs (no mailbox claim, no KeyPackage).
                guard chatRepository.currentAccountIdB64 == account.accountIdB64 else { return }
                // A backup keeps received names in recent-use order after
                // contacts (`OwnBackupEnvelopes.receivedProfileRecords`).
                BackupPeopleOrder.source = { [weak chatRepository] in
                    chatRepository?.peopleByRecentUse() ?? []
                }
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
                // ADR-0074 section 2.3 — a device removal that was signed out
                // at the server but not finished (a crash, an offline step)
                // is retried at every landing, after the chats are loaded.
                let removal = services.deviceRemoval
                // ADR-0074 section 3.2 — a key rotation that committed and
                // still owes its publish steps finishes here too.
                let rotation = services.keyRotation
                Task { @MainActor in
                    removal.reload()
                    await removal.resumeOwed()
                    rotation.reload()
                    await rotation.resumeOwed()
                    // A restored backup owes the profile as any change does.
                    await services.chatRepository.republishProfileAfterRestore()
                    // ADR-0074 section 3.5a rules 3 and 5 — what a key change
                    // left owed on this device (a kept device, or any device
                    // after a restart) is retried at each session start.
                    await services.chatRepository.retryOwedProfileResend()
                    if rotation.resume == .nothing {
                        await services.contactsRepository.spendGrantsAfterAdoptingKeyRotation()
                    }
                }
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
        case .saved: push(.saved, on: .chats)
        }
    }

    /// `docs/ux/who-can-find-me.md` section 6.1 — **Open** on a found person
    /// who is already a contact: the conversation with them, else their
    /// contact page when this device holds no conversation yet.
    private func openFoundPerson(_ person: FoundPerson, on tab: SessionTab) {
        let resolver = ReachCenter.shared
        guard let contact = contactsRepository.state.accepted.first(where: {
            $0.request.fromPubkey == person.accountKey
                || resolver.currentAccountKey(of: $0.request.fromPubkey) == person.accountKey
        }) else { return }
        if let groupIdHex = contactsVM.chatGroupIdHex(for: contact) {
            push(.chat(groupIdHex: groupIdHex), on: tab)
        } else {
            push(.contactDetail(messageId: contact.request.messageId), on: tab)
        }
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
        case .noConversation:
            // Section 5.1.1: such a row has no call-back button; an ended
            // link call cannot be joined again.
            break
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

    /// ADR-0087 sections 7.2 and 14 — `Subscribe` on a link page or an
    /// invitation card: the channel opens subscribing with what the link
    /// carried, or opens when this device already holds it.
    private func openChannelInvitation(_ channel: ChannelLinkFacts, on tab: SessionTab) {
        if ChannelInviteActions.shared.isHeld(channel.channelIdHex) {
            push(.channel(channelId: channel.channelIdHex), on: tab)
            return
        }
        switch channel.access {
        case .public(let readKey):
            FoundChannelReadKeys.put(channel.channelIdHex, readKey)
        case .private(let inviteSeed, let admissionPublicKey, let admissionVersion):
            PendingChannelInvites.put(channel.channelIdHex, .init(
                inviteSeed: inviteSeed, admissionPublicKey: admissionPublicKey,
                admissionVersion: admissionVersion))
        }
        push(.channelSubscribe(channelId: channel.channelIdHex, handle: "", name: channel.name), on: tab)
    }

    /// ADR-0087 section 12 — one channel in the conversation view.
    @ViewBuilder
    private func channelConversation(
        channelId: String, seed: Data, tab: SessionTab,
        handle: String = "", name: String = "", subscribe: Bool = false
    ) -> some View {
        ChannelConversationScreen(
            channelId: channelId,
            state: channelsState,
            client: ChannelsClient(),
            accountIdentitySeed: seed,
            foundHandle: handle,
            foundName: name,
            foundReadKey: FoundChannelReadKeys.take(channelId),
            subscribeOnOpen: subscribe,
            onPostRead: { [feedReading = models.feedReading] in
                feedReading.advanceChannelRead($0, cursor: $1)
            },
            onOpenInfo: { push(.channelInfo(channelId: channelId, foundHandle: handle), on: tab) },
            // UX group-and-channel-privacy section 9 — after the end: Chats.
            onEndedHere: {
                path(for: tab).wrappedValue = []
                if tab != .chats { selectedTab = .chats }
            })
        .toolbar(.hidden, for: .tabBar)
        // `docs/ux/channel-notifications.md` section 2 — no notification for
        // the channel open in front of the person.
        .onAppear { VisibleChannel.appeared(channelId) }
        .onDisappear { VisibleChannel.disappeared(channelId) }
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
                // `docs/ux/conversation-info.md` section 3 — a group's info is
                // a push over the conversation, which stays as it was left.
                onOpenGroupInfo: {
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.openInfo(
                        binding.wrappedValue, groupIdHex: groupIdHex)
                },
                // ADR-0087 section 10 — `Add members` in a group of one:
                // the Add members screen, pushed over the info.
                onOpenAddMembers: {
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.openAddMembersFromConversation(
                        binding.wrappedValue, groupIdHex: groupIdHex)
                },
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
        case .groupInfo(let groupIdHex):
            // `docs/ux/conversation-info.md` sections 2.2 and 3 — the same
            // view model as the conversation under it, so the roster, the
            // pending set and the composer draft are one state.
            GroupInfoScreen(
                viewModel: chatViewModel(for: groupIdHex),
                contacts: contactsVM,
                onAddMembers: {
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.openAddMembers(
                        binding.wrappedValue, groupIdHex: groupIdHex)
                },
                // Section 2.2 — a member who is a contact opens their contact
                // screen, whose back returns here.
                onOpenContact: { messageId in push(.contactDetail(messageId: messageId), on: tab) },
                onOpenSubscreen: { screen in
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.openSubscreen(
                        binding.wrappedValue, groupIdHex: groupIdHex, screen)
                },
                onCloseSubscreen: { screen in
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.closeSubscreen(
                        binding.wrappedValue, groupIdHex: groupIdHex, screen)
                },
                // ADR-0045 UX 6.2.4 — the chat is gone: the info and the
                // conversation under it both go.
                onDeleted: {
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.afterDelete(
                        binding.wrappedValue, groupIdHex: groupIdHex)
                }
            )
            .toolbar(.hidden, for: .tabBar)
        case .groupInfoSubscreen(let groupIdHex, let screen):
            let vm = chatViewModel(for: groupIdHex)
            let close = {
                let binding = path(for: tab)
                binding.wrappedValue = GroupInfoNavigation.closeSubscreen(
                    binding.wrappedValue, groupIdHex: groupIdHex, screen)
            }
            Group {
                switch screen {
                case .inviteLinks:
                    GroupInviteLinksScreen(viewModel: vm)
                case .reactions:
                    AllowedReactionsScreen(viewModel: vm) { close() }
                case .groupType:
                    GroupTypeScreen(viewModel: vm) { close() }
                case .transferOwnership:
                    TransferOwnershipStep(viewModel: vm)
                        // The view model ends the step (a transfer that landed,
                        // Cancel); the screen it covers is not on screen to see
                        // that, so the step closes itself.
                        .onChange(of: vm.transferStepOpen) { _, open in
                            if !open { close() }
                        }
                        // A back from the step ends it as Cancel would.
                        .onDisappear { vm.transferStepOpen = false }
                }
            }
            .toolbar(.hidden, for: .tabBar)
        case .addMembers(let groupIdHex):
            AddMembersScreen(
                viewModel: chatViewModel(for: groupIdHex),
                contacts: contactsVM,
                session: models.peopleSearchSession,
                onAdded: {
                    let binding = path(for: tab)
                    binding.wrappedValue = GroupInfoNavigation.afterAdd(
                        binding.wrappedValue, groupIdHex: groupIdHex)
                }
            )
            .toolbar(.hidden, for: .tabBar)
        case .newChat:
            ContactPickerScreen(
                viewModel: contactsVM,
                title: l10n.t("chats.create.chat"),
                onPick: { groupIdHex in
                    replaceTop(route, with: .chat(groupIdHex: groupIdHex), in: tab)
                }
            )
        case .saved:
            SavedScreen(
                center: SavedMessagesCenter.shared,
                chat: chatRepository,
                headOf: { chatRepository.headGroupIdHex(of: $0) },
                title: { groupIdHex in chatListVM.title(groupIdHex: groupIdHex) },
                onOpen: { groupIdHex, rowId in
                    chatViewModel(for: groupIdHex).requestJump(to: rowId)
                    push(.chat(groupIdHex: groupIdHex), on: tab)
                })
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
            // ADR-0087 section 12 — a channel opens where a group opens, in
            // the same conversation view, pushed on the Chats stack.
            if let seed = accountIdentitySeed {
                channelConversation(channelId: channelId, seed: seed, tab: tab)
            } else {
                channelSeedMissing(l10n.t("channels.fallback_name"))
            }
        case .channelSubscribe(let channelId, let handle, let name):
            if let seed = accountIdentitySeed {
                channelConversation(
                    channelId: channelId, seed: seed, tab: tab, handle: handle, name: name, subscribe: true)
            } else {
                channelSeedMissing(l10n.t("channels.fallback_name"))
            }
        case .channelFound(let channelId, let handle):
            // A channel the ONE network lookup resolved by `@handle`. This
            // device may hold no seed for it yet: Subscribe shows.
            if let seed = accountIdentitySeed {
                channelConversation(channelId: channelId, seed: seed, tab: tab, handle: handle)
            } else {
                channelSeedMissing(l10n.t("channels.fallback_name"))
            }
        case .channelInfo(let channelId, let handle):
            if let seed = accountIdentitySeed {
                ChannelInfoSheet(
                    channelId: channelId,
                    state: channelsState,
                    client: ChannelsClient(),
                    accountIdentitySeed: seed,
                    foundHandle: handle,
                    contacts: contactsVM,
                    chat: chatRepository,
                    onUnsubscribed: {
                        // Section 9.5 — the row and its posts are gone: the
                        // info and the conversation under it both go.
                        let binding = path(for: tab)
                        binding.wrappedValue = TabRoute.leavingChannel(binding.wrappedValue, channelId: channelId)
                    })
                .toolbar(.hidden, for: .tabBar)
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
                    onCreated: { channelId in
                        replaceTop(route, with: .channel(channelId: channelId), in: tab)
                    })
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
            // ADR-0084 + UX `profile-screen.md` — the avatar, the name and
            // the username edited in place; Save publishes into every group.
            ProfileScreen(
                model: ProfileEditModel.live(
                    authRepository: authRepository,
                    account: account,
                    protection: services.accountProtection,
                    republish: {
                        await chatRepository.publishOwnProfileToAllGroups()
                        // ADR-0060 — a new name changes what Everyone publishes
                        // and what the introduction record is findable by.
                        ReachCenter.shared.sync()
                        await chatRepository.sendIntroductions()
                    }),
                proof: FreshAuthProofModel(surface: .changeUsername, repository: authRepository),
                tintIdentity: account.accountIdB64
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
                removal: services.deviceRemoval,
                rotation: services.keyRotation,
                onAddDevice: { push(.addDevice, on: tab) },
                onChangePassword: { push(.password, on: tab) },
                onDeviceRemoved: { deviceKey in
                    // UX 17.5 — removing a device that carried `New` ends its
                    // notice and raises the change-password line.
                    NewDeviceNoticeCenter.shared.removed(signaturePublicKey: deviceKey)
                    // The removal itself (the sign-out at the server, the
                    // directory row, the DeviceGroup, every chat) and its
                    // retries are ``DeviceRemovalRunner``'s (ADR-0074 2.3).
                }
            )
        case .addDevice:
            AddDeviceScreen(
                authRepository: authRepository,
                onDeviceAdded: { deviceKey in
                    // UX 17.5 — this device let it in: no notice about it here.
                    NewDeviceNoticeCenter.shared.letIn(
                        accountIdB64: account.accountIdB64, signaturePublicKey: deviceKey)
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
                },
                // UX section 2.1 — linked: back to the Devices list.
                onLinked: { pop(.addDevice, from: tab) }
            )
        case .recoveryFactors:
            RecoverySettingsScreen(
                authRepository: authRepository,
                onOpen: { push($0, on: tab) }
            )
        case .protectAccount:
            ProtectAccountScreen(
                center: services.accountProtection,
                hasUsername: !account.username.isEmpty,
                onAddEmail: { push(.recoveryEmail(remove: false), on: tab) },
                onAddPasskey: { push(.passkeys, on: tab) },
                onSavePhrase: { push(.savePhrase, on: tab) }
            )
        case .recoveryEmail(let remove):
            RecoveryEmailScreen(
                authRepository: authRepository,
                center: services.accountProtection,
                startsRemoving: remove,
                onDone: { pop(.recoveryEmail(remove: remove), from: tab) }
            )
        case .savePhrase:
            SavePhraseScreen(
                authRepository: authRepository,
                center: services.accountProtection,
                rotation: services.keyRotation,
                onDone: { pop(.savePhrase, from: tab) }
            )
        case .secureAccount:
            SecureAccountScreen(
                authRepository: authRepository,
                center: services.accountProtection,
                onOpen: { push($0, on: tab) }
            )
        case .password:
            PasswordSettingsScreen(authRepository: authRepository)
        case .antiCensorship:
            AntiCensorshipSettingsView(manager: AntiCensorshipManager.shared)
        case .donations:
            DonationsScreen()
        case .channelsIShare:
            ChannelsIShareScreen(
                hub: chatRepository.sharedChannels,
                title: { groupIdHex in
                    chatRepository.state.threads.contains { $0.groupIdHex == groupIdHex }
                        ? chatListVM.title(groupIdHex: groupIdHex) : nil
                })
        case .language:
            LanguageScreen(showsTranslationSettings: true)
        case .supporters:
            SupportersScreen(center: SupportersListCenter.shared)
        case .chatFolders(let reorder):
            ChatFoldersSettingsScreen(
                startInReorder: reorder, viewModel: chatListVM, channels: channelsState,
                feedReading: models.feedReading)
        case .blockedPeople:
            BlockedPeopleScreen()
        case .yourReports:
            YourReportsScreen(accountIdB64: chatRepository.currentAccountIdB64)
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
                },
                contacts: visibilityContactRows())
        case .inviteLanding(let slug):
            InviteLandingScreen(
                viewModel: InviteLandingViewModel(
                    slug: slug,
                    repository: contactsRepository,
                    intake: inviteIntake
                ),
                onDone: { pop(route, from: tab) },
                // ADR-0079 — the page closes into the chat with the inviter.
                onOpenChat: { target in
                    switch target {
                    case .pending(let accountIdHex):
                        replaceTop(route, with: .pendingChat(accountIdHex: accountIdHex), in: tab)
                    case .conversation(let groupIdHex):
                        replaceTop(route, with: .chat(groupIdHex: groupIdHex), in: tab)
                    case .contact(let messageId):
                        replaceTop(route, with: .contactDetail(messageId: messageId), in: tab)
                    }
                },
                // Amended 2026-10-19 — a link that does not work: Contacts,
                // nothing else shown.
                onOpenContacts: {
                    pop(route, from: tab)
                    selectedTab = .contacts
                }
            )
        case .pendingChat(let accountIdHex):
            PendingChatScreen(
                accountIdHex: accountIdHex,
                contacts: contactsRepository,
                chat: chatRepository,
                onConnected: { groupIdHex in
                    replaceTop(route, with: .chat(groupIdHex: groupIdHex), in: tab)
                })
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
                },
                // ADR-0087 section 7.2 — a channel's link page: Subscribe
                // opens the channel subscribing, with what the link carried.
                onSubscribeChannel: { channel in
                    switch channel.access {
                    case .public(let readKey):
                        FoundChannelReadKeys.put(channel.channelIdHex, readKey)
                    case .private(let inviteSeed, let admissionPublicKey, let admissionVersion):
                        PendingChannelInvites.put(channel.channelIdHex, .init(
                            inviteSeed: inviteSeed, admissionPublicKey: admissionPublicKey,
                            admissionVersion: admissionVersion))
                    }
                    if channelsState.channels.contains(where: { $0.id == channel.channelIdHex }),
                       channelsState.subscriberKeysByChannel[channel.channelIdHex] != nil
                        || channelsState.keysByChannel[channel.channelIdHex] != nil {
                        replaceTop(route, with: .channel(channelId: channel.channelIdHex), in: tab)
                    } else {
                        replaceTop(route, with: .channelSubscribe(
                            channelId: channel.channelIdHex, handle: "", name: channel.name), in: tab)
                    }
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
            // Another link in the same place gets its own model.
            .id(slug)
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

    /// UX `profile-screen.md` section 6 — another tab is a navigation away
    /// from Settings > Profile: it asks first while a change is pending.
    private var guardedTab: Binding<SessionTab> {
        Binding(
            get: { selectedTab },
            set: { next in
                guard next != selectedTab, selectedTab == .settings,
                      settingsPath.contains(.profile) else {
                    selectedTab = next
                    return
                }
                if ProfileLeaveGuard.shared.allows({ selectedTab = next }) { selectedTab = next }
            })
    }

    /// The same for a Settings path that drops the profile screen (the tab
    /// tapped again, a path the shell replaces).
    private var guardedSettingsPath: Binding<[TabRoute]> {
        Binding(
            get: { settingsPath },
            set: { next in setSettingsPath(next) })
    }

    private func setSettingsPath(_ next: [TabRoute]) {
        guard settingsPath.contains(.profile), !next.contains(.profile) else {
            settingsPath = next
            return
        }
        if ProfileLeaveGuard.shared.allows({ settingsPath = next }) { settingsPath = next }
    }

    private func push(_ route: TabRoute, on tab: SessionTab) {
        path(for: tab).wrappedValue.append(route)
    }

    /// A tap on a channel post notification: the channel on the Chats
    /// stack, unless it is already the screen on top. A channel this device
    /// no longer holds opens nothing.
    private func openNotifiedChannel(_ channelId: String) {
        guard models.channelsState.channels.contains(where: { $0.id == channelId }) else { return }
        selectedTab = .chats
        if chatsPath.last != .channel(channelId: channelId) {
            push(.channel(channelId: channelId), on: .chats)
        }
    }

    /// Section 8.1 (group-and-channel-privacy) — a tap on a request-to-join
    /// notification: the conversation on the Chats stack (returned to when
    /// already there) and its info over it, scrolled to `Join requests`. A
    /// conversation this device no longer holds opens nothing.
    private func openJoinRequests(_ conversationIdHex: String) {
        let repository = services.chatRepository
        guard repository.state.threads.contains(where: { $0.conversationIdHex == conversationIdHex })
        else { return }
        selectedTab = .chats
        chatViewModel(for: conversationIdHex).reviewJoinRequests()
        returnToOrPush(.chat(groupIdHex: conversationIdHex), on: .chats)
        chatsPath = GroupInfoNavigation.openInfo(chatsPath, groupIdHex: conversationIdHex)
    }

    /// UX 17.5 — a tap on the new-device notification: Settings > Devices.
    private func openDevices() {
        selectedTab = .settings
        if settingsPath.last != .devices { setSettingsPath([.devices]) }
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
        accountProtection: AccountProtectionCenter,
        onSignedUp: @escaping () -> Void,
        initialRoute: AnonRoute? = nil
    ) {
        // The step after a sign-up ("Protect your account") is the app
        // root's, shown over the session; the view model only tells it.
        self._signupVM = StateObject(
            wrappedValue: SignupViewModel(repository: repository, onSignedUp: onSignedUp)
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
                cachedUsername: cachedUsername,
                noteOwnAction: { [weak accountProtection] in accountProtection?.noteOwnAction($0) },
                keepAddress: { [weak accountProtection] in accountProtection?.keepAddress($0) }
            )
        )
        // A returning user lands on Log in; a fresh device on Welcome. The
        // signed-out invite page opens the screen the person chose there.
        if let initialRoute {
            self._path = State(initialValue: [initialRoute])
        } else {
            self._path = State(initialValue: cachedUsername == nil ? [] : [.login])
        }
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
enum AnonRoute: Hashable {
    case signup
    case login
    case recover
}

/// UX 17.5 — the Settings tab, carrying the exclamation-mark badge while a
/// device added to the account is pending here. Its own view observes the
/// warning, so the session root is not re-evaluated by it.
private struct SettingsTabAttention<Content: View>: View {
    @ObservedObject private var notice = NewDeviceNoticeCenter.shared
    @ObservedObject private var l10n = Localization.shared
    /// Whether the Settings tab is the selected one: a selected tab shows
    /// the filled icon (design system section 8).
    let selected: Bool
    @ViewBuilder var content: Content

    var body: some View {
        let attention = notice.badgeVisible
        content
            .tabItem {
                Label { Text(l10n.t("nav.settings")) } icon: {
                    (selected ? LkIcon.gearFill : LkIcon.gear).fixedImage
                }
                    .accessibilityLabel(
                        attention ? l10n.t("nav.settings.attention_a11y") : l10n.t("nav.settings"))
            }
            .badge(attention ? Text(verbatim: "!") : nil)
    }
}

/// The process entry point.
///
/// `LakkoTests` is app-hosted: XCTest injects the test bundle into a launch
/// of this app. Launched as ``LakkoApp`` that process restored the
/// simulator's signed-in account from the Keychain and started the session
/// runtime (token refresh, the foreground receive, the device-link poll,
/// device cards, KeyPackage publish), whose requests the test bundle's
/// network guard then charged to whichever test was running. As a test host
/// the process runs ``LakkoTestHostApp`` instead: one empty window scene and
/// nothing else, so a test sees only what it set up itself. The simulator's
/// stored account is left as it is. Debug builds only; a Release build always
/// runs ``LakkoApp``.
@main
enum LakkoMain {
    static func main() {
#if DEBUG
        if LakkoTestHost.isActive {
            LakkoTestHostApp.main()
            return
        }
#endif
        LakkoApp.main()
    }
}

#if DEBUG
/// Whether this process was launched by XCTest to host the unit tests.
enum LakkoTestHost {
    /// XCTest sets these in the environment of the process it injects a test
    /// bundle into. A UI test's target app is an ordinary launch and has none.
    static var isActive: Bool {
        let environment = ProcessInfo.processInfo.environment
        return environment["XCTestConfigurationFilePath"] != nil
            || environment["XCTestBundlePath"] != nil
            || environment["XCTestSessionIdentifier"] != nil
    }
}

/// The app a unit-test run is hosted in: a window scene (some tests attach
/// views to it) and the two process-wide steps ``LakkoApp`` takes before
/// anything touches WebRTC. No repository, no session, no network.
struct LakkoTestHostApp: App {
    init() {
        WebRTCFieldTrials.installOnce()
        SFrameAvailability.shared.runBootProbe()
    }

    var body: some Scene {
        WindowGroup {
            Color.clear
        }
    }
}
#endif
