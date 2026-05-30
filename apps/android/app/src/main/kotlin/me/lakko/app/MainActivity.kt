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
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
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
import me.lakko.app.ui.contacts.ContactsScreen
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.contacts.InviteLandingScreen
import me.lakko.app.ui.contacts.InviteLandingViewModel
import me.lakko.app.ui.devices.DevicesScreen
import me.lakko.app.ui.home.HomeScreen

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
            chatRepo = chatRepository,
        )
        // Phase 2a3e2b-Android — process-scoped cross-device
        // sync runtime. Bootstrapped from `SessionRoot` once
        // identitySecrets land; cleared on logout /
        // forgetDevice from the same place ChatRepository
        // gets cleared.
        val deviceGroupSync = me.lakko.app.devices.sync
            .DeviceGroupSyncRepository(applicationContext)

        // Read the launching Intent — a cold-start tap on
        // `https://lakko.me/i/<slug>` carries the URL in
        // `intent.data`; a cold-start tap on a Lakko push
        // notification carries the wake extras
        // (EXTRA_WAKE_GROUP / EXTRA_WAKE_POINTER). `onNewIntent`
        // below handles the warm-start variants of both.
        handleInviteIntent(intent)
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
) {
    val state by authRepository.state.collectAsStateWithLifecycle()

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
        is AuthState.Session -> SessionRoot(
            authRepository = authRepository,
            contactsRepository = contactsRepository,
            chatRepository = chatRepository,
            deviceGroupSync = deviceGroupSync,
            inviteIntake = inviteIntake,
            pushIntake = pushIntake,
            account = current.account,
        )
    }
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
            onOpenPasskeys = { destination = SessionDestination.Passkeys },
            onOpenDevices = { destination = SessionDestination.Devices },
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
                    ChatViewModel(chatRepository, groupIdHex)
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
    data class Chat(val groupIdHex: String) : SessionDestination
    data class InviteLanding(val slug: String) : SessionDestination
    /** 2a2e — passkey management surface. */
    data object Passkeys : SessionDestination
    /** 2a3b4 — DeviceGroup pairing surface. */
    data object Devices : SessionDestination
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
