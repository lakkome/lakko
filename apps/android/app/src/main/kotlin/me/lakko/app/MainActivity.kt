// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.statusBars
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.ChatBubble
import androidx.compose.material.icons.filled.DynamicFeed
import androidx.compose.material.icons.filled.People
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.Call
import androidx.compose.material.icons.outlined.ChatBubbleOutline
import androidx.compose.material.icons.outlined.DynamicFeed
import androidx.compose.material.icons.outlined.People
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import me.lakko.app.ui.theme.LakkoTheme
import me.lakko.app.ui.theme.ThemePreference
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
import androidx.core.content.ContextCompat
import androidx.compose.ui.unit.dp
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import androidx.lifecycle.viewmodel.compose.viewModel
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.SessionAccount
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.contacts.InviteIntake
import me.lakko.app.feed.reporting
import me.lakko.app.push.LakkoMessagingService
import me.lakko.app.push.PushIntake
import me.lakko.app.ui.auth.LoginScreen
import me.lakko.app.ui.call.LinkCallNoticeBar
import me.lakko.app.ui.call.OneToOneCallOverlay
import me.lakko.app.ui.auth.LoginViewModel
import me.lakko.app.ui.auth.RecoverScreen
import me.lakko.app.ui.auth.RecoverViewModel
import me.lakko.app.ui.auth.SignupScreen
import me.lakko.app.ui.auth.SignupViewModel
import me.lakko.app.ui.chat.ChatListScreen
import me.lakko.app.ui.chat.ChatScreen
import me.lakko.app.ui.chat.ChatViewModel
import me.lakko.app.ui.channels.ChannelDetailScreen
import me.lakko.app.ui.channels.ChannelManageScreen
import me.lakko.app.ui.channels.ChannelSubscribeScreen
import me.lakko.app.ui.channels.JoinCoPublisherScreen
import me.lakko.app.ui.channels.NewChannelScreen
import me.lakko.app.ui.channels.rememberChannelsUiState
import me.lakko.app.ui.contacts.ContactsScreen
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.contacts.InviteLandingScreen
import me.lakko.app.ui.contacts.InviteLandingViewModel
import me.lakko.app.transport.AntiCensorshipManager
import me.lakko.app.ui.devices.DevicesScreen
import me.lakko.app.ui.settings.SettingsScreen
import me.lakko.app.ui.chat.NewChatScreen
import me.lakko.app.ui.chat.NewGroupScreen
import me.lakko.app.ui.transport.AntiCensorshipSettingsScreen
import me.lakko.app.ui.updates.UpdateAvailableBanner
import me.lakko.app.updates.ReleaseUpdateChecker
import me.lakko.app.updates.UpdateBannerDecision

/**
 * Single-activity Compose host.
 *
 * Phase 1.15f4c — drives the **signup-or-login-or-home-or-
 * contacts-or-chats-or-chat** flow:
 *
 *   * `Anon` → [me.lakko.app.ui.auth.WelcomeScreen], with [LoginScreen] /
 *     [SignupScreen] / [RecoverScreen] pushed from it.
 *   * `Session` → the tabbed shell (Chats / Groups / Channels / Contacts /
 *     Settings) with [ChatScreen] pushed per groupIdHex.
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
 *   * Forget device and Delete account → the in-memory runtimes
 *     stop (`contacts.clear()`, channels, `chat.forgetDevice()`,
 *     device sync) → `auth.forgetDevice()`, which runs the one
 *     on-device wipe ([me.lakko.app.account.AccountLocalWipe]:
 *     every per-account store) and ends the session. Log out keeps
 *     the account's local data for its next sign-in.
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
        // Client redesign Phase A — edge-to-edge with transparent system bars;
        // every screen pads by its own Scaffold / safe-drawing insets.
        enableEdgeToEdge()
        ThemePreference.init(applicationContext)

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
        // Every call lives in the process-wide call host, not in a screen: the
        // 1x1 call must ring whichever screen is showing, and no call — 1x1,
        // group or call link — may end or vanish because its screen went away.
        // Every poll of every group hands one-to-one call signaling to it, and
        // every settled group reaches the group-call rekey. An activity
        // recreate builds a new repository, so the wiring is re-attached here
        // each time.
        me.lakko.app.calls.AppCallHost.attach(
            applicationContext = applicationContext,
            repository = chatRepository,
            sessionTokenProvider = { authRepository.identitySecrets.value?.sessionToken },
        )
        val callHost = me.lakko.app.calls.AppCallHost.instance
        val contactsRepository = ContactsRepository(
            authRepo = authRepository,
            // G-4 (ADR-0016) — the app context backs the persistent
            // PeerPinStore so the fetch-path supersession gate is
            // active on every real send.
            appContext = applicationContext,
            chatRepo = chatRepository,
        )
        // ADR-0060 §4 / §5 — contacts of contacts: an offer or a bundle a
        // contact sent is kept, and the relay re-runs; a changed setting
        // re-runs the offer. Re-attached on every activity recreate, like the
        // call host above.
        chatRepository.introductionSink = { sender, kind, bytes ->
            me.lakko.app.reach.IntroductionIntake.keepInBackground(
                reach = me.lakko.app.reach.ReachManager.get(applicationContext),
                senderKey = sender,
                kind = kind,
                bytes = bytes,
                then = { chatRepository.runIntroductionsBestEffort() },
            )
        }
        chatRepository.visibilityListener = { chatRepository.runIntroductionsBestEffort() }
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

        // ADR-0038 S4 (G-11-adjacent) — the "update available" notice controller.
        // On launch (throttled to once per 24h internally) it fetches + verifies
        // the signed release manifest against the build-time-pinned publisher key
        // and publishes an UpdateBannerDecision the AppRoot mounts. Dormant when no
        // release key is pinned; fail-safe toward showing no banner on any fetch /
        // verify failure. The fetch rides the anti-censorship tunnel when armed.
        val releaseChecker = ReleaseUpdateChecker(applicationContext, lifecycleScope)
        releaseChecker.checkOnLaunch()

        // Read the launching Intent — a cold-start tap on
        // `https://lakko.me/i/<slug>` carries the URL in
        // `intent.data`; a cold-start tap on a Lakko push
        // notification carries the wake extras
        // (EXTRA_WAKE_GROUP / EXTRA_WAKE_POINTER). `onNewIntent`
        // below handles the warm-start variants of both.
        handleInviteIntent(intent)
        handleGroupJoinIntent(intent)
        handleCallJoinIntent(intent)
        handleFeedImportIntent(intent)
        handleFeedFileIntent(intent)
        handleWakeIntent(intent)
        handleCallRingIntent(intent)

        setContent {
            LakkoTheme {
                // System-bar icon contrast follows the resolved scheme (the user
                // may pin Dark while the OS is light), not the OS setting.
                val dark = me.lakko.app.ui.theme.isLakkoDark()
                LaunchedEffect(dark) {
                    val transparent = android.graphics.Color.TRANSPARENT
                    enableEdgeToEdge(
                        statusBarStyle = if (dark) {
                            androidx.activity.SystemBarStyle.dark(transparent)
                        } else {
                            androidx.activity.SystemBarStyle.light(transparent, transparent)
                        },
                        navigationBarStyle = if (dark) {
                            androidx.activity.SystemBarStyle.dark(transparent)
                        } else {
                            androidx.activity.SystemBarStyle.light(transparent, transparent)
                        },
                    )
                }
                Surface(
                    modifier = Modifier.fillMaxSize(),
                    color = MaterialTheme.colorScheme.background,
                ) {
                    AppRoot(
                        authRepository = authRepository,
                        contactsRepository = contactsRepository,
                        chatRepository = chatRepository,
                        callHost = callHost,
                        deviceGroupSync = deviceGroupSync,
                        inviteIntake = inviteIntake,
                        pushIntake = pushIntake,
                        antiCensorship = antiCensorship,
                        releaseChecker = releaseChecker,
                    )
                }
            }
        }
    }

    /**
     * Tell the ringer whether the app is showing, so a ring raised while the
     * user is in the app never posts a notification: the platform downgrades a
     * full-screen intent to a heads-up on an unlocked phone, and that strip
     * lands on top of the app's own full-screen incoming call and folds away
     * (`docs/ux/calls.md` section 2.1, first row).
     */
    override fun onResume() {
        super.onResume()
        me.lakko.app.calls.IncomingCallRinger.attach(applicationContext)
        me.lakko.app.calls.IncomingCallRinger.setAppForeground(true)
    }

    override fun onPause() {
        super.onPause()
        me.lakko.app.calls.IncomingCallRinger.setAppForeground(false)
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
        handleFeedImportIntent(intent)
        handleFeedFileIntent(intent)
        handleWakeIntent(intent)
        handleCallRingIntent(intent)
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
        // Sealed links (2026-09-28): the `#<link secret>` fragment opens the
        // link, so it travels with the slug as one argument; a link that lost
        // it is refused as damaged on the landing, before any request.
        inviteIntake.publish(
            me.lakko.app.discovery.InviteLinkArgument(slug, uri.fragment?.takeIf { it.isNotBlank() }).encode(),
        )
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
        // Sealed links — the `#<link secret>` fragment opens the link and its
        // preview. It is read here and nowhere else: `Uri.fragment` is decoded
        // and never sent. A URL that lost it, or carries a wrong one, is
        // refused as damaged on the landing, not here.
        inviteIntake.publishGroupJoin(slug, uri.fragment?.takeIf { it.isNotBlank() })
    }

    /**
     * ADR-0061 / `docs/ux/feed.md` section 4.1 — a shared feed link
     * `https?://lakko.me/feed/import#<rule>`. The rule rides in the fragment,
     * which is read here on the device and never sent anywhere.
     */
    private fun handleFeedImportIntent(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        val path = uri.path ?: return
        if (path.trimEnd('/') != "/feed/import") return
        me.lakko.app.ui.feed.FeedImportIntake.publish(uri.encodedFragment)
    }

    /**
     * `docs/ux/feed.md` section 5 — a `.lakkofeed` file opened from Files,
     * mail or a messenger ("Open with", or shared to Lakko). It lands on the
     * same import screen as the link; whether the bytes are a feed is the
     * core's to say there, with the same refusal.
     */
    private fun handleFeedFileIntent(intent: Intent) {
        val uri: android.net.Uri = when (intent.action) {
            Intent.ACTION_VIEW -> intent.data?.takeIf { it.scheme == "content" || it.scheme == "file" }
            Intent.ACTION_SEND -> if (android.os.Build.VERSION.SDK_INT >= 33) {
                intent.getParcelableExtra(Intent.EXTRA_STREAM, android.net.Uri::class.java)
            } else {
                @Suppress("DEPRECATION")
                intent.getParcelableExtra(Intent.EXTRA_STREAM)
            }
            else -> null
        } ?: return
        me.lakko.app.ui.feed.FeedImportIntake.publishFile(uri)
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
        // Sealed links: the fragment opens the capability and travels with
        // the slug; the landing refuses a link without it as damaged.
        inviteIntake.publishCallJoin(
            me.lakko.app.discovery.InviteLinkArgument(slug, uri.fragment?.takeIf { it.isNotBlank() }).encode(),
        )
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

    /**
     * `docs/ux/calls.md` section 2.2 — the user answered on the ring screen,
     * which then started the app. Hand the choice to the call host.
     *
     * The host may not have a session for that call yet: an offer that arrived
     * while the app was backgrounded is only HELD, and a cold start has not
     * polled at all. It therefore remembers the answer and takes it the moment
     * the call rings there, which is what makes answering from a locked screen
     * work with the app closed.
     */
    private fun handleCallRingIntent(intent: Intent) {
        val command = me.lakko.app.calls.callRingCommand(
            action = intent.action,
            groupIdHex = intent.getStringExtra(me.lakko.app.calls.CALL_RING_EXTRA_GROUP),
            callIdHex = intent.getStringExtra(me.lakko.app.calls.CALL_RING_EXTRA_CALL_ID),
        ) ?: return
        when (command.action) {
            me.lakko.app.calls.CallRingAction.ACCEPT,
            me.lakko.app.calls.CallRingAction.ACCEPT_AUDIO_ONLY,
            -> me.lakko.app.calls.AppCallHost.instance
                .acceptRinging(command.callIdHex, command.withVideo)
            // Declining never reaches here: it is handled without opening the
            // app (`IncomingCallActionReceiver`).
            me.lakko.app.calls.CallRingAction.DECLINE -> Unit
        }
    }
}

@Composable
private fun AppRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
    callHost: me.lakko.app.calls.OneToOneCallHost<me.lakko.app.calls.CallSession>,
    deviceGroupSync: me.lakko.app.devices.sync.DeviceGroupSyncRepository,
    inviteIntake: InviteIntake,
    pushIntake: PushIntake,
    antiCensorship: AntiCensorshipManager,
    releaseChecker: ReleaseUpdateChecker,
) {
    val state by authRepository.state.collectAsStateWithLifecycle()

    // ADR-0032 (G-21/G-22) — a tapped `https://lakko.me/c/<slug>` multi-joiner
    // call-invite link opens the PUBLIC JoinCall landing, in ANY auth state (an
    // anonymous visitor gets the guest-join path). Without a session it takes
    // over the whole screen, gated before the auth-state router so a guest
    // mid-onboarding (auth flips to SaveFreshPhrase) stays inside the JoinCall
    // context instead of falling into the top-level SaveFreshPhraseRoot. With a
    // session it is drawn ABOVE the session screens instead (see the end of
    // this function): the session root keeps running underneath — its receive
    // loop is what answers a 1x1 offer busy during the link call.
    val pendingCallJoinSlug by inviteIntake.pendingCallJoinSlug.collectAsStateWithLifecycle()
    val callSlug = pendingCallJoinSlug
    val appContext = LocalContext.current.applicationContext
    if (callSlug != null && state !is AuthState.Session) {
        me.lakko.app.ui.call.JoinCallRoot(
            authRepository = authRepository,
            slug = callSlug,
            applicationContext = appContext,
            onExit = { inviteIntake.consumeCallJoin() },
            host = me.lakko.app.calls.AppCallHost.link,
            endCurrentCall = { me.lakko.app.calls.AppCallHost.endCurrentCall() },
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
            // A phrase can only be parked by a sign-up that landed a session;
            // leaving the session drops it (it is never shown twice).
            me.lakko.app.ui.auth.SignupPhraseGate.clear()
        }
    }

    // ADR-0048 — one resolver for every call surface below: the collapsed bar
    // (section 4.5) and the in-call screens must name a call identically, so
    // they read the same directory and the same version rather than each
    // collecting their own.
    val profileDirectory = remember(chatRepository) {
        me.lakko.app.profile.ProfileDirectory.get(chatRepository.context)
    }
    val nameVersion by profileDirectory.version.collectAsStateWithLifecycle()
    val resolveOneToOneName: (String) -> String? = { groupIdHex ->
        nameVersion.let {
            chatRepository.oneToOnePeerKey(groupIdHex)?.let { key ->
                profileDirectory.resolve(key).primary
            }
        }
    }
    val resolveGroupName: (String) -> String? = { groupIdHex ->
        nameVersion.let { chatRepository.groupName(groupIdHex)?.name }
    }

    when (val current = state) {
        AuthState.Loading -> InsetRoot { LoadingScreen() }
        is AuthState.Anon -> InsetRoot {
            AnonRoot(
                repository = authRepository,
                cachedUsername = current.cachedUsername,
            )
        }
        // ADR-0017 fresh-device fail-safe: auth succeeded but no
        // cached AccountIdentity (phrase) seed on this device — prompt
        // for the recovery phrase (HARD-BLOCK verified) before opening
        // a full session. NEVER derive the identity from the export key
        // or mint fresh here.
        is AuthState.NeedsRecoveryPhrase -> InsetRoot {
            RestoreIdentityRoot(
                repository = authRepository,
                account = current.account,
            )
        }
        // ADR-0017 lost-everything fallback: the user minted a brand-new
        // identity (already published, no proof); show the new recovery
        // phrase for them to save before the session lands.
        is AuthState.SaveFreshPhrase -> InsetRoot {
            SaveFreshPhraseRoot(repository = authRepository)
        }
        is AuthState.Session -> {
            // Sign-up lands the session the moment the account exists; the
            // recovery phrase it produced is shown (and confirmed) here,
            // before the app, from the in-memory gate.
            val parkedPhrase by me.lakko.app.ui.auth.SignupPhraseGate.phrase.collectAsState()
            val phraseToSave = parkedPhrase
            if (phraseToSave != null) {
                InsetRoot {
                    me.lakko.app.ui.auth.RecoveryPhraseOnboarding(
                        phrase = phraseToSave,
                        onDone = { me.lakko.app.ui.auth.SignupPhraseGate.clear() },
                    )
                }
                return
            }
            // ADR-0017 defence-in-depth — surface the identity-changed
            // security alert above the session content.
            val identityAlert by authRepository.identityAlert.collectAsState()
            // ADR-0038 S4 — the "update available" notice, mounted BELOW the
            // security banner (severity ordering: security alert first, the
            // informational update notice second — UX §3). Shown only when the
            // verified manifest resolves to an actionable update.
            val updateDecision by releaseChecker.decision.collectAsState()
            val context = androidx.compose.ui.platform.LocalContext.current
            val shownUpdate = updateDecision as? UpdateBannerDecision.Shown
            // docs/ux/calls.md section 4.5 — the live call, put away. Resolved
            // here rather than inside `SessionRoot` so that "no bar" is a `null`
            // slot and the top strip reserves nothing when there is no call.
            // The name resolvers are the ones the call screens use, so the bar
            // says exactly what the screen it came from says.
            val callBarModel = me.lakko.app.ui.call.rememberCollapsedCallBar(
                oneToOne = callHost,
                group = me.lakko.app.calls.AppCallHost.group,
                link = me.lakko.app.calls.AppCallHost.link,
                nameForOneToOne = resolveOneToOneName,
                groupNameFor = resolveGroupName,
            )
            SessionRoot(
                authRepository = authRepository,
                contactsRepository = contactsRepository,
                chatRepository = chatRepository,
                callHost = callHost,
                deviceGroupSync = deviceGroupSync,
                inviteIntake = inviteIntake,
                pushIntake = pushIntake,
                antiCensorship = antiCensorship,
                account = current.account,
                banners = if (identityAlert == null && shownUpdate == null) {
                    null
                } else {
                    {
                        if (identityAlert != null) {
                            me.lakko.app.ui.auth.IdentitySecurityBanner(
                                onDismiss = { authRepository.dismissIdentityAlert() },
                            )
                        }
                        shownUpdate?.let { shown ->
                            UpdateAvailableBanner(
                                showUpdateButton = shown.showUpdateButton,
                                onUpdate = { releaseChecker.openStore(context) },
                                onDismiss = { releaseChecker.dismiss() },
                            )
                        }
                    }
                },
                callBar = callBarModel?.let { bar ->
                    {
                        me.lakko.app.ui.call.CollapsedCallBar(
                            model = bar,
                            onExpand = { me.lakko.app.calls.AppCallHost.expandCurrentCall() },
                            onToggleMute = { on ->
                                me.lakko.app.calls.AppCallHost.setCurrentCallMicrophoneEnabled(on)
                            },
                        )
                    }
                },
            )
        }
    }

    if (state is AuthState.Session) {
        // The call surfaces, above every session screen — including the `/c/`
        // landing, which is drawn first. Rendered from the process-wide host,
        // so a destination change never hides or ends a call.
        androidx.compose.foundation.layout.Box(modifier = Modifier.fillMaxSize()) {
            if (callSlug != null) {
                androidx.activity.compose.BackHandler { inviteIntake.consumeCallJoin() }
                me.lakko.app.ui.call.JoinCallRoot(
                    authRepository = authRepository,
                    slug = callSlug,
                    applicationContext = appContext,
                    onExit = { inviteIntake.consumeCallJoin() },
                    host = me.lakko.app.calls.AppCallHost.link,
                    endCurrentCall = { me.lakko.app.calls.AppCallHost.endCurrentCall() },
                )
            }
            // The 1x1 call's ringing banner and in-call screen.
            OneToOneCallOverlay(host = callHost, nameForGroup = resolveOneToOneName)
            // A call-link call that is connecting, waiting to be admitted, or
            // just ended, on screens other than its landing.
            LinkCallNoticeBar(
                host = me.lakko.app.calls.AppCallHost.link,
                onOpen = { slug -> inviteIntake.publishCallJoin(slug) },
            )
        }
        // ADR-0049 section 2 — "confirm on another device": this signed-in
        // device answers a sign-in request while it is in the foreground. A
        // device removed from the account (docs/ux/second-device.md section
        // 3.1) no longer belongs to it and confirms nothing.
        val removedFromAccount by deviceGroupSync.deviceRemoved.collectAsStateWithLifecycle()
        if (!removedFromAccount) {
            me.lakko.app.ui.devices.DeviceLinkRequestHost(
                authRepository = authRepository,
                chatRepository = chatRepository,
            )
        }
        // The group call's and the call-link call's in-call screens.
        me.lakko.app.ui.call.GroupCallOverlay(
            host = me.lakko.app.calls.AppCallHost.group,
            // ADR-0051 — the group name every member holds, so a group call is
            // titled by its group instead of by a participant's label.
            groupNameFor = resolveGroupName,
        )
        // docs/ux/call-links.md section 3 — the `Invite` control reads the
        // session at the moment it is pressed; it is never held for the call.
        me.lakko.app.ui.call.LinkCallOverlay(
            host = me.lakko.app.calls.AppCallHost.link,
            // The landing covers the collapsed bar's strip, so putting the call
            // away leaves it — see `LinkCallOverlay`'s parameter.
            onLeaveLanding = { inviteIntake.consumeCallJoin() },
            identity = {
                authRepository.identitySecrets.value?.let { secrets ->
                    me.lakko.app.calls.CallInviteIdentity(
                        accountIdentitySeed = secrets.accountIdentitySeed,
                        accountId = secrets.accountId,
                        sessionToken = secrets.sessionToken,
                    )
                }
            },
        )
    }

    // ADR-0037 (G-11) — rollback-gate overlays, shown OVER any auth state (the
    // recover flow can trigger them while still on the Anon screen). Both are
    // window-hosted dialogs, so they float above whatever the router rendered.
    val staleOverride by authRepository.staleOverridePrompt.collectAsState()
    staleOverride?.let { prompt ->
        me.lakko.app.ui.auth.StaleBackupOverrideDialog(
            reason = prompt.reason,
            onConfirm = { authRepository.resolveStaleOverride(true) },
            onCancel = { authRepository.resolveStaleOverride(false) },
        )
    }
    val rollbackAlert by authRepository.rollbackAlert.collectAsState()
    rollbackAlert?.let { alert ->
        me.lakko.app.ui.auth.BackupRollbackAlertDialog(
            reason = alert.reason,
            onDismiss = { authRepository.dismissRollbackAlert() },
        )
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
 * Session-state shell — docs/ux/client-redesign.md section 3.
 *
 * One `NavHost` with five tab-level start destinations (Chats, Groups,
 * Channels, Contacts, Settings) behind a Material 3 `NavigationBar`, plus the
 * pushed destinations (a chat, the settings screens, the new chat / new group
 * pickers, the `/i/` and `/g/` landings). System back and predictive back are
 * the navigation controller's. The app opens on Chats; there is no home screen.
 *
 * Also owns the app-level receive loops (every group long-polled while the app
 * is in the foreground, whatever screen is showing) and the ChatRepository
 * bootstrap.
 */
@Composable
private fun SessionRoot(
    authRepository: AuthRepository,
    contactsRepository: ContactsRepository,
    chatRepository: ChatRepository,
    callHost: me.lakko.app.calls.OneToOneCallHost<me.lakko.app.calls.CallSession>,
    deviceGroupSync: me.lakko.app.devices.sync.DeviceGroupSyncRepository,
    inviteIntake: InviteIntake,
    pushIntake: PushIntake,
    antiCensorship: AntiCensorshipManager,
    account: SessionAccount,
    /** The security / update banners, drawn above the tab content. */
    banners: (@Composable () -> Unit)?,
    /**
     * `docs/ux/calls.md` section 4.5 — the collapsed call bar, drawn across the
     * top of the app above every screen and above [banners].
     *
     * `null` when there is no bar, which is what keeps the status-bar inset from
     * being reserved for nothing. It sits ABOVE the notices rather than under
     * them because it is a live control and not a notice: a person reaching for
     * Mute must not have it move because an update notice appeared underneath.
     */
    callBar: (@Composable () -> Unit)?,
) {
    // Drive ChatRepository.bootstrap whenever a fresh
    // identity-secrets value lands. The first non-null emission
    // happens immediately after signup / login / recover — or, on a
    // cold start, straight from AuthRepository's silent session
    // restore; the bootstrap call is idempotent for the same account
    // id, so repeated emissions cost nothing. A non-idempotent
    // bootstrap ends by republishing this device's KeyPackage from the
    // long-lived identity (ChatRepository.requestKeyPackageRepublish).
    val identitySecrets by authRepository.identitySecrets.collectAsStateWithLifecycle()
    LaunchedEffect(identitySecrets) {
        val secrets = identitySecrets ?: return@LaunchedEffect
        // Cold start — everything in this effect used to run synchronously on
        // the main thread: the MLS snapshot restore, one `loadGroup` per chat,
        // the message-log parse, the DeviceGroup restore. The shell now draws
        // first and these populate it; `bootstrap` hops onto the repository's
        // own dispatcher, the two stores below are hopped here.
        // The accepted contacts, the stranger-inbox cursor and the pending
        // list are per account: load this account's now, so the Contacts tab
        // shows its contacts and saved requests before its first poll, which
        // then resumes from the saved cursor. Idempotent when the repository
        // already bound this account at construction.
        contactsRepository.bindAccount(secrets.accountId)
        chatRepository.bootstrap(secrets)
        // ADR-0048 — make sure this account's own profile exists and states
        // its current username (normalised; `null` for a guest). Republished
        // into every group only when that actually changed.
        val canonicalUsername = account.username
            .takeIf { it.isNotBlank() }
            ?.let { typed ->
                (me.lakko.app.profile.checkUsername(typed) as? me.lakko.app.profile.UsernameCheck.Valid)
                    ?.normalized
            }
        val (_, profileChanged) = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            me.lakko.app.profile.OwnProfileStore.ensureUsername(
                context = chatRepository.context,
                accountId = secrets.accountId,
                username = canonicalUsername,
            )
        }
        if (profileChanged) chatRepository.publishOwnProfileToAllGroups()
        // Phase 2a3e2b — bootstrap the cross-device sync
        // runtime alongside the chat bootstrap so the poll
        // loop starts at the same moment chat surfaces become
        // usable. Idempotent for the same account id.
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
            deviceGroupSync.bootstrap(secrets)
        }
    }

    // docs/ux/calls.md section 2.1 — the phone must ring with the app closed,
    // and on Android 13+ that needs POST_NOTIFICATIONS, a runtime permission
    // the manifest declaration grants nothing towards. It was never requested,
    // so the permission stayed denied and IncomingCallRinger's post was
    // refused: the phone silently never rang.
    //
    // The ask is here, not in PushSubscriber, for a structural reason: the
    // subscribe runs on an application-scoped coroutine holding only an
    // application context, and a permission dialog needs an Activity. This is
    // also the in-context moment — the session has just landed, the app is
    // about to subscribe to push, and being reachable is what the person just
    // signed in for. It is deliberately not at app launch, where there is
    // nothing yet to be notified about.
    val notificationAskContext = LocalContext.current
    val notificationPermissionLauncher = rememberLauncherForActivityResult(
        contract = ActivityResultContracts.RequestPermission(),
    ) {
        // Nothing to do with the answer here. Every surface that cares reads
        // the live permission state when it is shown (Settings), and the
        // ringer already fails soft on a refusal; storing a copy of the answer
        // would only be a second, staler truth. Recording that we ASKED is a
        // different fact, and it is written before the prompt is raised.
    }
    LaunchedEffect(identitySecrets) {
        if (identitySecrets == null) return@LaunchedEffect
        val granted = Build.VERSION.SDK_INT < me.lakko.app.push.NOTIFICATION_PERMISSION_SDK ||
            ContextCompat.checkSelfPermission(
                notificationAskContext,
                Manifest.permission.POST_NOTIFICATIONS,
            ) == PackageManager.PERMISSION_GRANTED
        // Read the durable "already asked" flag and, when we are going to ask,
        // write it BEFORE the dialog exists — a prompt can take the process
        // down, and the answer callback of a dead process never runs, so
        // writing afterwards would re-prompt on every launch and spend the
        // platform's two-dismissal budget without the user choosing anything.
        val ask = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            val decision = me.lakko.app.push.notificationPermissionAsk(
                sdkInt = Build.VERSION.SDK_INT,
                granted = granted,
                askedBefore = me.lakko.app.push.NotificationPermissionStore
                    .askedBefore(notificationAskContext),
            )
            if (decision == me.lakko.app.push.NotificationPermissionAsk.Ask) {
                me.lakko.app.push.NotificationPermissionStore.markAsked(notificationAskContext)
            }
            decision
        }
        if (ask == me.lakko.app.push.NotificationPermissionAsk.Ask) {
            notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    val navController = rememberNavController()
    // Shared by every Channels destination (the list, a channel, its subscribe
    // route, Manage, New channel, Join as co-publisher): one local channels
    // state (Keystore-backed key persistence) and one REST client.
    val channelsState = rememberChannelsUiState()
    // ADR-0060 — people search (who-can-find-me.md section 6.1).
    val peopleSearchContext = LocalContext.current.applicationContext
    val peopleSearch = remember(contactsRepository) {
        me.lakko.app.search.PeopleSearchService(peopleSearchContext, contactsRepository)
    }
    val channelsClient = remember { me.lakko.app.channels.ChannelsClient() }
    // ADR-0060 §1 — the channels this account is in: the setting decides
    // their `channel` keys on the account mailbox.
    LaunchedEffect(channelsState) {
        channelsState.channels.collect { list ->
            me.lakko.app.reach.ReachManager.peek()?.setChannels(list.map { it.id }.toSet())
        }
    }
    // Bind the channels state to the account once the identity secrets land:
    // load this account's persisted channel list + subscriber seeds, restore
    // the publisher MLS provider snapshot, and re-open the publisher keys of
    // every channel this device has a signing seed for — so a creator can
    // post again and a subscriber can read again after a relaunch. Idempotent
    // per account, so a token-refresh re-emission costs nothing; the MLS work
    // runs off the main thread inside `bind`.
    LaunchedEffect(identitySecrets, channelsState) {
        val secrets = identitySecrets ?: return@LaunchedEffect
        try {
            channelsState.bind(
                accountIdB64 = me.lakko.app.auth.base64Encode(secrets.accountId),
                accountIdentitySeed = secrets.accountIdentitySeed,
            )
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (e: Throwable) {
            android.util.Log.w("SessionRoot", "channels state bind failed", e)
        }
    }

    // ADR-0059 section 6 — the channels this device can share in a group:
    // subscribed, listed by their own metadata, with a handle. `null` while the
    // channel list is not bound to the signed-in account, so a list read
    // before loading never withdraws what was shared. Any change of the list,
    // the subscriptions or a channel's metadata re-sends (debounced) to every
    // group the switch is on in whose last list differs.
    LaunchedEffect(channelsState, chatRepository) {
        chatRepository.sharedChannels.shareable = shareable@{
            val acct = chatRepository.currentAccountIdB64() ?: return@shareable null
            if (channelsState.boundAccount != acct) return@shareable null
            val subscribed = channelsState.subscriberKeysByChannel.value.keys
            channelsState.channels.value.map { c ->
                me.lakko.app.discover.ShareableChannel(
                    channelIdHex = c.id,
                    subscribed = c.id in subscribed,
                    listed = c.sharedListed,
                    handle = c.handle.takeIf { it.isNotEmpty() },
                    name = c.sharedName.ifBlank { c.localName },
                )
            }
        }
        kotlinx.coroutines.flow.combine(
            channelsState.channels,
            channelsState.subscriberKeysByChannel,
        ) { list, keys -> list to keys.keys }
            .collect { chatRepository.sharedChannels.followedSetChanged() }
    }

    // The app-level receive loop: every group long-polled while the app is in
    // the foreground, WHATEVER screen is showing. Call signaling (the 1x1 call
    // host), group-call records and the chat list's unread counts all come from
    // what a poll applies, so polling only on the chat list meant an incoming
    // call rang nowhere else. `STARTED` is the foreground gate: stopped, the
    // loop ends and the host holds an offer instead of ringing it; started
    // again, the loop fetches at once and a still-fresh offer rings.
    //
    // Groups an open chat screen is long-polling itself are left to that
    // screen, so no group is polled twice (the loser of the per-group race
    // would return at once and spin) and none is left unpolled.
    //
    // The exclusion is READ PER PASS rather than collected, and that is a fix
    // rather than a simplification. It used to drive `collectLatest`, so every
    // chat the user opened or left CANCELLED the sweep in flight and started it
    // again from the first wave. A set larger than the concurrency bound polls
    // in waves, so a user moving between chats could keep restarting the sweep
    // and the later waves would never be reached at all — and an incoming call
    // for one of those groups was never read. Reading the current value at the
    // top of each pass keeps ownership exactly as it was (an open chat's group
    // is still skipped from the next pass on) without ever discarding a sweep
    // that is part way through.
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    LaunchedEffect(lifecycleOwner, chatRepository, callHost) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            callHost.setForeground(true)
            try {
                while (true) {
                    val startedAt = android.os.SystemClock.elapsedRealtime()
                    // docs/ux/feed.md, "refresh failed (network)": every sweep
                    // reports what it tried and what answered into the feed's
                    // app-wide failure state, for the account it ran for. A
                    // sweep that fails as a whole counts as not answered.
                    val sweepAccount = chatRepository.currentAccountIdB64()
                    try {
                        chatRepository.feedRefresh.reporting(sweepAccount, me.lakko.app.feed.CheckKind.GROUPS) {
                            chatRepository.pollAllGroups(
                                waitSecs = APP_LONG_POLL_SECS,
                                exclude = chatRepository.screenPolledGroups.value,
                            )
                        }
                    } catch (cancel: kotlinx.coroutines.CancellationException) {
                        throw cancel
                    } catch (_: Throwable) {
                        kotlinx.coroutines.delay(APP_POLL_RETRY_BACKOFF_MS)
                    }
                    // A poll that returns at once (not bootstrapped yet, or a
                    // group momentarily in flight elsewhere) must not become
                    // a busy loop.
                    val elapsed = android.os.SystemClock.elapsedRealtime() - startedAt
                    if (elapsed < APP_POLL_MIN_ITERATION_MS) {
                        kotlinx.coroutines.delay(APP_POLL_MIN_ITERATION_MS - elapsed)
                    }
                }
            } finally {
                callHost.setForeground(false)
            }
        }
    }

    // ADR-0060 §6 (amending ADR-0050 §2.7) — Welcomes go to the ACCOUNT
    // mailbox now, read by the loop below; this device's own mailbox stays
    // claimed and closed with no reach keys, and is no longer polled.

    // The ACCOUNT mailbox, for the whole session rather than one screen.
    //
    // This loop used to live inside the Contacts screen, so nothing was read
    // from the account mailbox while the user sat on Chats, Groups, Channels
    // or Settings — and three things ride it: a contact request, a contact
    // acknowledgement, and the Welcome a peer sends from `startGroupWith` when
    // they accept, which is the only thing that makes their 1x1 chat appear.
    // A person added to a chat would not see it until they happened to open
    // Contacts. iOS moved the same loop to its session root on 2026-09-17 and
    // Android was not carried along; found on 2026-09-22 while timing a
    // contact request that had in fact been waiting for the screen.
    //
    // It is deliberately the ONLY continuous reader of this mailbox: the
    // cursor is one value on the repository, so a second long poll racing this
    // one could advance it past events the other is still handling. The
    // Contacts screen keeps its one-shot refresh (a manual pull, `waitSecs 0`).
    // Keep the encrypted backup current. Until this ran, the blob was written
    // exactly twice in an account's life - an empty baseline at signup and one
    // at recover - so a restore on a new device landed the person back at
    // registration day. Inert while nothing has changed; see
    // `backup/AutoBackup.kt` for what counts as a change.
    val backupContext = androidx.compose.ui.platform.LocalContext.current.applicationContext
    LaunchedEffect(lifecycleOwner, authRepository, chatRepository) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            val driver = me.lakko.app.backup.AutoBackupDriver(
                applicationContext = backupContext,
                scope = this,
                inputs = me.lakko.app.backup.AutoBackupDriver.Inputs(
                    accountId = { authRepository.identitySecrets.value?.accountId },
                    accountIdB64 = {
                        authRepository.identitySecrets.value?.let {
                            me.lakko.app.auth.base64Encode(it.accountId)
                        }
                    },
                    sessionToken = { authRepository.identitySecrets.value?.sessionToken },
                    accountIdentitySeed = {
                        authRepository.identitySecrets.value?.accountIdentitySeed
                    },
                    // `null` until the chat layer has bootstrapped, so an empty
                    // list during startup is never mistaken for "left every
                    // group".
                    knownGroupIds = {
                        chatRepository.state.value.let { st ->
                            if (st.bootstrapped) st.threads.map { it.groupIdHex } else null
                        }
                    },
                    snapshot = { chatRepository.currentStorageSnapshot() },
                    // ADR-0058 section 5.2 — the folded synced state (folders,
                    // pins, mutes, archive, read cursors, drafts, order).
                    syncedState = { deviceGroupSync.currentSyncedStateBytes() },
                ),
            )
            driver.start()
            try {
                kotlinx.coroutines.awaitCancellation()
            } finally {
                driver.stop()
            }
        }
    }

    LaunchedEffect(lifecycleOwner, contactsRepository) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            while (true) {
                val startedAt = android.os.SystemClock.elapsedRealtime()
                try {
                    contactsRepository.refreshInbox(waitSecs = APP_LONG_POLL_SECS)
                } catch (cancel: kotlinx.coroutines.CancellationException) {
                    throw cancel
                } catch (_: Throwable) {
                    kotlinx.coroutines.delay(APP_POLL_RETRY_BACKOFF_MS)
                }
                // A poll that returns at once (no session yet, a refused
                // request) must not become a busy loop.
                val elapsed = android.os.SystemClock.elapsedRealtime() - startedAt
                if (elapsed < APP_POLL_MIN_ITERATION_MS) {
                    kotlinx.coroutines.delay(APP_POLL_MIN_ITERATION_MS - elapsed)
                }
            }
        }
    }

    // The call lane: the group of a hosted group call keeps being long-polled
    // while the app is in the background, so a commit that lands during the
    // call is applied — and the call key rotated (G-18 / ADR-0029) — even with
    // the chat closed. In the foreground the loop above polls it; a chat screen
    // that is open polls it itself (in the background too), so the lane stands
    // aside for both and a group is never long-polled twice.
    LaunchedEffect(lifecycleOwner, chatRepository) {
        kotlinx.coroutines.flow.combine(
            me.lakko.app.calls.AppCallHost.group.slot
                .map { it?.groupIdHex }
                .distinctUntilChanged(),
            lifecycleOwner.lifecycle.currentStateFlow
                .map { it.isAtLeast(androidx.lifecycle.Lifecycle.State.STARTED) }
                .distinctUntilChanged(),
            chatRepository.screenPolledGroups,
        ) { callGroup, foreground, screenPolled ->
            callGroup?.takeIf { !foreground && it !in screenPolled }
        }
            .distinctUntilChanged()
            .collectLatest { laneGroup ->
                if (laneGroup == null) return@collectLatest
                while (true) {
                    val startedAt = android.os.SystemClock.elapsedRealtime()
                    try {
                        chatRepository.pollGroup(laneGroup, waitSecs = APP_LONG_POLL_SECS)
                    } catch (cancel: kotlinx.coroutines.CancellationException) {
                        throw cancel
                    } catch (_: Throwable) {
                        kotlinx.coroutines.delay(APP_POLL_RETRY_BACKOFF_MS)
                    }
                    val elapsed = android.os.SystemClock.elapsedRealtime() - startedAt
                    if (elapsed < APP_POLL_MIN_ITERATION_MS) {
                        kotlinx.coroutines.delay(APP_POLL_MIN_ITERATION_MS - elapsed)
                    }
                }
            }
    }

    // A tapped `https://lakko.me/i/<slug>` invite link (cold start: published
    // before this composable mounted; warm: via onNewIntent) pushes the
    // landing over the current tab, then the slot is consumed so a later slug
    // lands cleanly.
    val pendingInviteSlug by inviteIntake.pendingInviteSlug.collectAsStateWithLifecycle()
    LaunchedEffect(pendingInviteSlug) {
        val slug = pendingInviteSlug ?: return@LaunchedEffect
        navController.navigate(Routes.invite(slug)) { launchSingleTop = true }
        inviteIntake.consume()
    }

    // ADR-0061 section 4 — channels join the foreground poll: while the app is
    // started, the posts of every readable channel are filed into the
    // channel-post log the feed ranks. At once on start (and on every return
    // to the foreground), then every minute, and at once after this device
    // publishes a post.
    val channelPostSync = remember(channelsState, channelsClient) {
        me.lakko.app.feed.ChannelPostSync(backupContext, channelsState, channelsClient)
    }
    val channelsBound by channelsState.channels.collectAsStateWithLifecycle()
    LaunchedEffect(lifecycleOwner, channelPostSync, identitySecrets, channelsBound.isNotEmpty()) {
        if (identitySecrets == null) return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            while (true) {
                // docs/ux/feed.md, "refresh failed (network)": each pass reports
                // into the feed's app-wide failure state; a skipped pass says
                // nothing.
                val syncAccount = chatRepository.currentAccountIdB64()
                chatRepository.feedRefresh.reporting(syncAccount, me.lakko.app.feed.CheckKind.CHANNELS) {
                    channelPostSync.syncOnce()
                }
                // The interval, or sooner when this device just published.
                me.lakko.app.feed.awaitChannelPostSyncTick()
            }
        }
    }

    // ADR-0059 section 4 — letting new subscribers in: while the app is
    // started, every channel this device holds publisher keys for (owned or
    // co-published) has the current seed sealed to each subscriber not yet
    // let in, every 15 s whatever screen is showing. The channel screens poll
    // the same shared state faster while open. Restarts when the set of
    // publisher channels changes (keys re-open after bind); stops on sign-out.
    val subscriberAdmission = remember(channelsState, channelsClient) {
        me.lakko.app.channels.SubscriberAdmission.forChannels(channelsState, channelsClient)
    }
    val publisherChannels by channelsState.keysByChannel.collectAsStateWithLifecycle()
    LaunchedEffect(lifecycleOwner, subscriberAdmission, identitySecrets, publisherChannels.keys) {
        if (identitySecrets == null || publisherChannels.isEmpty()) return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            while (true) {
                if (channelsState.boundAccount != null) {
                    subscriberAdmission.admitAll(channelsState.keysByChannel.value.keys.toList())
                }
                kotlinx.coroutines.delay(me.lakko.app.channels.SubscriberAdmission.APP_INTERVAL_MS)
            }
        }
    }

    // A tapped shared-feed link pushes the import screen.
    val pendingFeedImport by me.lakko.app.ui.feed.FeedImportIntake.pending.collectAsStateWithLifecycle()
    val pendingFeedFile by me.lakko.app.ui.feed.FeedImportIntake.pendingFile.collectAsStateWithLifecycle()
    LaunchedEffect(pendingFeedImport, pendingFeedFile) {
        if (pendingFeedImport == null && pendingFeedFile == null) return@LaunchedEffect
        navController.navigate(Routes.FEED_IMPORT) { launchSingleTop = true }
    }

    // ADR-0028 (G-3) — a tapped `/g/<slug>` group join-ticket link pushes the
    // group-join landing the same way.
    val pendingGroupJoin by inviteIntake.pendingGroupJoin.collectAsStateWithLifecycle()
    LaunchedEffect(pendingGroupJoin) {
        val link = pendingGroupJoin ?: return@LaunchedEffect
        navController.navigate(Routes.groupJoin(link)) { launchSingleTop = true }
        inviteIntake.consumeGroupJoin()
    }

    // A notification tap (Phase 1.20d2) opens the matching chat when the group
    // is known locally; a stale wake lands on the list its kind belongs to.
    val pendingWake by pushIntake.pendingWake.collectAsStateWithLifecycle()
    val chatState by chatRepository.state.collectAsStateWithLifecycle()
    LaunchedEffect(pendingWake, chatState.bootstrapped) {
        val wake = pendingWake ?: return@LaunchedEffect
        if (!chatState.bootstrapped) return@LaunchedEffect
        val knownThread = chatState.threads.firstOrNull {
            it.groupIdHex.equals(wake.groupHex, ignoreCase = true)
        }
        if (knownThread != null) {
            navController.navigate(Routes.chat(knownThread.groupIdHex)) { launchSingleTop = true }
        } else {
            navController.navigateToTab(Routes.CHATS)
        }
        pushIntake.consume()
    }

    val appContext = LocalContext.current.applicationContext
    val profileDirectory = remember(chatRepository) {
        me.lakko.app.profile.ProfileDirectory.get(chatRepository.context)
    }
    val backStackEntry by navController.currentBackStackEntryAsState()
    val currentRoute = backStackEntry?.destination?.route
    val showTabs = currentRoute in Routes.TOP_LEVEL

    val logout = {
        // ADR-0042, amended 2026-09-30 — the push wake binding and the
        // delivered notifications go FIRST, before anything else sign-out
        // does (AuthRepository.logout repeats it, idempotently, for the
        // callers that reach it directly).
        me.lakko.app.push.PushSignOut.clearFirst(appContext)
        me.lakko.app.calls.AppCallHost.reset()
        contactsRepository.clear()
        // In-memory only, like chat.clear(): the persisted channel log and
        // the publisher provider snapshot stay for the next sign-in.
        channelsState.unbind()
        subscriberAdmission.clear()
        chatRepository.clear()
        authRepository.logout()
    }
    // Forget device and Delete account (PasskeysViewModel, after the server
    // erasure) both run THIS, and the removed-device screen's one button too.
    // Order: the push binding and delivered notifications first; then every
    // runtime that could write a store back is stopped (calls, contacts,
    // channels, chat, device sync) — chat first taking this device's
    // KeyPackage out of the directory while the session is still readable;
    // last, auth.forgetDevice() runs the one on-device wipe
    // (AccountLocalWipe: every per-account store) and ends the session.
    val forgetDevice = {
        // ADR-0042, amended 2026-09-30 — first, as in logout above.
        me.lakko.app.push.PushSignOut.clearFirst(appContext)
        me.lakko.app.calls.AppCallHost.reset()
        contactsRepository.clear()
        channelsState.unbind()
        subscriberAdmission.clear()
        chatRepository.forgetDevice()
        deviceGroupSync.clear()
        authRepository.forgetDevice()
    }

    // docs/ux/second-device.md section 3.1 — another device removed this one.
    // The removed screen replaces the whole app, tab bar, call bar and notices
    // included; its one button is "Forget this device" above.
    val deviceRemoved by deviceGroupSync.deviceRemoved.collectAsStateWithLifecycle()
    if (deviceRemoved) {
        InsetRoot {
            me.lakko.app.ui.devices.RemovedDeviceScreen(onSignOut = forgetDevice)
        }
        return
    }

    Scaffold(
        // Each screen pads by its own Scaffold; the shell contributes only the
        // tab bar (whose height is consumed below so a screen's Scaffold does
        // not add the navigation-bar inset a second time).
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            if (showTabs) {
                NavigationBar {
                    for (tab in Tabs) {
                        val selected = currentRoute == tab.route
                        NavigationBarItem(
                            selected = selected,
                            onClick = { navController.navigateToTab(tab.route) },
                            icon = {
                                Icon(
                                    imageVector = if (selected) tab.selectedIcon else tab.icon,
                                    contentDescription = tab.label,
                                )
                            },
                            label = { Text(tab.label) },
                        )
                    }
                }
            }
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .consumeWindowInsets(padding),
        ) {
            // One strip at the top of the app: the collapsed call bar first,
            // then the notices. It carries the status-bar inset and the NavHost
            // consumes it, so nothing here covers the status bar and no screen
            // pads for it twice.
            val topStrip = callBar != null || banners != null
            if (topStrip) {
                Column(modifier = Modifier.windowInsetsPadding(WindowInsets.statusBars)) {
                    callBar?.invoke()
                    banners?.invoke()
                }
            }
            NavHost(
                navController = navController,
                startDestination = Routes.CHATS,
                modifier = if (topStrip) {
                    Modifier.consumeWindowInsets(WindowInsets.statusBars)
                } else {
                    Modifier
                },
            ) {
                // docs/ux/navigation.md section 2 — ONE list: one-to-one
                // chats, groups and channels, ordered by the shared rule in
                // `crates/lakko-ui-rules`. A tap is routed by the ROW's kind,
                // which is the only thing that kind is carried for.
                composable(Routes.CHATS) {
                    ChatListScreen(
                        repository = chatRepository,
                        onOpenThread = { hex -> navController.navigate(Routes.chat(hex)) },
                        onOpenChannel = { id -> navController.navigate(Routes.channel(id)) },
                        onNewChat = { navController.navigate(Routes.NEW_CHAT) },
                        onCreateGroup = { navController.navigate(Routes.NEW_GROUP) },
                        onCreateChannel = { navController.navigate(Routes.CHANNEL_NEW) },
                        onSubscribeToChannel = { id, handle ->
                            navController.navigate(Routes.channelSubscribe(id, handle))
                        },
                        // Section 4.3 — a found person goes to Contacts, which
                        // owns the request flow because it asks for an
                        // introduction the search field has no room for.
                        onFoundPerson = { username ->
                            me.lakko.app.contacts.PendingContactLookup.publish(username)
                            navController.navigateToTab(Routes.CONTACTS)
                        },
                        channels = channelsState,
                        deviceGroupSync = deviceGroupSync,
                        // docs/ux/chat-folders.md — the editor and Settings >
                        // Chat folders are pushed destinations.
                        onEditFolder = { id -> navController.navigate(Routes.folderEdit(id, null)) },
                        onNewFolder = { row ->
                            navController.navigate(
                                Routes.folderEdit(null, row?.let { me.lakko.app.chat.refOf(it) }),
                            )
                        },
                        onOpenFolderSettings = { navController.navigate(Routes.FOLDERS) },
                        // docs/ux/calls.md section 2.1.1 — a muted
                        // conversation's call never takes the screen over, so
                        // the list offers it on that chat's own row instead.
                        callHost = me.lakko.app.calls.AppCallHost.instance,
                        // docs/ux/who-can-find-me.md section 6.1 — people in
                        // the search field, local and network.
                        peopleSearch = peopleSearch,
                        onOpenContact = { keyB64 ->
                            val key = runCatching { java.util.Base64.getDecoder().decode(keyB64) }.getOrNull()
                            val chat = key?.let { k ->
                                contactsRepository.state.value.accepted
                                    .firstOrNull { it.request.fromPubkey.contentEquals(k) }?.groupIdHex
                            }
                            if (chat != null) {
                                navController.navigate(Routes.chat(chat))
                            } else {
                                navController.navigateToTab(Routes.CONTACTS)
                            }
                        },
                    )
                }
                // `docs/ux/feed.md` — the ranked feed of group messages and
                // channel posts (ADR-0061).
                composable(Routes.FEED) {
                    me.lakko.app.ui.feed.FeedScreen(
                        chatRepository = chatRepository,
                        channels = channelsState,
                        deviceGroupSync = deviceGroupSync,
                        contactsRepository = contactsRepository,
                        ownUsername = account.username
                            .takeIf { it.isNotBlank() }
                            ?.let { typed ->
                                (me.lakko.app.profile.checkUsername(typed) as?
                                    me.lakko.app.profile.UsernameCheck.Valid)?.normalized
                            },
                        onOpenGroupMessage = { hex, messageId ->
                            me.lakko.app.chat.PendingMessageJump.publish(hex, messageId)
                            navController.navigate(Routes.chat(hex))
                        },
                        onOpenChannel = { id -> navController.navigate(Routes.channel(id)) },
                        onGoToChats = { navController.navigateToTab(Routes.CHATS) },
                        channelsClient = channelsClient,
                        // ADR-0059 — a card whose handle was verified opens the
                        // ordinary subscribe route under that handle.
                        onSubscribeChannel = { id, handle ->
                            navController.navigate(Routes.channelSubscribe(id, handle))
                        },
                        // UX spec section 5 — the item menu's Forward opens the
                        // chat's own picker.
                        onForward = { navController.navigate(Routes.FORWARD) },
                    )
                }
                composable(Routes.FEED_IMPORT) {
                    val fragment = remember {
                        me.lakko.app.ui.feed.FeedImportIntake.pending.value.also {
                            me.lakko.app.ui.feed.FeedImportIntake.consume()
                        }
                    }
                    val fileUri = remember {
                        me.lakko.app.ui.feed.FeedImportIntake.pendingFile.value.also {
                            me.lakko.app.ui.feed.FeedImportIntake.consumeFile()
                        }
                    }
                    me.lakko.app.ui.feed.FeedImportScreen(
                        accountIdB64 = chatRepository.currentAccountIdB64(),
                        fragment = fragment,
                        fileUri = fileUri,
                        onDone = {
                            navController.popBackStack()
                            navController.navigateToTab(Routes.FEED)
                        },
                    )
                }
                // Channels (ADR-0010) — docs/ux/client-redesign.md section 5.4.
                // The list is the tab; the channel view, its subscribe route,
                // the owner's Manage screen, New channel and Join as
                // co-publisher are pushed destinations of this host. The
                // AccountIdentity (phrase) seed (ADR-0017) comes from the live
                // IdentitySecrets so a publisher can rebuild its group.
                composable(Routes.CHANNEL_NEW) {
                    val secrets = identitySecrets
                    if (secrets != null) {
                        NewChannelScreen(
                            state = channelsState,
                            client = channelsClient,
                            sessionToken = secrets.sessionToken,
                            onBack = { navController.popBackStack() },
                            onCreated = { id ->
                                // The Channels tab is gone, so a created
                                // channel pops back to the one list every
                                // conversation now lives in.
                                navController.navigate(Routes.channel(id)) {
                                    popUpTo(Routes.CHANNEL_NEW) { inclusive = true }
                                }
                            },
                            // ADR-0028 OQ3 — the co-publisher join is reached
                            // from here because the create menu is fixed at
                            // three items and this is the same moment.
                            onJoinAsCoPublisher = {
                                navController.navigate(Routes.CHANNEL_JOIN)
                            },
                        )
                    } else {
                        LoadingScreen()
                    }
                }
                composable(Routes.CHANNEL_JOIN) {
                    val secrets = identitySecrets
                    if (secrets != null) {
                        JoinCoPublisherScreen(
                            state = channelsState,
                            onBack = { navController.popBackStack() },
                            onJoined = { id ->
                                navController.navigate(Routes.channel(id)) {
                                    popUpTo(Routes.CHANNEL_JOIN) { inclusive = true }
                                }
                            },
                        )
                    } else {
                        LoadingScreen()
                    }
                }
                composable(Routes.CHANNEL) { entry ->
                    val id = entry.arguments?.getString("channelId") ?: return@composable
                    ChannelDetailScreen(
                        channelId = id,
                        state = channelsState,
                        client = channelsClient,
                        onBack = { navController.popBackStack() },
                        onSubscribe = { navController.navigate(Routes.channelSubscribe(id, handle = null)) },
                        onManage = { navController.navigate(Routes.channelManage(id)) },
                    )
                }
                composable(
                    Routes.CHANNEL_SUBSCRIBE,
                    arguments = listOf(
                        androidx.navigation.navArgument("handle") {
                            type = androidx.navigation.NavType.StringType
                            nullable = true
                            defaultValue = null
                        },
                    ),
                ) { entry ->
                    val id = entry.arguments?.getString("channelId") ?: return@composable
                    ChannelSubscribeScreen(
                        channelId = id,
                        state = channelsState,
                        client = channelsClient,
                        handle = entry.arguments?.getString("handle")?.takeIf { it.isNotEmpty() },
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.CHANNEL_MANAGE) { entry ->
                    val id = entry.arguments?.getString("channelId") ?: return@composable
                    val secrets = identitySecrets
                    if (secrets != null) {
                        ChannelManageScreen(
                            channelId = id,
                            state = channelsState,
                            client = channelsClient,
                            onBack = { navController.popBackStack() },
                        )
                    } else {
                        LoadingScreen()
                    }
                }
                composable(Routes.CONTACTS) {
                    val contactsVm: ContactsViewModel = viewModel(
                        factory = singletonViewModelFactory {
                            ContactsViewModel(
                                repo = contactsRepository,
                                chatRepo = chatRepository,
                            )
                        },
                    )
                    // docs/ux/navigation.md section 4.3 — a username the
                    // conversation list's one lookup row resolved. The add
                    // sheet opens with it filled in, and the slot is consumed
                    // so a later visit does not re-open it.
                    val foundUsername by me.lakko.app.contacts.PendingContactLookup
                        .pendingUsername.collectAsStateWithLifecycle()
                    ContactsScreen(
                        viewModel = contactsVm,
                        onOpenContact = { key -> navController.navigate(Routes.contact(key)) },
                        onOpenChat = { hex -> navController.navigate(Routes.chat(hex)) },
                        prefillUsername = foundUsername,
                        onPrefillConsumed = {
                            me.lakko.app.contacts.PendingContactLookup.consume()
                        },
                    )
                }
                // docs/ux/navigation.md section 5 — Calls: recent calls across
                // every conversation, newest first, read off the index written
                // when each call row is. docs/ux/call-links.md section 2 — its
                // one action starts a group video call rather than opening a
                // form; the create-a-link form it used to hold is gone.
                composable(Routes.CALLS) {
                    val callPermissions = me.lakko.app.ui.call.rememberCallPermissionGate()
                    val callContext = LocalContext.current
                    me.lakko.app.ui.calls.CallsHost(
                        repository = chatRepository,
                        onOpenConversation = { hex ->
                            navController.navigate(Routes.chat(hex))
                        },
                        onCallBack = { hex, video ->
                            // The same just-in-time permission gate every call
                            // entry point uses; a call back re-dials the mode
                            // the row records.
                            callPermissions.request(video = video) { granted ->
                                callHost.startOutgoing(
                                    groupIdHex = hex,
                                    kind = me.lakko.app.calls.requestedCallKind(granted),
                                    hasCamera = me.lakko.app.calls.CameraVideoCapture
                                        .hasUsableCamera(callContext),
                                )
                            }
                        },
                        // ADR-0032 — a pasted call link opens the same `/c/`
                        // landing a tapped link does; AppRoot presents it over
                        // the session.
                        onOpenCallLink = { slug -> inviteIntake.publishCallJoin(slug) },
                        identitySecrets = { authRepository.identitySecrets.value },
                    )
                }
                composable(Routes.CONTACT_DETAIL) { entry ->
                    val key = entry.arguments?.getString("key") ?: return@composable
                    val contactsVm: ContactsViewModel = viewModel(
                        factory = singletonViewModelFactory {
                            ContactsViewModel(repo = contactsRepository, chatRepo = chatRepository)
                        },
                    )
                    // The same just-in-time permission gate every call entry
                    // point uses. docs/ux/calls.md section 3: the host dials a
                    // voice call at once and stops only for a video call's
                    // camera check, exactly as the chat header does it. The
                    // chosen kind used to be dropped here, so a video call from
                    // a contact went out as a voice one.
                    val callPermissions = me.lakko.app.ui.call.rememberCallPermissionGate()
                    val callContext = LocalContext.current
                    me.lakko.app.ui.contacts.ContactDetailScreen(
                        contactKey = key,
                        contactsRepository = contactsRepository,
                        viewModel = contactsVm,
                        onBack = { navController.popBackStack() },
                        // docs/ux/conversation-info.md section 3, "Except a
                        // person" — one contact screen, reached from the list
                        // AND from the chat's own header. `Message` from the
                        // chat is a RETURN to it: if that chat is already on
                        // the back stack, pop back to it (the filled-in route
                        // matches that entry) rather than push a second copy;
                        // from the list there is nothing to pop to, so push.
                        onOpenChat = { hex ->
                            if (!navController.popBackStack(Routes.chat(hex), inclusive = false)) {
                                navController.navigate(Routes.chat(hex))
                            }
                        },
                        // ADR-0045 UX 6.2 — the core's own predicate, read the
                        // way the chat screen reads it: an unreadable handle
                        // is not evidence of an eviction.
                        chatMembershipEnded = { hex -> chatRepository.isGroupActive(hex) == false },
                        resolveChat = { linked, key -> chatRepository.chatGroupIdHexFor(linked, key) },
                        // UX 6.2.4 — after the delete, land wherever the chat
                        // was opened from: pop the deleted chat off the stack
                        // too when this screen was pushed from it.
                        onDeleteChat = { hex ->
                            chatRepository.deleteEndedChat(hex)
                            if (!navController.popBackStack(Routes.chat(hex), inclusive = true)) {
                                navController.popBackStack()
                            }
                        },
                        onStartCall = { hex, video ->
                            callPermissions.request(video = video) { granted ->
                                callHost.startOutgoing(
                                    groupIdHex = hex,
                                    kind = me.lakko.app.calls.requestedCallKind(granted),
                                    hasCamera = me.lakko.app.calls.CameraVideoCapture
                                        .hasUsableCamera(callContext),
                                )
                            }
                        },
                        onVerifySafetyNumber = { k -> navController.navigate(Routes.safetyNumber(k)) },
                    )
                }
                composable(Routes.SAFETY_NUMBER) { entry ->
                    val key = entry.arguments?.getString("key") ?: return@composable
                    me.lakko.app.ui.contacts.SafetyNumberScreen(
                        contactKey = key,
                        contactsRepository = contactsRepository,
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.SETTINGS) {
                    val secrets = identitySecrets
                    // ADR-0048 section 4a — the name may change on another of
                    // the account's devices: re-read on every store write.
                    val ownProfileRevision by me.lakko.app.profile.OwnProfileStore.revision
                        .collectAsStateWithLifecycle()
                    val ownProfile = remember(secrets?.accountId, ownProfileRevision) {
                        secrets?.let { me.lakko.app.profile.OwnProfileStore.load(chatRepository.context, it.accountId) }
                    }
                    SettingsScreen(
                        displayName = ownProfile?.displayName,
                        username = account.username.takeIf { it.isNotBlank() },
                        onOpenProfile = { navController.navigate(Routes.PROFILE) },
                        onOpenPasskeys = { navController.navigate(Routes.PASSKEYS) },
                        onOpenDevices = { navController.navigate(Routes.DEVICES) },
                        onOpenRecovery = { navController.navigate(Routes.RECOVERY) },
                        onOpenAntiCensorship = { navController.navigate(Routes.ANTI_CENSORSHIP) },
                        onOpenChannelsIShare = { navController.navigate(Routes.CHANNELS_I_SHARE) },
                        onOpenChatFolders = { navController.navigate(Routes.FOLDERS) },
                        onLogout = logout,
                        onForgetDevice = forgetDevice,
                        onOpenWhoCanFindMe = { navController.navigate(Routes.WHO_CAN_FIND_ME) },
                    )
                }

                composable(Routes.WHO_CAN_FIND_ME) {
                    // docs/ux/who-can-find-me.md — Settings > Privacy > Who can
                    // find me. The groups and channels this account is in are
                    // what "Only the ones I choose" lists.
                    val chatState by chatRepository.state.collectAsStateWithLifecycle()
                    val channels by channelsState.channels.collectAsStateWithLifecycle()
                    val ctx = chatRepository.context
                    val scopes = remember(chatState.threads, channels) {
                        val groups = chatState.threads
                            .filterNot { it.membershipEnded }
                            .filter {
                                // ADR-0062 — the kind read from the group.
                                chatRepository.conversationKind(it.groupIdHex) ==
                                    me.lakko.app.chat.ConversationKind.Group
                            }
                            .map { thread ->
                                me.lakko.app.ui.settings.FindMeScope(
                                    kind = me.lakko.app.ui.settings.FindMeScopeKind.GROUP,
                                    idHex = thread.groupIdHex,
                                    name = chatRepository.groupName(thread.groupIdHex)?.name
                                        ?.takeIf { it.isNotBlank() } ?: "Group",
                                )
                            }
                        val chans = channels.map { handle ->
                            me.lakko.app.ui.settings.FindMeScope(
                                kind = me.lakko.app.ui.settings.FindMeScopeKind.CHANNEL,
                                idHex = handle.id,
                                name = handle.sharedName.ifBlank { handle.localName }.ifBlank { "Channel" },
                            )
                        }
                        groups + chans
                    }
                    me.lakko.app.ui.settings.WhoCanFindMeScreen(
                        reach = me.lakko.app.reach.ReachManager.get(ctx),
                        scopes = scopes,
                        onBack = { navController.popBackStack() },
                    )
                }

                composable(Routes.NEW_CHAT) {
                    NewChatScreen(
                        contactsRepository = contactsRepository,
                        onBack = { navController.popBackStack() },
                        onOpenChat = { hex ->
                            navController.navigate(Routes.chat(hex)) {
                                popUpTo(Routes.NEW_CHAT) { inclusive = true }
                            }
                        },
                        onOpenContacts = { navController.navigateToTab(Routes.CONTACTS) },
                    )
                }
                composable(Routes.NEW_GROUP) {
                    NewGroupScreen(
                        contactsRepository = contactsRepository,
                        chatRepository = chatRepository,
                        onBack = { navController.popBackStack() },
                        onCreated = { hex ->
                            navController.navigate(Routes.chat(hex)) {
                                popUpTo(Routes.NEW_GROUP) { inclusive = true }
                            }
                        },
                        onOpenContacts = { navController.navigateToTab(Routes.CONTACTS) },
                    )
                }

                composable(Routes.CHAT) { entry ->
                    val groupIdHex = entry.arguments?.getString("groupIdHex") ?: return@composable
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
                                callHost = callHost,
                                groupCallHost = me.lakko.app.calls.AppCallHost.group,
                                callOccupant = me.lakko.app.calls.AppCallHost.occupant,
                                endCurrentCall = { me.lakko.app.calls.AppCallHost.endCurrentCall() },
                            )
                        },
                    )
                    // Phase 2a3e2b-Android — wire cross-device sync
                    // into this VM's composer on every navigation.
                    LaunchedEffect(chatVm) {
                        chatVm.attachSync(deviceGroupSync)
                    }
                    // The group's title (local name, else member-derived), re-read
                    // when names or the roster change. A 1x1 names its peer itself.
                    val nameVersion by profileDirectory.version.collectAsStateWithLifecycle()
                    val membersVersion by chatVm.members.collectAsStateWithLifecycle()
                    // ADR-0051 — a shared name accepted on a poll re-titles the header.
                    val groupNameVersion by me.lakko.app.chat.GroupNameStore.version
                        .collectAsStateWithLifecycle()
                    val groupTitle = remember(groupIdHex, nameVersion, membersVersion, groupNameVersion, chatState) {
                        me.lakko.app.ui.chat.conversationTitle(
                            appContext, chatRepository, profileDirectory, groupIdHex,
                        )
                    }
                    ChatScreen(
                        viewModel = chatVm,
                        // Back returns to wherever the chat was opened from.
                        onNavigateBack = { navController.popBackStack() },
                        contactsRepository = contactsRepository,
                        // docs/ux/conversation-info.md section 3, "Except a
                        // person" — a one-to-one header opens the person, which
                        // is the contact screen, pushed exactly as the contacts
                        // list pushes it (the same route, above). This chat
                        // stays beneath it, so Back and `Message` return here.
                        onOpenContact = { key -> navController.navigate(Routes.contact(key)) },
                        groupTitle = groupTitle,
                        // ADR-0052 decision 2 / UX 5.1 — Forward opens the
                        // picker with the message in the one-shot slot; the
                        // picker's route takes it into its own view model.
                        onForwardMessage = { request ->
                            me.lakko.app.chat.PendingForward.publish(request)
                            navController.navigate(Routes.FORWARD)
                        },
                        // UX section 8 — a selection's Forward: the same
                        // picker, one target, the requests oldest first.
                        onForwardSelection = { requests ->
                            me.lakko.app.chat.PendingForward.publishSelection(requests)
                            navController.navigate(Routes.FORWARD)
                        },
                        // UX 5.1 "Tap to jump" — the origin conversation is
                        // pushed over this one; Back returns here.
                        onOpenConversation = { hex ->
                            navController.navigate(Routes.chat(hex)) { launchSingleTop = true }
                        },
                    )
                }
                composable(Routes.FORWARD) {
                    // The request is taken from the slot ONCE, into a view
                    // model scoped to this back-stack entry, so a rotation
                    // keeps it and a second visit to the route starts empty.
                    val forwardVm: me.lakko.app.ui.chat.ForwardViewModel = viewModel(
                        factory = singletonViewModelFactory {
                            me.lakko.app.ui.chat.ForwardViewModel(
                                repo = chatRepository,
                                batch = me.lakko.app.chat.PendingForward.take(),
                            )
                        },
                    )
                    me.lakko.app.ui.chat.ForwardPickerScreen(
                        viewModel = forwardVm,
                        repository = chatRepository,
                        deviceGroupSync = deviceGroupSync,
                        onBack = { navController.popBackStack() },
                        // Section 8 — a selection's forward that stopped short
                        // returns to the source chat beneath, which keeps the
                        // unsent messages selected and says how far it got.
                        onReturnToSource = { navController.popBackStack() },
                        // Section 5.1 — picking sends at once and takes the
                        // person to that conversation. The picker leaves the
                        // back stack; a forward into the conversation it was
                        // opened from lands on the same screen.
                        onForwarded = { hex ->
                            navController.navigate(Routes.chat(hex)) {
                                popUpTo(Routes.FORWARD) { inclusive = true }
                                launchSingleTop = true
                            }
                        },
                    )
                }

                composable(Routes.PROFILE) {
                    // ADR-0048 UX §4 — Settings -> Profile. Keyed on the raw account
                    // id from the live session secrets; a save persists the own
                    // profile and republishes it into every group.
                    val secrets = identitySecrets
                    if (secrets != null) {
                        val ctx = chatRepository.context
                        val accountId = secrets.accountId
                        val ownProfileRevision by me.lakko.app.profile.OwnProfileStore.revision
                            .collectAsStateWithLifecycle()
                        me.lakko.app.ui.profile.ProfileSettingsScreen(
                            current = remember(accountId, ownProfileRevision) {
                                me.lakko.app.profile.OwnProfileStore.load(ctx, accountId)
                            },
                            username = account.username.takeIf { it.isNotBlank() },
                            onSaved = { updated ->
                                me.lakko.app.profile.OwnProfileStore.save(ctx, accountId, updated)
                                chatRepository.publishOwnProfileToAllGroups()
                                // ADR-0048 section 4a — and to the account's
                                // other devices, with the name's stored stamp.
                                me.lakko.app.reach.ReachManager.get(ctx).publishOwnProfileToDevices()
                            },
                            onBack = { navController.popBackStack() },
                        )
                    } else {
                        LoadingScreen()
                    }
                }
                composable(Routes.PASSKEYS) {
                    val passkeysVm: me.lakko.app.webauthn.PasskeysViewModel = viewModel(
                        factory = singletonViewModelFactory {
                            me.lakko.app.webauthn.PasskeysViewModel(
                                repo = authRepository,
                                onAccountDeleted = forgetDevice,
                            )
                        },
                    )
                    me.lakko.app.webauthn.PasskeysScreen(
                        viewModel = passkeysVm,
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.DEVICES) {
                    DevicesScreen(
                        authRepository = authRepository,
                        onBack = { navController.popBackStack() },
                        onAddDevice = { navController.navigate(Routes.DEVICES_ADD) },
                        chatRepository = chatRepository,
                    )
                }
                composable(Routes.DEVICES_ADD) {
                    // ADR-0050 — the scan side admits the new device to every chat
                    // (`chatRepository.requestAdmitDevice`) inside the screen, as
                    // before the redesign.
                    me.lakko.app.ui.devices.AddDeviceScreen(
                        authRepository = authRepository,
                        onBack = { navController.popBackStack() },
                        chatRepository = chatRepository,
                    )
                }
                // docs/ux/chat-folders.md section 4 — Settings > Chat folders.
                composable(Routes.FOLDERS) {
                    me.lakko.app.ui.chat.ChatFoldersSettingsScreen(
                        repository = chatRepository,
                        channels = channelsState,
                        deviceGroupSync = deviceGroupSync,
                        onBack = { navController.popBackStack() },
                        onEditFolder = { id -> navController.navigate(Routes.folderEdit(id, null)) },
                        onNewFolder = { navController.navigate(Routes.folderEdit(null, null)) },
                    )
                }
                // Section 3 — the folder editor: an existing folder, or a new
                // one optionally holding the chat "New folder..." came from.
                composable(
                    Routes.FOLDER_EDIT,
                    arguments = listOf(
                        androidx.navigation.navArgument("folder") {
                            type = androidx.navigation.NavType.StringType
                            nullable = true
                            defaultValue = null
                        },
                        androidx.navigation.navArgument("prefill") {
                            type = androidx.navigation.NavType.StringType
                            nullable = true
                            defaultValue = null
                        },
                    ),
                ) { entry ->
                    me.lakko.app.ui.chat.FolderEditorScreen(
                        repository = chatRepository,
                        channels = channelsState,
                        deviceGroupSync = deviceGroupSync,
                        folderIdHex = entry.arguments?.getString("folder")?.takeIf { it.isNotEmpty() },
                        prefill = Routes.decodeFolderPrefill(entry.arguments?.getString("prefill")),
                        onDone = { navController.popBackStack() },
                    )
                }
                composable(Routes.RECOVERY) {
                    me.lakko.app.ui.settings.RecoveryScreen(
                        authRepository = authRepository,
                        onBack = { navController.popBackStack() },
                    )
                }
                // ADR-0059 section 6 — Settings -> Security -> Channels I share.
                composable(Routes.CHANNELS_I_SHARE) {
                    me.lakko.app.ui.discover.ChannelsIShareScreen(
                        chatRepository = chatRepository,
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.ANTI_CENSORSHIP) {
                    AntiCensorshipSettingsScreen(
                        manager = antiCensorship,
                        onBack = { navController.popBackStack() },
                    )
                }

                composable(Routes.INVITE) { entry ->
                    val inviteSlug = entry.arguments?.getString("slug") ?: return@composable
                    // Keyed by slug so re-tapping a different invite URL builds a
                    // fresh VM with the right slug binding.
                    val inviteVm: InviteLandingViewModel = viewModel(
                        key = "invite-$inviteSlug",
                        factory = singletonViewModelFactory {
                            InviteLandingViewModel(contactsRepository, inviteSlug)
                        },
                    )
                    InviteLandingScreen(
                        viewModel = inviteVm,
                        onDone = { navController.popBackStack() },
                        onOpenChat = { hex ->
                            navController.navigate(Routes.chat(hex)) {
                                popUpTo(Routes.INVITE) { inclusive = true }
                            }
                        },
                    )
                }
                composable(Routes.GROUP_JOIN) { entry ->
                    // ADR-0028 (G-3) — request to join the group the `/g/<slug>`
                    // link points at + poll for the admin's approval.
                    // The one argument carries `<slug>` or `<slug>#<fragment>`
                    // (ADR-0028 v2); the fragment is the preview's link key.
                    val groupJoinArgument = entry.arguments?.getString("slug") ?: return@composable
                    val groupJoinLink = me.lakko.app.chat.parseGroupJoinArgument(groupJoinArgument)
                    val groupJoinVm: me.lakko.app.ui.chat.JoinGroupLandingViewModel = viewModel(
                        key = "group-join-$groupJoinArgument",
                        factory = singletonViewModelFactory {
                            me.lakko.app.ui.chat.JoinGroupLandingViewModel(
                                chatRepository,
                                groupJoinLink.slug,
                                me.lakko.app.chat.parseGroupInviteFragment(groupJoinLink.fragment),
                            )
                        },
                    )
                    me.lakko.app.ui.chat.JoinGroupLandingScreen(
                        viewModel = groupJoinVm,
                        onDone = { navController.popBackStack() },
                        onOpenChat = { hex ->
                            // Replace the landing with the chat, so Back from
                            // the chat does not return to a finished join.
                            navController.popBackStack()
                            navController.navigate(Routes.chat(hex)) { launchSingleTop = true }
                        },
                    )
                }
            }
        }
    }
}

/** App-level receive loop: DS long-poll budget per pass. */
private const val APP_LONG_POLL_SECS: Int = 30

/** App-level receive loop: pause after a failed pass. */
private const val APP_POLL_RETRY_BACKOFF_MS: Long = 5_000L

/** App-level receive loop: a pass that returned sooner than this waits out the rest. */
private const val APP_POLL_MIN_ITERATION_MS: Long = 1_000L

/** Navigation routes of the session shell. */
private object Routes {
    const val CHATS = "chats"
    const val FEED = "feed"
    const val CONTACTS = "contacts"
    const val CALLS = "calls"
    const val SETTINGS = "settings"
    val TOP_LEVEL: Set<String> = setOf(CHATS, FEED, CONTACTS, CALLS, SETTINGS)

    /** `docs/ux/feed.md` section 4.1 — a shared feed; the rule rides the
     *  [me.lakko.app.ui.feed.FeedImportIntake] slot, not the route. */
    const val FEED_IMPORT = "feed_import"

    const val CHAT = "chat/{groupIdHex}"
    fun chat(groupIdHex: String): String = "chat/$groupIdHex"
    /** ADR-0052 decision 2 — the "Forward to" picker. Argument-less: the
     *  message rides the [me.lakko.app.chat.PendingForward] slot. */
    const val FORWARD = "forward"
    const val NEW_CHAT = "new_chat"
    const val NEW_GROUP = "new_group"
    const val CHANNEL_NEW = "channel_new"
    const val CHANNEL_JOIN = "channel_join"
    /** A channel; `channelId` is the 32-char hex id (URL-safe by construction). */
    const val CHANNEL = "channel/{channelId}"
    fun channel(channelId: String): String = "channel/$channelId"
    const val CHANNEL_SUBSCRIBE = "channel/{channelId}/subscribe?handle={handle}"
    fun channelSubscribe(channelId: String, handle: String?): String =
        "channel/$channelId/subscribe?handle=${android.net.Uri.encode(handle.orEmpty())}"
    const val CHANNEL_MANAGE = "channel/{channelId}/manage"
    fun channelManage(channelId: String): String = "channel/$channelId/manage"
    const val PROFILE = "settings/profile"
    const val WHO_CAN_FIND_ME = "settings/privacy/who-can-find-me"
    const val PASSKEYS = "settings/passkeys"
    const val DEVICES = "settings/devices"
    const val DEVICES_ADD = "settings/devices/add"
    const val RECOVERY = "settings/recovery"
    const val ANTI_CENSORSHIP = "settings/anti_censorship"
    const val CHANNELS_I_SHARE = "settings/channels_i_share"
    /** docs/ux/chat-folders.md section 4. */
    const val FOLDERS = "settings/folders"
    /** The folder editor; both arguments are hex (URL-safe by construction). */
    const val FOLDER_EDIT = "folder_edit?folder={folder}&prefill={prefill}"
    fun folderEdit(folderIdHex: String?, prefill: uniffi.lakko_ffi_uniffi.FolderConversationRef?): String {
        val p = prefill?.let { ref ->
            val k = when (ref.kind) {
                uniffi.lakko_ffi_uniffi.FolderConversationKind.ONE_TO_ONE -> "o"
                uniffi.lakko_ffi_uniffi.FolderConversationKind.GROUP -> "g"
                uniffi.lakko_ffi_uniffi.FolderConversationKind.CHANNEL -> "c"
            }
            k + me.lakko.app.chat.rowIdOf(ref)
        }
        return "folder_edit?folder=${folderIdHex.orEmpty()}&prefill=${p.orEmpty()}"
    }
    fun decodeFolderPrefill(arg: String?): uniffi.lakko_ffi_uniffi.FolderConversationRef? {
        if (arg.isNullOrEmpty()) return null
        val kind = when (arg[0]) {
            'o' -> uniffi.lakko_ffi_uniffi.FolderConversationKind.ONE_TO_ONE
            'g' -> uniffi.lakko_ffi_uniffi.FolderConversationKind.GROUP
            'c' -> uniffi.lakko_ffi_uniffi.FolderConversationKind.CHANNEL
            else -> return null
        }
        val id = me.lakko.app.devices.sync.hexToBytesOrNull(arg.substring(1)) ?: return null
        return uniffi.lakko_ffi_uniffi.FolderConversationRef(kind = kind, id = id)
    }
    /** The contact detail; `key` is the base64url signature key (URL-safe by construction). */
    const val CONTACT_DETAIL = "contact/{key}"
    fun contact(key: String): String = "contact/$key"
    const val SAFETY_NUMBER = "contact/{key}/safety"
    fun safetyNumber(key: String): String = "contact/$key/safety"
    const val INVITE = "invite/{slug}"
    fun invite(slug: String): String = "invite/${android.net.Uri.encode(slug)}"
    const val GROUP_JOIN = "group_join/{slug}"
    fun groupJoin(link: me.lakko.app.chat.GroupJoinLink): String =
        "group_join/${android.net.Uri.encode(me.lakko.app.chat.composeGroupJoinArgument(link))}"
}

private data class TabSpec(
    val route: String,
    val label: String,
    val icon: ImageVector,
    val selectedIcon: ImageVector,
)

/**
 * The five primary destinations, in the order every client shows them
 * (`docs/ux/navigation.md` section 1; Feed added by ADR-0061).
 *
 * Groups and Channels are gone as destinations: both are KINDS of conversation
 * inside Chats now, because a person does not think "I will go to the Groups
 * place" — they think of a conversation and look for it, and a list sorted by
 * WHEN cannot be three lists. Their creation flows and their screens are all
 * still here, reached from the merged list and the create menu.
 *
 * Calls is back. It was removed on the recorded ground that it "has no history,
 * only a create-link panel", which was true of what existed and was never an
 * argument that call history should not exist.
 */
private val Tabs: List<TabSpec> = listOf(
    TabSpec(Routes.CHATS, "Chats", Icons.Outlined.ChatBubbleOutline, Icons.Filled.ChatBubble),
    // `docs/ux/feed.md` section 2 — second, one tap from the home.
    TabSpec(Routes.FEED, "Feed", Icons.Outlined.DynamicFeed, Icons.Filled.DynamicFeed),
    TabSpec(Routes.CONTACTS, "Contacts", Icons.Outlined.People, Icons.Filled.People),
    TabSpec(Routes.CALLS, "Calls", Icons.Outlined.Call, Icons.Filled.Call),
    TabSpec(Routes.SETTINGS, "Settings", Icons.Outlined.Settings, Icons.Filled.Settings),
)

/** Standard tab switch: one back-stack per tab, state saved and restored. */
private fun NavHostController.navigateToTab(route: String) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

/** Safe-drawing insets for the roots outside the tabbed shell (auth, recovery). */
@Composable
private fun InsetRoot(content: @Composable () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .windowInsetsPadding(WindowInsets.safeDrawing),
    ) {
        content()
    }
}

/**
 * Signed-out subgraph (docs/ux/client-redesign.md section 5.6): Welcome is
 * the start destination; Log in, Create account and Recover are pushed from
 * it inside their own `NavHost`, so the top-app-bar arrow and system back
 * both pop. [cachedUsername] pre-fills the log-in form and scopes Recover to
 * the account this phone knows.
 */
@Composable
private fun AnonRoot(
    repository: AuthRepository,
    cachedUsername: String?,
) {
    val appContext = LocalContext.current.applicationContext
    val nav = rememberNavController()

    NavHost(navController = nav, startDestination = AnonRoutes.WELCOME) {
        composable(AnonRoutes.WELCOME) {
            me.lakko.app.ui.auth.WelcomeScreen(
                onCreateAccount = { nav.navigate(AnonRoutes.SIGNUP) { launchSingleTop = true } },
                onLogIn = { nav.navigate(AnonRoutes.LOGIN) { launchSingleTop = true } },
            )
        }
        composable(AnonRoutes.LOGIN) {
            val loginVm: LoginViewModel = viewModel(
                key = "login-${cachedUsername ?: ""}",
                factory = singletonViewModelFactory {
                    LoginViewModel(repository, cachedUsername)
                },
            )
            LoginScreen(
                viewModel = loginVm,
                onBack = { nav.popBackStack() },
                onNavigateToRecover = { nav.navigate(AnonRoutes.RECOVER) { launchSingleTop = true } },
            )
        }
        composable(AnonRoutes.SIGNUP) {
            val signupVm: SignupViewModel = viewModel(
                factory = singletonViewModelFactory {
                    SignupViewModel(
                        repo = repository,
                        // ADR-0048 — the optional "Your name" lands in the
                        // own-profile store once the account exists.
                        saveOwnProfile = { accountId, profile ->
                            me.lakko.app.profile.OwnProfileStore.save(appContext, accountId, profile)
                        },
                        // The guided phrase step renders above the session
                        // (AppRoot reads the gate) once sign-up lands it.
                        parkRecoveryPhrase = { me.lakko.app.ui.auth.SignupPhraseGate.park(it) },
                    )
                },
            )
            SignupScreen(
                viewModel = signupVm,
                onBack = { nav.popBackStack() },
            )
        }
        composable(AnonRoutes.RECOVER) {
            val recoverVm: RecoverViewModel = viewModel(
                key = "recover-${cachedUsername ?: ""}",
                factory = singletonViewModelFactory {
                    RecoverViewModel(repository, cachedUsername)
                },
            )
            RecoverScreen(
                viewModel = recoverVm,
                onBack = { nav.popBackStack() },
            )
        }
    }
}

private object AnonRoutes {
    const val WELCOME = "welcome"
    const val LOGIN = "login"
    const val SIGNUP = "signup"
    const val RECOVER = "recover"
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
 * and [AuthRepository] rehydration, which runs off the main thread (a
 * Keystore decrypt plus three `EncryptedSharedPreferences` opens), so this is
 * what the first frame shows on a signed-in cold start.
 */
@Composable
private fun LoadingScreen() {
    Column(
        modifier = Modifier.fillMaxSize(),
        verticalArrangement = androidx.compose.foundation.layout.Arrangement.Center,
        horizontalAlignment = androidx.compose.ui.Alignment.CenterHorizontally,
    ) {
        Text(
            text = "Lakko",
            style = MaterialTheme.typography.displaySmall,
            modifier = Modifier.padding(24.dp),
        )
        androidx.compose.material3.CircularProgressIndicator()
    }
}
