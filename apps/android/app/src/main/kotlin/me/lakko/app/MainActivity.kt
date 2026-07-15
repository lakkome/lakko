// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.viewmodel.compose.viewModel
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.SessionAccount
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.InviteIntake
import me.lakko.app.push.LakkoMessagingService
import me.lakko.app.push.PushIntake
import me.lakko.app.ui.auth.LoginScreen
import me.lakko.app.ui.auth.LoginViewModel
import me.lakko.app.ui.auth.RecoverScreen
import me.lakko.app.ui.auth.RecoverViewModel
import me.lakko.app.ui.auth.SignupScreen
import me.lakko.app.ui.auth.SignupViewModel
import me.lakko.app.ui.chat.ChatListScreen
import me.lakko.app.ui.chat.ChatScreen
import me.lakko.app.ui.chat.ChatViewModel
import me.lakko.app.ui.channels.ChannelsRootScreen
import me.lakko.app.ui.contacts.ContactsScreen
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.contacts.InviteLandingScreen
import me.lakko.app.ui.contacts.InviteLandingViewModel
import me.lakko.app.transport.AntiCensorshipManager
import me.lakko.app.ui.devices.DevicesScreen
import me.lakko.app.ui.home.HomeScreen
import me.lakko.app.ui.transport.AntiCensorshipSettingsScreen

/**
 * Single-activity Compose host.
 *
 * Phase 1.15f4c — drives the **signup-or-login-or-home-or-
 * contacts-or-chats-or-chat** flow:
 *
 *   * `Anon` → [LoginScreen] / [SignupScreen] / [RecoverScreen].
 *   * `Session` → [HomeScreen] / [ContactsScreen] /
 *     [ChatListScreen] / [ChatScreen] (per groupIdHex).
 *
 * Three repositories live at the Activity scope:
 *   * [AuthRepository] — auth state + identity secrets.
 *   * [ContactsRepository] — pending / sent / accepted contacts.
 *   * [ChatRepository] — MLS groups + per-thread message timeline.
 *
 * Lifecycle wire-up (1.15f4c):
 *   * On every transition into a `Session` state we observe
 *     [AuthRepository.identitySecrets]; the first non-null
 *     value drives [ChatRepository.bootstrap]. Subsequent
 *     re-emissions of the same secrets are idempotent.
 *   * Logout → `contacts.clear()` → `chat.clear()` →
 *     `auth.logout()`. Order matters: drop ephemeral state
 *     BEFORE flipping the auth state, so the AnonRoot doesn't
 *     briefly observe a Session-side flow.
 *   * Forget device → `contacts.clear()` → `chat.forgetDevice()`
 *     → `auth.forgetDevice()` — same order, but
 *     `chat.forgetDevice()` additionally wipes the on-disk
 *     snapshot envelope under the current accountId before
 *     [AuthRepository.forgetDevice] clears the
 *     [me.lakko.app.auth.SessionStore] marker. (If we cleared
 *     the SessionStore first, ChatRepository would lose the
 *     accountId reference and skip the deleteSnapshot.)
 */
class MainActivity : ComponentActivity() {

    /** Process-scoped intake for invite slugs from
     *  `https://lakko.me/i/<slug>` deep links. `onCreate` +
     *  `onNewIntent` publish to this holder; `SessionRoot`
     *  collects it and routes to [InviteLandingScreen]. */
    private val inviteIntake = InviteIntake()

    /** Process-scoped intake for push-wake hints (Phase 1.20d2).
     *  `onCreate` + `onNewIntent` read the FCM extras the
     *  notification tap carried over from
     *  [LakkoMessagingService.showWakeNotification] and publish
     *  to this holder; `SessionRoot` collects it and routes to
     *  the relevant chat thread (or falls back to the chat list
     *  when the group is unknown locally). */
    private val pushIntake = PushIntake()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        // G3-sframe-native N-5 — runtime probe for the public
        // org.webrtc.RtpSender.setFrameEncryptor /
        // RtpReceiver.setFrameDecryptor methods libwebrtc M125
        // exposes on Android. We deliberately call this BEFORE
        // any other side effect because the probe's NO result
        // becomes a process-wide gate: every call entry point
        // gates on `SFrameAvailability.isCallingAvailable` and
        // aborts if false. There is no silent fallback to
        // non-E2EE media — security invariant #4 forbids it.
        //
        // The probe is cheap (a handful of Class.forName +
        // getMethod calls) and the result is cached for the
        // process lifetime.
        me.lakko.sframe.SFrameAvailability.bootProbe()

        val authRepository = AuthRepository(applicationContext)
        // ChatRepository before ContactsRepository — the
        // contacts side optionally bridges to chat via
        // joinGroupFromWelcome, and the order of construction
        // makes the dependency obvious in the call site.
        val chatRepository = ChatRepository(applicationContext)
        val contactsRepository = ContactsRepository(
            authRepo = authRepository,
            // G-4 (ADR-0016) — the app context backs the persistent
            // PeerPinStore so the fetch-path supersession gate is
            // active on every real send.
            appContext = applicationContext,
            chatRepo = chatRepository,
        )
        // Phase 2a3e2b-Android — process-scoped cross-device
        // sync runtime. Bootstrapped from `SessionRoot` once
        // identitySecrets land; cleared on logout /
        // forgetDevice from the same place ChatRepository
        // gets cleared.
        val deviceGroupSync = me.lakko.app.devices.sync
            .DeviceGroupSyncRepository(applicationContext)

        // ADR-0026 Phase 4 — the anti-censorship arming controller.
        // Apply the persisted preference at boot: when on, fetch +
        // verify the signed bootstrap config and publish the armed
        // ambient config so the clients route through the obfuscated
        // tunnel. Disarmed when off, so this is a no-op by default.
        val antiCensorship = AntiCensorshipManager(applicationContext, lifecycleScope)
        antiCensorship.armFromSettings()

        // Read the launching Intent — a cold-start tap on
        // `https://lakko.me/i/<slug>` carries the URL in
        // `intent.data`; a cold-start tap on a Lakko push
        // notification carries the wake extras
        // (EXTRA_WAKE_GROUP / EXTRA_WAKE_POINTER). `onNewIntent`
        // below handles the warm-start variants of both.
        handleInviteIntent(intent)
        handleGroupJoinIntent(intent)
        handleCallJoinIntent(intent)
        handleWakeIntent(intent)

        setContent {
            MaterialTheme {
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppRoot(
                        authRepository = authRepository,
                        contactsRepository = contactsRepository,
                        chatRepository = chatRepository,
                        deviceGroupSync = deviceGroupSync,
                        inviteIntake = inviteIntake,
                        pushIntake = pushIntake,
                        antiCensorship = antiCensorship,
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // Standard `singleTask` /`launchMode = singleInstance`
        // shape: warm-start deep links + push taps route here.
        // We DON'T call `setIntent(intent)` because the Compose
        // tree doesn't observe `getIntent()` directly — we
        // publish into the appropriate intake whose StateFlow
        // drives the re-render.
        handleInviteIntent(intent)
        handleGroupJoinIntent(intent)
        handleCallJoinIntent(intent)
        handleWakeIntent(intent)
    }

    /**
     * Extract an invite slug from [intent]'s `data` URI when
     * the URL matches `https?://lakko.me/i/<slug>`, then hand
     * it to [InviteIntake.publish] so the Compose router can
     * navigate to the landing screen.
     *
     * Defensive against:
     *   * Non-VIEW intents (LAUNCHER taps land here too on
     *     every restart with the same Intent stored as the
     *     "last intent" — those don't carry a `data` URI
     *     unless the user actually deep-linked in).
     *   * Wrong-host URLs (the manifest filter narrows to
     *     `lakko.me`, but if the user opens the same URL on a
     *     self-hosted mirror that happens to match the http
     *     filter we still want to handle it).
     *   * Empty or whitespace slugs (the manifest's
     *     `pathPrefix="/i/"` allows `/i/` with no slug at all
     *     — extract to "" + skip).
     */
    private fun handleInviteIntent(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        val path = uri.path ?: return
        // Manifest filter pinned `pathPrefix="/i/"`; the slug
        // is everything after that. `substringAfter` returns
        // the original string when the separator is missing —
        // guard with a startsWith check first.
        if (!path.startsWith("/i/")) return
        val slug = path.removePrefix("/i/").trim().removeSuffix("/")
        if (slug.isEmpty()) return
        inviteIntake.publish(slug)
    }

    /**
     * ADR-0028 (G-3) — extract a group join-ticket slug from [intent]'s `data`
     * URI when the URL matches `https?://lakko.me/g/<slug>`, then hand it to
     * [InviteIntake.publishGroupJoin]. Mirror of [handleInviteIntent] for the
     * `/g/` deep-link.
     */
    private fun handleGroupJoinIntent(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        val path = uri.path ?: return
        if (!path.startsWith("/g/")) return
        val slug = path.removePrefix("/g/").trim().removeSuffix("/")
        if (slug.isEmpty()) return
        inviteIntake.publishGroupJoin(slug)
    }

    /**
     * ADR-0032 (G-21/G-22) — extract a multi-joiner call-invite slug from
     * [intent]'s `data` URI when the URL matches `https?://lakko.me/c/<slug>`,
     * then hand it to [InviteIntake.publishCallJoin]. Mirror of
     * [handleInviteIntent] for the `/c/` deep-link. Unlike the other two, the
     * call-join landing is a PUBLIC route honoured in any auth state (an
     * anonymous visitor gets the ADR-0032 guest-join path).
     */
    private fun handleCallJoinIntent(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        val path = uri.path ?: return
        if (!path.startsWith("/c/")) return
        val slug = path.removePrefix("/c/").trim().removeSuffix("/")
        if (slug.isEmpty()) return
        inviteIntake.publishCallJoin(slug)
    }

    /**
     * Extract a push-wake hint from [intent]'s extras when the
     * notification tap from
     * [LakkoMessagingService.showWakeNotification] landed here
     * (Phase 1.20d2). Publish to [pushIntake] so the Compose
     * router can navigate to the matching chat.
     *
     * Defensive against:
     *   * Non-notification intents (LAUNCHER taps land here too
     *     on every restart and the cold-start LaunchActivity
     *     Intent gets re-delivered without the wake extras —
     *     guard on the presence of [LakkoMessagingService.EXTRA_WAKE_GROUP]).
     *   * Empty / blank group hex (treated as absent).
     *
     * Pointer is optional — the wake hint can carry it as a
     * cursor advance for the next mailbox poll. Phase-1's chat
     * surface ignores it; preserved in the [PushIntake.Wake]
     * payload for a future "jump to message id" surface.
     */
    private fun handleWakeIntent(intent: Intent) {
        val groupHex = intent.getStringExtra(LakkoMessagingService.EXTRA_WAKE_GROUP)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?: return
        // `EXTRA_WAKE_POINTER` is stored as a `Long` extra by
        // `LakkoMessagingService.showWakeNotification` — read it
        // back as a long, but treat the default value `-1L` as
        // "not present" so a never-set extra surfaces as `null`
        // in the wake payload.
        val rawPointer = intent.getLongExtra(LakkoMessagingService.EXTRA_WAKE_POINTER, -1L)
        val pointer = if (rawPointer >= 0L) rawPointer else null
        pushIntake.publish(PushIntake.Wake(groupHex = groupHex, pointer = pointer))
    }
}

@Composable
private fun AppRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
    deviceGroupSync: me.lakko.app.devices.sync.DeviceGroupSyncRepository,
    inviteIntake: InviteIntake,
    pushIntake: PushIntake,
    antiCensorship: AntiCensorshipManager,
) {
    val state by authRepository.state.collectAsStateWithLifecycle()

    // ADR-0032 (G-21/G-22) — a tapped `https://lakko.me/c/<slug>` multi-joiner
    // call-invite link takes over the whole screen with the PUBLIC JoinCall
    // landing, in ANY auth state (an anonymous visitor gets the guest-join
    // path). This gates before the auth-state router so a guest mid-onboarding
    // (auth flips to SaveFreshPhrase) stays inside the JoinCall context instead
    // of falling into the top-level SaveFreshPhraseRoot.
    val pendingCallJoinSlug by inviteIntake.pendingCallJoinSlug.collectAsStateWithLifecycle()
    val callSlug = pendingCallJoinSlug
    if (callSlug != null) {
        me.lakko.app.ui.call.JoinCallRoot(
            authRepository = authRepository,
            slug = callSlug,
            applicationContext = LocalContext.current.applicationContext,
            onExit = { inviteIntake.consumeCallJoin() },
        )
        return
    }

    // Phase 2a3e2b-Android — tear down the cross-device sync
    // runtime when we leave the session entirely (logout /
    // forgetDevice flips us to Anon). Bootstrap fires from
    // SessionRoot's LaunchedEffect block.
    LaunchedEffect(state is AuthState.Session) {
        if (state !is AuthState.Session) {
            deviceGroupSync.clear()
        }
    }

    when (val current = state) {
        AuthState.Loading -> LoadingScreen()
        is AuthState.Anon -> AnonRoot(
            repository = authRepository,
            cachedUsername = current.cachedUsername,
        )
        // ADR-0017 fresh-device fail-safe: auth succeeded but no
        // cached AccountIdentity (phrase) seed on this device — prompt
        // for the recovery phrase (HARD-BLOCK verified) before opening
        // a full session. NEVER derive the identity from the export key
        // or mint fresh here.
        is AuthState.NeedsRecoveryPhrase -> RestoreIdentityRoot(
            repository = authRepository,
            account = current.account,
        )
        // ADR-0017 lost-everything fallback: the user minted a brand-new
        // identity (already published, no proof); show the new recovery
        // phrase for them to save before the session lands.
        is AuthState.SaveFreshPhrase -> SaveFreshPhraseRoot(
            repository = authRepository,
        )
        is AuthState.Session -> {
            // ADR-0017 defence-in-depth — surface the identity-changed
            // security alert above the session content.
            val identityAlert by authRepository.identityAlert.collectAsState()
            androidx.compose.foundation.layout.Column {
                if (identityAlert != null) {
                    me.lakko.app.ui.auth.IdentitySecurityBanner(
                        onDismiss = { authRepository.dismissIdentityAlert() },
                    )
                }
                SessionRoot(
                    authRepository = authRepository,
                    contactsRepository = contactsRepository,
                    chatRepository = chatRepository,
                    deviceGroupSync = deviceGroupSync,
                    inviteIntake = inviteIntake,
                    pushIntake = pushIntake,
                    antiCensorship = antiCensorship,
                    account = current.account,
                )
            }
        }
    }
}

/**
 * ADR-0017 fresh-device fail-safe subgraph — renders the
 * [RestoreIdentityPrompt] that completes the session once the user
 * supplies their recovery phrase. "Sign out" routes back to the Anon
 * flow via [AuthRepository.logout] (which clears the parked
 * fail-safe context).
 */
@Composable
private fun RestoreIdentityRoot(
    repository: AuthRepository,
    account: SessionAccount,
) {
    val vm: me.lakko.app.ui.auth.RestoreIdentityPromptViewModel = viewModel(
        factory = singletonViewModelFactory {
            me.lakko.app.ui.auth.RestoreIdentityPromptViewModel(repo = repository)
        },
    )
    me.lakko.app.ui.auth.RestoreIdentityPrompt(
        viewModel = vm,
        username = account.username,
        accountIdB64 = account.accountIdB64,
        onSignOut = { repository.logout() },
    )
}

/**
 * ADR-0017 lost-everything fallback — renders the freshly-minted recovery
 * phrase for the user to save, then finishes the session.
 */
@Composable
private fun SaveFreshPhraseRoot(
    repository: AuthRepository,
) {
    val phrase = repository.freshPhrase() ?: ""
    me.lakko.app.ui.auth.SaveFreshPhraseScreen(
        phrase = phrase,
        onConfirm = { repository.confirmFreshPhraseSaved() },
    )
}

/**
 * Session-state subgraph. Adds ChatRepository bootstrap +
 * a four-way [SessionDestination] router (Home / Contacts /
 * Chats / Chat(groupIdHex)). Compose Navigation lands later
 * (deferred 1.15d7); the local destination state + a
 * `String?`-shaped Chat argument is plenty for four screens.
 */
@Composable
private fun SessionRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
    deviceGroupSync: me.lakko.app.devices.sync.DeviceGroupSyncRepository,
    inviteIntake: InviteIntake,
    pushIntake: PushIntake,
    antiCensorship: AntiCensorshipManager,
    account: SessionAccount,
) {
    // Drive ChatRepository.bootstrap whenever a fresh
    // identity-secrets value lands. The first non-null emission
    // happens immediately after signup / login / recover; the
    // bootstrap call is idempotent for the same account id, so
    // repeated emissions (state restoration across config
    // changes) cost nothing.
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    LaunchedEffect(identitySecrets) {
        val secrets = identitySecrets ?: return@LaunchedEffect
        chatRepository.bootstrap(secrets)
        // Phase 2a3e2b — bootstrap the cross-device sync
        // runtime alongside the chat bootstrap so the poll
        // loop starts at the same moment chat surfaces become
        // usable. Idempotent for the same account id.
        deviceGroupSync.bootstrap(secrets)
    }

    // Auto-clear chat in-memory state when we leave the
    // Session entirely (e.g. via process death after logout).
    // The explicit logout / forgetDevice handlers below cover
    // the user-initiated paths; this DisposableEffect catches
    // the natural composition exit.
    DisposableEffect(Unit) {
        onDispose { /* keep state — the next SessionRoot re-mount
                       will bootstrap again from identitySecrets */ }
    }

    var destination: SessionDestination by remember {
        mutableStateOf(SessionDestination.Home)
    }

    // Observe pending invite slugs (1.15e11). When a deep
    // link arrives — either at cold start (MainActivity's
    // onCreate published it before SessionRoot mounted) or
    // during a warm session (onNewIntent published mid-
    // session) — route to InviteLanding(slug) and consume()
    // the slot so a subsequent slug for a different invite
    // can land cleanly.
    val pendingInviteSlug by inviteIntake.pendingInviteSlug.collectAsStateWithLifecycle()
    LaunchedEffect(pendingInviteSlug) {
        val slug = pendingInviteSlug ?: return@LaunchedEffect
        destination = SessionDestination.InviteLanding(slug)
        inviteIntake.consume()
    }

    // ADR-0028 (G-3) — a tapped `/g/<slug>` group join-ticket link routes to
    // the group-join landing, mirroring the invite-landing flow above.
    val pendingGroupJoinSlug by inviteIntake.pendingGroupJoinSlug.collectAsStateWithLifecycle()
    LaunchedEffect(pendingGroupJoinSlug) {
        val slug = pendingGroupJoinSlug ?: return@LaunchedEffect
        destination = SessionDestination.GroupJoinLanding(slug)
        inviteIntake.consumeGroupJoin()
    }

    // Observe pending push wakes (Phase 1.20d2). A notification
    // tap delivers a group-pseudonym hex to MainActivity, which
    // publishes here via `pushIntake`. We route to the matching
    // chat thread when the group is known locally; otherwise we
    // fall back to the chat list (a stale wake — the group
    // record was wiped after the push was minted, or this
    // device joined the conversation under a different account).
    //
    // Chat-state freshness matters: we read
    // `chatRepository.state.threads` snapshot at the moment the
    // wake lands. A "known" group whose record was just wiped
    // by `forgetDevice()` still races; the fallback to the chat
    // list catches that case cleanly (the user lands on a list
    // that no longer shows the thread, which matches the
    // "you've left this conversation" mental model).
    val pendingWake by pushIntake.pendingWake.collectAsStateWithLifecycle()
    val chatState by chatRepository.state.collectAsStateWithLifecycle()
    LaunchedEffect(pendingWake, chatState.bootstrapped) {
        val wake = pendingWake ?: return@LaunchedEffect
        // Wait for ChatRepository.bootstrap to finish populating
        // the threads list before deciding whether the wake's
        // group is locally known. The bootstrap LaunchedEffect
        // above fires on the first identity-secrets emission;
        // until it completes, `chatState.threads` may be empty.
        if (!chatState.bootstrapped) return@LaunchedEffect
        val knownThread = chatState.threads.firstOrNull {
            it.groupIdHex.equals(wake.groupHex, ignoreCase = true)
        }
        destination = if (knownThread != null) {
            SessionDestination.Chat(knownThread.groupIdHex)
        } else {
            // Stale wake — group is gone or never was. The chat
            // list is the safe landing pad; the user can scan
            // their thread inventory from there.
            SessionDestination.Chats
        }
        pushIntake.consume()
    }

    when (val current = destination) {
        SessionDestination.Home -> HomeScreen(
            account = account,
            onOpenContacts = { destination = SessionDestination.Contacts },
            onOpenChats = { destination = SessionDestination.Chats },
            onOpenChannels = { destination = SessionDestination.Channels },
            onOpenPasskeys = { destination = SessionDestination.Passkeys },
            onOpenDevices = { destination = SessionDestination.Devices },
            onOpenAntiCensorship = { destination = SessionDestination.AntiCensorship },
            onLogout = {
                contactsRepository.clear()
                chatRepository.clear()
                authRepository.logout()
            },
            onForgetDevice = {
                contactsRepository.clear()
                // Note: chat.forgetDevice() BEFORE
                // auth.forgetDevice() — see the file-level
                // docstring for the ordering rationale.
                chatRepository.forgetDevice()
                authRepository.forgetDevice()
            },
        )

        SessionDestination.Contacts -> {
            val contactsVm: ContactsViewModel = viewModel(
                factory = singletonViewModelFactory {
                    ContactsViewModel(
                        repo = contactsRepository,
                        chatRepo = chatRepository,
                    )
                },
            )
            ContactsScreen(
                viewModel = contactsVm,
                onNavigateBack = { destination = SessionDestination.Home },
                onOpenChat = { groupIdHex ->
                    destination = SessionDestination.Chat(groupIdHex)
                },
            )
        }

        SessionDestination.Chats -> ChatListScreen(
            repository = chatRepository,
            onNavigateBack = { destination = SessionDestination.Home },
            onOpenThread = { groupIdHex ->
                destination = SessionDestination.Chat(groupIdHex)
            },
            // Phase 2a3e2c-Android — surfaces pin / mute /
            // archive indicators + swipe / icon-tap toggles
            // backed by the cross-device sync store.
            deviceGroupSync = deviceGroupSync,
        )

        is SessionDestination.Chat -> {
            val groupIdHex = current.groupIdHex
            // Key the ChatViewModel by groupIdHex so navigating
            // to a different thread rebuilds the VM with the
            // right per-thread state binding.
            val chatVm: ChatViewModel = viewModel(
                key = "chat-$groupIdHex",
                factory = singletonViewModelFactory {
                    ChatViewModel(
                        repo = chatRepository,
                        groupIdHex = groupIdHex,
                        // SFU group-call admission mints a JWT from the
                        // account session bearer. The AuthRepository owns
                        // it; thread it in as a closure so the VM has no
                        // auth-layer dependency. `null` when signed out →
                        // startGroupCall refuses.
                        sessionTokenProvider = {
                            authRepository.identitySecrets.value?.sessionToken
                        },
                        // A4 — resolve @username -> (accountId, signaturePubkey)
                        // through the G-4-verified contacts path for add-member.
                        resolveRecipient = { username ->
                            contactsRepository.resolveVerifiedRecipient(username)
                        },
                    )
                },
            )
            // Phase 2a3e2b-Android — wire cross-device sync
            // into this VM's composer on every navigation.
            // Idempotent on the same `(vm, sync)` pair so the
            // re-runs from Compose recomposition cost nothing.
            LaunchedEffect(chatVm) {
                chatVm.attachSync(deviceGroupSync)
            }
            ChatScreen(
                viewModel = chatVm,
                // Back from a 1×1 chat goes to the chat list,
                // not Home — standard chat-app navigation
                // shape.
                onNavigateBack = { destination = SessionDestination.Chats },
            )
        }

        SessionDestination.Channels -> {
            // Phase 4 (ADR-0010) — broadcast channels. Threads the
            // AccountIdentity (phrase) seed (ADR-0017) from the live
            // IdentitySecrets so a publisher can rebuild the MLS
            // PublisherGroup. In a live Session the secrets are always
            // present (bootstrap ran on entry); the null guard is purely
            // defensive and recomposes into Channels once they land.
            val secrets = identitySecrets
            if (secrets != null) {
                ChannelsRootScreen(
                    accountIdentitySeed = secrets.accountIdentitySeed,
                    sessionToken = secrets.sessionToken,
                    onBack = { destination = SessionDestination.Home },
                )
            } else {
                LoadingScreen()
            }
        }

        SessionDestination.Passkeys -> {
            val passkeysVm: me.lakko.app.webauthn.PasskeysViewModel = viewModel(
                factory = singletonViewModelFactory {
                    me.lakko.app.webauthn.PasskeysViewModel(repo = authRepository)
                },
            )
            me.lakko.app.webauthn.PasskeysScreen(
                viewModel = passkeysVm,
                onBack = { destination = SessionDestination.Home },
            )
        }

        SessionDestination.Devices -> DevicesScreen(
            authRepository = authRepository,
            onBack = { destination = SessionDestination.Home },
        )

        SessionDestination.AntiCensorship -> AntiCensorshipSettingsScreen(
            manager = antiCensorship,
            onBack = { destination = SessionDestination.Home },
        )

        is SessionDestination.InviteLanding -> {
            val inviteSlug = current.slug
            // Key the VM by slug so re-tapping a different
            // invite URL (already-consumed-and-published-
            // again via onNewIntent) builds a fresh VM with
            // the right slug binding + auto-runs the Accept
            // flow against the new slug.
            val inviteVm: InviteLandingViewModel = viewModel(
                key = "invite-$inviteSlug",
                factory = singletonViewModelFactory {
                    InviteLandingViewModel(contactsRepository, inviteSlug)
                },
            )
            InviteLandingScreen(
                viewModel = inviteVm,
                onDone = { destination = SessionDestination.Home },
                onOpenChat = { groupIdHex ->
                    destination = SessionDestination.Chat(groupIdHex)
                },
            )
        }

        is SessionDestination.GroupJoinLanding -> {
            // ADR-0028 (G-3) — request to join the group the `/g/<slug>` link
            // points at + poll for the admin's approval. Keyed by slug so a
            // re-tapped link rebuilds a fresh VM.
            val groupJoinSlug = current.slug
            val groupJoinVm: me.lakko.app.ui.chat.JoinGroupLandingViewModel = viewModel(
                key = "group-join-$groupJoinSlug",
                factory = singletonViewModelFactory {
                    me.lakko.app.ui.chat.JoinGroupLandingViewModel(chatRepository, groupJoinSlug)
                },
            )
            me.lakko.app.ui.chat.JoinGroupLandingScreen(
                viewModel = groupJoinVm,
                onDone = { destination = SessionDestination.Chats },
            )
        }
    }
}

/**
 * Five Session-state destinations. Compose Navigation lands in
 * a later sprint — this sealed hierarchy holds the place. The
 * [Chat] + [InviteLanding] variants carry id args so their
 * ViewModels know which thread / invite to project.
 */
private sealed interface SessionDestination {
    data object Home : SessionDestination
    data object Contacts : SessionDestination
    data object Chats : SessionDestination

    /** Phase 4 (ADR-0010) — broadcast channels surface. */
    data object Channels : SessionDestination
    data class Chat(val groupIdHex: String) : SessionDestination
    data class InviteLanding(val slug: String) : SessionDestination

    /** ADR-0028 (G-3) — group join-ticket landing for a tapped `/g/<slug>`
     *  link. Carries the slug so the VM knows which group to request to join. */
    data class GroupJoinLanding(val slug: String) : SessionDestination
    /** 2a2e — passkey management surface. */
    data object Passkeys : SessionDestination
    /** 2a3b4 — DeviceGroup pairing surface. */
    data object Devices : SessionDestination

    /** ADR-0026 — anti-censorship arming surface. */
    data object AntiCensorship : SessionDestination
}

/**
 * Anon-state subgraph — picks Login, Signup, or Recover. The
 * default destination is keyed on [cachedUsername]. All three
 * screens carry footer links to switch between them.
 */
@Composable
private fun AnonRoot(
    repository: AuthRepository,
    cachedUsername: String?,
) {
    var destination by remember(cachedUsername) {
        mutableStateOf(
            if (cachedUsername != null) AnonDestination.Login
            else AnonDestination.Signup,
        )
    }

    when (destination) {
        AnonDestination.Login -> {
            val loginVm: LoginViewModel = viewModel(
                key = "login-${cachedUsername ?: ""}",
                factory = singletonViewModelFactory {
                    LoginViewModel(repository, cachedUsername)
                },
            )
            LoginScreen(
                viewModel = loginVm,
                onNavigateToSignup = { destination = AnonDestination.Signup },
                onNavigateToRecover = { destination = AnonDestination.Recover },
            )
        }
        AnonDestination.Signup -> {
            val signupVm: SignupViewModel = viewModel(
                factory = singletonViewModelFactory { SignupViewModel(repository) },
            )
            SignupScreen(
                viewModel = signupVm,
                onAuthenticated = {},
                onNavigateToLogin = { destination = AnonDestination.Login },
            )
        }
        AnonDestination.Recover -> {
            val recoverVm: RecoverViewModel = viewModel(
                key = "recover-${cachedUsername ?: ""}",
                factory = singletonViewModelFactory {
                    RecoverViewModel(repository, cachedUsername)
                },
            )
            RecoverScreen(
                viewModel = recoverVm,
                onNavigateToLogin = { destination = AnonDestination.Login },
                onNavigateToSignup = { destination = AnonDestination.Signup },
            )
        }
    }
}

private enum class AnonDestination {
    Login,
    Signup,
    Recover,
}

/**
 * Tiny inline [ViewModelProvider.Factory] that constructs a single
 * VM via the supplied [block].
 */
private inline fun <reified VM : ViewModel> singletonViewModelFactory(
    crossinline block: () -> VM,
): ViewModelProvider.Factory = object : ViewModelProvider.Factory {
    @Suppress("UNCHECKED_CAST")
    override fun <T : ViewModel> create(modelClass: Class<T>): T {
        require(modelClass.isAssignableFrom(VM::class.java)) {
            "Unexpected ViewModel class: $modelClass"
        }
        return block() as T
    }
}

/**
 * Loading splash for the brief window between Activity start
 * and [AuthRepository] rehydration. Practically invisible
 * (rehydrate is synchronous), but rendered for symmetry.
 */
@Composable
private fun LoadingScreen() {
    Text(
        text = "Lakko",
        style = MaterialTheme.typography.displaySmall,
        modifier = Modifier.padding(24.dp),
    )
}
