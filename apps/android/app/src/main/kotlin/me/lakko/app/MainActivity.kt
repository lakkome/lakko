// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
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
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
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
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.rememberSaveable
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
import me.lakko.app.contacts.publishLink
import me.lakko.app.feed.reporting
import me.lakko.app.push.LakkoMessagingService
import me.lakko.app.push.PushIntake
import me.lakko.app.ui.auth.LoginScreen
import me.lakko.app.ui.call.EndedForEveryoneNoticeBar
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
import me.lakko.app.ui.channels.ChannelConversationScreen
import me.lakko.app.ui.channels.ChannelInfoScreen
import me.lakko.app.ui.channels.JoinCoPublisherScreen
import me.lakko.app.ui.channels.NewChannelScreen
import me.lakko.app.ui.channels.rememberChannelsUiState
import me.lakko.app.ui.contacts.ContactsScreen
import me.lakko.app.ui.contacts.ContactsViewModel
import me.lakko.app.ui.contacts.InviteLandingScreen
import me.lakko.app.ui.contacts.InviteLandingViewModel
import me.lakko.app.ui.contacts.keepsLinkParked
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
        me.lakko.app.ui.theme.TextSizePreference.init(applicationContext)
        // ADR-0061 — the interface language: stored choice, else the system
        // language, else English. Loaded before the first frame.
        me.lakko.app.l10n.Localization.init(applicationContext)
        // ADR-0098 F2: the standing proofs refuse a moderator list older
        // than the one this device has seen (the report sheet's floor).
        me.lakko.app.moderation.ModeratorListCache.installFloor(
            me.lakko.app.moderation.ModeratorListVersionStore(applicationContext),
        )

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
        // docs/ux/calls.md section 4.5.4 — an activity recreated while in the
        // system's picture-in-picture window starts out knowing it.
        me.lakko.app.calls.CallPictureInPicture.setActive(isInPictureInPictureMode)

        // One set per process, not per Activity: a configuration change
        // recreates this Activity, and a second set built here would run
        // beside the first for the same account (see [AppRepositories]).
        val repositories = AppRepositories.get(applicationContext)
        val authRepository = repositories.auth
        val chatRepository = repositories.chat
        // Every call lives in the process-wide call host, not in a screen: the
        // 1x1 call must ring whichever screen is showing, and no call — 1x1,
        // group or call link — may end or vanish because its screen went away.
        // Every poll of every group hands one-to-one call signaling to it, and
        // every settled group reaches the group-call rekey. Attached on every
        // Activity create; the repository is the process's one.
        me.lakko.app.calls.AppCallHost.attach(
            applicationContext = applicationContext,
            repository = chatRepository,
            sessionTokenProvider = { authRepository.identitySecrets.value?.sessionToken },
        )
        val callHost = me.lakko.app.calls.AppCallHost.instance
        val contactsRepository = repositories.contacts
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
        val deviceGroupSync = repositories.deviceGroupSync
        // ADR-0073 R.5 — a conversation that continues in a new group keeps
        // its row through the lineage register of the synced state.
        chatRepository.lineageRecorder = { successor, root -> deviceGroupSync.recordLineage(successor, root) }
        // One person, one one-to-one row (`chat/DuplicateOneToOne.kt`): a
        // one-to-one the other person founded continues an older one this
        // account founded with them. Re-run whenever the held groups or the
        // lineage move; a register already held is not written again.
        lifecycleScope.launch(kotlinx.coroutines.Dispatchers.IO) {
            kotlinx.coroutines.flow.combine(
                chatRepository.state,
                deviceGroupSync.lineage,
            ) { chats, lineage -> Triple(chats.bootstrapped, chats.threads.map { it.groupIdHex }.toSet(), lineage) }
                .distinctUntilChanged()
                .collect { (bootstrapped, held, _) ->
                    if (!bootstrapped || held.size < 2) return@collect
                    try {
                        chatRepository.foldDuplicateOneToOnes()
                    } catch (cause: Throwable) {
                        if (cause is kotlinx.coroutines.CancellationException) throw cause
                        android.util.Log.w("MainActivity", "duplicate one-to-one fold failed: ${cause.javaClass.simpleName}")
                    }
                }
        }
        // ADR-0065 section 4 — a saved message its author deleted for everyone
        // leaves the saved list, as it leaves the pinned bar.
        chatRepository.onMessagesDeleted = { segmentHex, ids ->
            val gone = me.lakko.app.chat.savedDeletedIn(deviceGroupSync.saved.value, segmentHex, ids)
            if (gone.isNotEmpty()) {
                lifecycleScope.launch {
                    try {
                        deviceGroupSync.applySavedChanges(
                            gone.map {
                                uniffi.lakko_ffi_uniffi.composeSavedRemoval(
                                    me.lakko.app.devices.sync.Hlc.mint(),
                                    it.conversation,
                                    it.messageId,
                                )
                            },
                        )
                    } catch (cause: Throwable) {
                        if (cause is kotlinx.coroutines.CancellationException) throw cause
                        android.util.Log.w("MainActivity", "saved removal of a deleted message failed: ${cause.javaClass.simpleName}")
                    }
                }
            }
        }
        // ADR-0079 section 4 — the contacts runtime reads the synced
        // link-record register (auto-accept, the link list, revocations).
        contactsRepository.deviceGroupSync = deviceGroupSync
        // ADR-0075 (amended 2026-10-13) - the account's devices keep the
        // recovery address in one register of the synced state.
        authRepository.recoveryAddressPort = object : me.lakko.app.auth.RecoveryAddressPort {
            override fun state(): me.lakko.app.auth.RecoveryAddressState = deviceGroupSync.recoveryEmail.value

            override suspend fun write(address: String?): Boolean = deviceGroupSync.setRecoveryEmail(address)
        }
        // ADR-0076 - and the account's username in another.
        authRepository.ownUsernamePort = object : me.lakko.app.auth.OwnUsernamePort {
            override fun state(): me.lakko.app.auth.OwnUsernameState = deviceGroupSync.ownUsername.value

            override suspend fun write(username: String?): Boolean = deviceGroupSync.setOwnUsername(username)
        }
        // The username changed on this device's marker (a rename here, or
        // one adopted from another device): the stored profile takes it and
        // the existing publishers follow (profile, Everyone, introductions).
        authRepository.onUsernameChanged = { username, renamedElsewhere ->
            authRepository.identitySecrets.value?.accountId?.let { accountId ->
                val existing = me.lakko.app.profile.OwnProfileStore.load(applicationContext, accountId)
                me.lakko.app.profile.applyOwnNameChange(
                    current = existing,
                    newName = existing?.displayName,
                    username = username.takeIf { it.isNotBlank() },
                    nowMs = System.currentTimeMillis(),
                )?.let { me.lakko.app.profile.OwnProfileStore.save(applicationContext, accountId, it) }
            }
            if (renamedElsewhere) {
                // The certificate held here is the old name's: fetched again.
                chatRepository.refreshUsernameCertificate()
            } else {
                chatRepository.publishOwnProfileToAllGroups()
            }
            me.lakko.app.reach.ReachManager.get(applicationContext).publishOwnProfileToDevices()
        }

        // A restore that ran under a live session gave the name or photo back.
        authRepository.onOwnProfileRestored = {
            chatRepository.publishOwnProfileToAllGroups()
            me.lakko.app.reach.ReachManager.get(applicationContext).publishOwnProfileToDevices()
        }

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

        // ADR-0061 S6 — signed translation updates on the same cadence (once
        // per 24h at launch, through the tunnel when armed). Dormant without a
        // pinned translation key; an accepted bundle switches the live strings.
        me.lakko.app.l10n.TranslationUpdater(applicationContext, lifecycleScope).checkOnLaunch()

        // ADR-0097 — the signed supporters list, at most once a day at
        // launch (and when Settings > About is shown). Dormant with no
        // release key pinned.
        me.lakko.app.updates.SupportersListController.get(applicationContext).refreshIfDue()

        // Read the launching Intent — a cold-start tap on
        // `https://lakko.me/i/<slug>` carries the URL in
        // `intent.data`; a cold-start tap on a Lakko push
        // notification carries the wake extras
        // (EXTRA_WAKE_GROUP / EXTRA_WAKE_POINTER). `onNewIntent`
        // below handles the warm-start variants of both.
        handleLakkoLinkIntent(intent)
        handleFeedImportIntent(intent)
        handleFeedFileIntent(intent)
        handleWakeIntent(intent)
        handleCallRingIntent(intent)
        handleOpenDevicesIntent(intent)
        handleOpenChannelIntent(intent)

        setContent {
            me.lakko.app.l10n.ProvideLocalization {
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
                    // docs/ux/message-links.md section 2 — a Lakko link tapped
                    // in a message goes where an opened app link goes.
                    androidx.compose.runtime.CompositionLocalProvider(
                        me.lakko.app.ui.components.LocalOpenLakkoLink provides { link ->
                            inviteIntake.publishLink(link)
                        },
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
                        // ADR-0098 — the block dialog, the report sheet and the
                        // notice after a report, for every screen.
                        me.lakko.app.ui.moderation.ModerationHost(
                            contactsRepository = contactsRepository,
                            chatRepository = chatRepository,
                        )
                    }
                }
            }
            }
        }
    }

    /**
     * MainActivity declares `locale|layoutDirection` config changes, so a
     * language switch (ours, or one made in system Settings > Apps > Lakko >
     * Language) does not recreate the Activity and lose its state; the
     * language holder re-reads the choice and Compose re-renders.
     */
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        me.lakko.app.l10n.Localization.refresh()
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
        me.lakko.app.devices.DeviceCardCenter.foreground = true
        // ADR-0081 third amendment - a device answers DeviceGroup knocks only
        // while it shows.
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.setAppForeground(true)
        // ADR-0060 §4 — the scheduled `fof` rotation fires when the app is
        // active, never from a background job of its own.
        me.lakko.app.reach.ReachManager.peek()?.onAppActive()
    }

    /** ADR-0080 section 3: the person's touches and keys on this device are
     *  what refreshes its device card; never a timer. */
    override fun onUserInteraction() {
        super.onUserInteraction()
        me.lakko.app.devices.DeviceCardCenter.noteAction()
    }

    /**
     * `docs/ux/calls.md` section 4.5.4 — on Android 8 to 11 the system window
     * is entered when the person leaves the app with home or a switch during a
     * call; on 12 and later the platform enters it by itself.
     */
    override fun onUserLeaveHint() {
        super.onUserLeaveHint()
        me.lakko.app.calls.CallPictureInPicture.onUserLeaveHint(this)
    }

    override fun onPictureInPictureModeChanged(
        isInPictureInPictureMode: Boolean,
        newConfig: android.content.res.Configuration,
    ) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig)
        me.lakko.app.calls.CallPictureInPicture.setActive(isInPictureInPictureMode)
    }

    override fun onPause() {
        super.onPause()
        me.lakko.app.calls.IncomingCallRinger.setAppForeground(false)
        me.lakko.app.devices.DeviceCardCenter.foreground = false
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.setAppForeground(false)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        // Standard `singleTask` /`launchMode = singleInstance`
        // shape: warm-start deep links + push taps route here.
        // We DON'T call `setIntent(intent)` because the Compose
        // tree doesn't observe `getIntent()` directly — we
        // publish into the appropriate intake whose StateFlow
        // drives the re-render.
        handleLakkoLinkIntent(intent)
        handleFeedImportIntent(intent)
        handleFeedFileIntent(intent)
        handleWakeIntent(intent)
        handleCallRingIntent(intent)
        handleOpenDevicesIntent(intent)
        handleOpenChannelIntent(intent)
    }

    /**
     * An opened `https?://lakko.me/{i,g,c}/<slug>#<secret>` app link: read by
     * [me.lakko.app.contacts.lakkoLinkFromPath] and handed to [InviteIntake]
     * so the Compose router opens the matching landing. A QR code scanned on
     * the Contacts screen goes through the same two functions.
     *
     * Non-VIEW intents (a launcher tap re-delivering the last intent) and a
     * path with an empty slug (the manifest's `pathPrefix` allows `/i/`
     * alone) are ignored. The `#<link secret>` fragment travels with the
     * slug; `Uri.fragment` is decoded and never sent. A link that lost it is
     * refused as damaged on the landing, not here.
     */
    private fun handleLakkoLinkIntent(intent: Intent) {
        if (intent.action != Intent.ACTION_VIEW) return
        val uri = intent.data ?: return
        val path = uri.path ?: return
        val link = me.lakko.app.contacts.lakkoLinkFromPath(path, uri.fragment) ?: return
        inviteIntake.publishLink(link)
    }

    /**
     * ADR-0063 / `docs/ux/feed.md` section 4.1 — a shared feed link
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
    /** `docs/ux/identity-recovery.md` 17.5 - a tap on the "A device was
     *  added to your account" notification opens Settings > Devices. */
    private fun handleOpenDevicesIntent(intent: Intent) {
        if (!intent.getBooleanExtra(me.lakko.app.devices.NewDeviceNotices.EXTRA_OPEN_DEVICES, false)) return
        intent.removeExtra(me.lakko.app.devices.NewDeviceNotices.EXTRA_OPEN_DEVICES)
        me.lakko.app.devices.NewDeviceNotices.requestOpenDevices()
    }

    /** `docs/ux/channel-notifications.md` - a tap on a channel post
     *  notification opens that channel. */
    private fun handleOpenChannelIntent(intent: Intent) {
        val id = intent.getStringExtra(me.lakko.app.feed.ChannelNotificationIntake.EXTRA_OPEN_CHANNEL)
            ?.trim()
            ?.takeIf { it.isNotEmpty() }
            ?: return
        intent.removeExtra(me.lakko.app.feed.ChannelNotificationIntake.EXTRA_OPEN_CHANNEL)
        me.lakko.app.feed.ChannelNotificationIntake.request(id)
    }

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

    // ADR-0073 section 3.5 / 4.2 (docs/ux/identity-recovery.md 15.2) — this
    // device's keys are not the account's keys any more: the account was
    // reset on another device, or the directory serves other keys. The
    // signed-out screen replaces the whole app in EVERY auth state, at launch
    // too; no chat list behind it, no dismiss. Rendering it here, ahead of the
    // router, takes the session shell out of composition, which stops its
    // receive loops; the runtimes that outlive composition are stopped
    // below, then the full wipe runs (nothing is published first).
    val keyResetSignOut by authRepository.keyResetSignOut.collectAsStateWithLifecycle()
    val keysReset = keyResetSignOut
    if (keysReset != null) {
        val wipe = {
            me.lakko.app.push.PushSignOut.clearFirst(chatRepository.context)
            me.lakko.app.calls.AppCallHost.reset()
            contactsRepository.clear()
            chatRepository.clear()
            deviceGroupSync.clear()
            authRepository.wipeAfterKeyReset()
        }
        LaunchedEffect(keysReset.kind, keysReset.phase) {
            // A known reset, and a removal by another device (ADR-0074):
            // the wipe runs by itself. Only the mismatch waits for the tap.
            if (keysReset.kind != me.lakko.app.auth.KeyResetSignOut.Kind.Mismatch &&
                keysReset.phase == me.lakko.app.auth.KeyResetSignOut.Phase.Removing
            ) {
                kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) { wipe() }
            }
        }
        InsetRoot {
            me.lakko.app.ui.devices.KeysResetSignedOutScreen(
                signOut = keysReset,
                onSignIn = { authRepository.dismissKeyResetSignOut() },
                onSignOut = {
                    // The mismatch variant: the deletion runs on the tap.
                    if (keysReset.kind == me.lakko.app.auth.KeyResetSignOut.Kind.Mismatch) {
                        Thread { wipe() }.start()
                    }
                },
            )
        }
        return
    }

    // ADR-0074 part 1 (docs/ux/identity-recovery.md 18.4) - this device holds
    // a recall record: the account's keys were reset elsewhere and what the
    // account kept here is already deleted. "Was it you?" replaces the whole
    // app, in every auth state and at launch (the state is rebuilt from the
    // record), until it is answered.
    val recall by authRepository.recall.collectAsStateWithLifecycle()
    val recallUi = recall
    if (recallUi != null) {
        InsetRoot {
            me.lakko.app.ui.devices.RecallRoot(repository = authRepository, ui = recallUi)
        }
        return
    }

    // ADR-0098 section 4 (docs/ux/reporting-and-moderation.md section 4) -
    // the account is suspended. The suspended screen replaces the app until
    // "Read my chats"; then the app runs read-only with a row back to it.
    // While the app is in front the refresh route is asked now and every ten
    // minutes; a normal pair ends the mode without a word.
    val suspension by authRepository.suspension.collectAsStateWithLifecycle()
    val suspendedUi = suspension
    if (suspendedUi != null) {
        val suspensionLifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current
        LaunchedEffect(suspendedUi.accountIdB64, suspensionLifecycle) {
            suspensionLifecycle.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
                kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                    authRepository.keepCheckingSuspension()
                }
            }
        }
        if (!suspendedUi.reading) {
            val signOutScope = androidx.compose.runtime.rememberCoroutineScope()
            InsetRoot {
                me.lakko.app.ui.suspension.SuspendedScreen(
                    repository = authRepository,
                    ui = suspendedUi,
                    onSignOut = {
                        signOutScope.launch(kotlinx.coroutines.Dispatchers.IO) {
                            // As every sign-out: the push binding first, then
                            // the runtimes that could write a store back.
                            me.lakko.app.push.PushSignOut.clearFirst(chatRepository.context)
                            me.lakko.app.calls.AppCallHost.reset()
                            contactsRepository.clear()
                            deviceGroupSync.clear()
                            chatRepository.clear()
                            authRepository.signOutSuspended()
                        }
                    },
                    deleteDialog = { onClose ->
                        val deleteVm: me.lakko.app.webauthn.PasskeysViewModel = viewModel(
                            key = "suspended-delete-${suspendedUi.accountIdB64}",
                            factory = singletonViewModelFactory {
                                me.lakko.app.webauthn.PasskeysViewModel(
                                    repo = authRepository,
                                    // After the erasure: the same local wipe
                                    // Delete account always runs.
                                    onAccountDeleted = {
                                        me.lakko.app.push.PushSignOut.clearFirst(chatRepository.context)
                                        me.lakko.app.calls.AppCallHost.reset()
                                        contactsRepository.clear()
                                        deviceGroupSync.clear()
                                        chatRepository.forgetDevice()
                                        authRepository.forgetDevice()
                                    },
                                )
                            },
                        )
                        me.lakko.app.webauthn.DeleteAccountConfirmation(viewModel = deleteVm, onClose = onClose)
                    },
                )
            }
            return
        }
    }

    // docs/ux/call-links.md section 9 — a guest account (made to join a call
    // from a link) never lands in the app. The repository publishes the guest
    // flag before the session and drops it after, so a session observed here
    // already carries it.
    val guestAccount by authRepository.guestAccount.collectAsStateWithLifecycle()
    val guest = me.lakko.app.auth.guestSessionOf(state, guestAccount)
    // `No, thanks` runs the Forget-device orchestration, which lives in the
    // session root (it stops the runtimes that root owns before the wipe).
    val forgetDeviceSlot = remember { ForgetDeviceSlot() }
    // One model for the question, shared by the call-link landing and the
    // whole-screen form, so a step taken on one is not lost on the other.
    val guestOffer: me.lakko.app.ui.auth.GuestAccountOfferViewModel? = guest?.let { guestSession ->
        viewModel(
            key = "guest-offer-${guestSession.accountIdB64}",
            factory = singletonViewModelFactory {
                me.lakko.app.ui.auth.GuestAccountOfferViewModel(
                    claim = { username, password ->
                        authRepository.claimGuestAccount(
                            username = username,
                            password = password,
                            forgetThisDevice = { forgetDeviceSlot.forget(fallback = { authRepository.forgetDevice() }) },
                        )
                    },
                    discard = {
                        authRepository.discardGuestAccount(
                            forgetThisDevice = { forgetDeviceSlot.forget(fallback = { authRepository.forgetDevice() }) },
                        )
                    },
                    // A guest account lives 23 hours on a device: past that it
                    // is deleted when the question appears, not offered.
                    expire = {
                        authRepository.expireGuestIfDue(
                            forgetThisDevice = { forgetDeviceSlot.forget(fallback = { authRepository.forgetDevice() }) },
                        )
                    },
                    claimedUsername = {
                        me.lakko.app.auth.guestSessionOf(
                            authRepository.state.value,
                            authRepository.guestAccount.value,
                        )?.claimedUsername
                    },
                    signupOpen = { authRepository.signupOpen() },
                )
            },
        )
    }

    // ADR-0032 (G-21/G-22) — a tapped `https://lakko.me/c/<slug>` multi-joiner
    // call-invite link opens the PUBLIC JoinCall landing, in ANY auth state (an
    // anonymous visitor gets the guest-join path). Without a session it takes
    // over the whole screen, gated before the auth-state router. With a
    // session it is drawn ABOVE the session screens instead (see the end of
    // this function): the session root keeps running underneath — its receive
    // loop is what answers a 1x1 offer busy during the link call. A guest's
    // session lands the moment its account exists, so the landing simply
    // moves from the first place to the second and the call starts.
    val pendingCallJoinSlug by inviteIntake.pendingCallJoinSlug.collectAsStateWithLifecycle()
    val callSlug = pendingCallJoinSlug
    val callLinkOpen by inviteIntake.callJoinOpens.collectAsStateWithLifecycle()
    val appContext = LocalContext.current.applicationContext
    if (callSlug != null && state !is AuthState.Session) {
        me.lakko.app.ui.call.JoinCallRoot(
            authRepository = authRepository,
            slug = callSlug,
            applicationContext = appContext,
            onExit = { inviteIntake.consumeCallJoin() },
            host = me.lakko.app.calls.AppCallHost.link,
            endCurrentCall = { me.lakko.app.calls.AppCallHost.endCurrentCall() },
            linkOpen = callLinkOpen,
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

    // A fresh identity's phrase that was never confirmed, brought back after
    // a restart, is shown before the app. (Sign-up, a guest's included,
    // shows no phrase: ADR-0075 D2.)
    val parkedPhrase by authRepository.signupPhrase.collectAsState()
    val callOccupant by me.lakko.app.calls.AppCallHost.occupant.collectAsStateWithLifecycle()

    // What became of a guest account. Kept: once it has a name and a
    // password (and no call is running) the person goes into the app,
    // where an account belongs; the call-link landing would only offer the
    // call again. Deleted: the landing says so itself.
    var wasGuestHere by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(guest != null, state, parkedPhrase != null, callOccupant != null) {
        val watch = me.lakko.app.auth.observeGuestOutcome(
            wasGuest = wasGuestHere,
            guest = guest != null,
            state = state,
            signupPhrasePending = parkedPhrase != null,
            callInProgress = callOccupant != null,
        )
        wasGuestHere = watch.wasGuest
        if (watch.outcome == me.lakko.app.auth.GuestOutcome.KEPT) inviteIntake.consumeCallJoin()
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
            // ADR-0079 (amended 2026-10-26) - a tapped `/i/` link from a
            // person who is not signed in shows the inviter card first, with
            // Create an account / I already have an account. A live link stays
            // parked, so once a session lands it is opened again (live -> the
            // invite page, dead -> Contacts); a dead link is dropped.
            val anonInviteSlug by inviteIntake.pendingInviteSlug.collectAsStateWithLifecycle()
            // The parked link the visitor already chose an account for, and
            // the screen chosen (sign up or sign in).
            var answeredInviteSlug by androidx.compose.runtime.saveable.rememberSaveable {
                androidx.compose.runtime.mutableStateOf<String?>(null)
            }
            var anonStartRoute by androidx.compose.runtime.saveable.rememberSaveable {
                androidx.compose.runtime.mutableStateOf<String?>(null)
            }
            val parkedInvite = anonInviteSlug
            if (parkedInvite != null && parkedInvite != answeredInviteSlug) {
                // Back drops the link and leaves the ordinary Welcome screen.
                androidx.activity.compose.BackHandler { inviteIntake.consume() }
                val choose = { route: String, page: me.lakko.app.ui.contacts.SignedOutInviteState ->
                    anonStartRoute = route
                    if (page.keepsLinkParked) {
                        answeredInviteSlug = parkedInvite
                    } else {
                        answeredInviteSlug = null
                        inviteIntake.consume()
                    }
                }
                me.lakko.app.ui.contacts.SignedOutInviteScreen(
                    slug = parkedInvite,
                    preview = { contactsRepository.previewInviteLink(it) },
                    onCreateAccount = { page -> choose(AnonRoutes.SIGNUP, page) },
                    onHaveAccount = { page -> choose(AnonRoutes.LOGIN, page) },
                )
            } else {
                AnonRoot(
                    repository = authRepository,
                    cachedUsername = current.cachedUsername,
                    startRoute = anonStartRoute,
                )
            }
        }
        // Signed in, but this device does not hold the account's keys:
        // the restore prompt (another device, a passkey that carries the
        // keys, the 12-word phrase, or a new identity). Whatever brings
        // the keys is checked against the published identity before a
        // full session opens. NEVER derive the identity from the export
        // key or mint one silently here.
        is AuthState.NeedsRecoveryPhrase -> InsetRoot {
            RestoreIdentityRoot(
                repository = authRepository,
                account = current.account,
            )
        }
        // G-56 (UX section 8a): signed in with the account's current keys,
        // but not the earlier keys its chats use. The app shell does not
        // start; the screen offers another device and the reset, and the
        // device looks for the keys by itself.
        is AuthState.NeedsEarlierKeys -> InsetRoot {
            RestoreIdentityRoot(
                repository = authRepository,
                account = current.account,
                earlierKeysMissing = true,
            )
        }
        // ADR-0017 lost-everything fallback: the user minted a brand-new
        // identity (already published, no proof); show the new recovery
        // phrase for them to save before the session lands.
        is AuthState.SaveFreshPhrase -> InsetRoot {
            SaveFreshPhraseRoot(repository = authRepository, username = current.account.username)
        }
        is AuthState.Session -> {
            // What a live session shows is decided in one place: a phrase
            // still owed first (a fresh identity's, never confirmed; sign-up
            // shows none, ADR-0075 D2), then a guest's screens, else the app.
            val surface = me.lakko.app.auth.sessionSurface(
                signupPhrasePending = parkedPhrase != null,
                guest = guest != null,
            )
            val phraseToSave = parkedPhrase
            if (surface == me.lakko.app.auth.SessionSurface.SAVE_SIGNUP_PHRASE && phraseToSave != null) {
                InsetRoot {
                    me.lakko.app.ui.auth.SaveFreshPhraseScreen(
                        phrase = phraseToSave,
                        onConfirm = { authRepository.acknowledgeSignupPhrase() },
                        username = current.account.username,
                    )
                }
                return
            }
            // A forgot-password route just signed the person in on a device
            // that holds the keys: offer a new password before the app.
            val newPasswordOwed by authRepository.newPasswordOwed.collectAsState()
            val sessionSecrets by authRepository.identitySecrets.collectAsState()
            if (newPasswordOwed && sessionSecrets != null && authRepository.canReplacePassword()) {
                InsetRoot { me.lakko.app.ui.auth.NewPasswordPrompt(repository = authRepository) }
                return
            }
            // ADR-0073: a changed published identity is the signed-out screen
            // above or nothing; the dismissible security banner is gone.
            // ADR-0038 S4 — the "update available" notice. Shown only when the
            // verified manifest resolves to an actionable update.
            val updateDecision by releaseChecker.decision.collectAsState()
            val context = androidx.compose.ui.platform.LocalContext.current
            val shownUpdate = updateDecision as? UpdateBannerDecision.Shown
            // Everything a session's screens keep (their ViewModels, the back
            // stack) belongs to ONE account's session: see [SessionViewModels].
            SessionViewModels(accountIdB64 = current.account.accountIdB64) {
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
                guest = guest.takeIf { surface == me.lakko.app.auth.SessionSurface.GUEST },
                guestOffer = guestOffer,
                callInProgress = callOccupant != null,
                forgetDeviceSlot = forgetDeviceSlot,
                banners = if (shownUpdate == null) {
                    null
                } else {
                    {
                        shownUpdate.let { shown ->
                            UpdateAvailableBanner(
                                showUpdateButton = shown.showUpdateButton,
                                onUpdate = { releaseChecker.openStore(context) },
                                onDismiss = { releaseChecker.dismiss() },
                            )
                        }
                    }
                },
            )
            }
        }
    }

    val sessionAccount = (state as? AuthState.Session)?.account
    // ADR-0075 - the register changed or became readable: a parked write is
    // flushed, Settings is told, the automatic re-seal is evaluated.
    LaunchedEffect(authRepository, deviceGroupSync) {
        deviceGroupSync.recoveryEmail.collect {
            try {
                authRepository.onRecoveryAddressState()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                android.util.Log.w("lakko-auth", "recovery address not applied: ${cause.javaClass.simpleName}")
            }
        }
    }
    // ADR-0076 - the own-username register changed or became readable.
    LaunchedEffect(authRepository, deviceGroupSync, sessionAccount?.accountIdB64) {
        deviceGroupSync.ownUsername.collect {
            try {
                authRepository.onOwnUsernameState()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                android.util.Log.w("lakko-auth", "own username not applied: ${cause.javaClass.simpleName}")
            }
        }
    }
    if (sessionAccount != null) {
        // ADR-0075 / UX 19.1a - "Protect your account", the one step after
        // sign-up. Drawn ABOVE the session root, which keeps running
        // underneath: "Save a recovery phrase" changes the account keys and
        // needs the device list the session root opens.
        val protectOwed by authRepository.protectStepOwed.collectAsStateWithLifecycle()
        if (protectOwed) {
            androidx.compose.material3.Surface(modifier = Modifier.fillMaxSize()) {
                InsetRoot {
                    me.lakko.app.ui.auth.ProtectAccountStep(
                        repository = authRepository,
                        chatRepository = chatRepository,
                        hasUsername = sessionAccount.username.isNotBlank(),
                    )
                }
            }
        } else {
            // UX 19.2 / 19.5 / 19.6 - what happened to the account elsewhere.
            me.lakko.app.ui.auth.AccountNoticeHost(repository = authRepository, chatRepository = chatRepository)
            // A session that landed with no username and whose backup named
            // none asks for it (`auth/UsernamePrompt.kt`).
            me.lakko.app.ui.auth.UsernamePromptHost(repository = authRepository)
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
                    // A guest who is on the landing and not in the call is
                    // asked whether to keep the account.
                    guestOffer = guestOffer,
                    guestClaimedUsername = guest?.claimedUsername,
                    linkOpen = callLinkOpen,
                )
            }
            // The 1x1 call's ringing banner and in-call screen.
            OneToOneCallOverlay(
                host = callHost,
                nameForGroup = resolveOneToOneName,
                keyForGroup = { chatRepository.oneToOnePeerKey(it) },
            )
            // A call-link call that is connecting, waiting to be admitted, or
            // just ended, on screens other than its landing.
            LinkCallNoticeBar(
                host = me.lakko.app.calls.AppCallHost.link,
                onOpen = { slug -> inviteIntake.publishCallJoin(slug) },
            )
            // docs/ux/calls.md section 4.1.2 — the call this device was in was
            // ended for everyone by the person who started it.
            EndedForEveryoneNoticeBar(notice = me.lakko.app.calls.AppCallHost.endedForEveryone)
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
            endedForEveryone = me.lakko.app.calls.AppCallHost.endedForEveryone,
        )
        // docs/ux/call-links.md section 3 — the `Invite` control reads the
        // session at the moment it is pressed; it is never held for the call.
        me.lakko.app.ui.call.LinkCallOverlay(
            host = me.lakko.app.calls.AppCallHost.link,
            // The landing covers the collapsed bar's strip, so putting the call
            // away leaves it — see `LinkCallOverlay`'s parameter.
            onLeaveLanding = { inviteIntake.consumeCallJoin() },
            // `Invite contacts` lists the person's contacts and sends the
            // invitation into the one-to-one with the chosen one, through
            // that conversation's ordinary send.
            chatRepository = chatRepository,
            contactsRepository = contactsRepository,
            // Every account gets `Invite contacts`; a guest gets the link alone.
            guest = guest != null,
            endedForEveryone = me.lakko.app.calls.AppCallHost.endedForEveryone,
            identity = {
                authRepository.identitySecrets.value?.let { secrets ->
                    me.lakko.app.calls.CallInviteIdentity(
                        // ADR-0074 Stage A: a call founded for a link is an MLS
                        // group; its owner key is the conversation identity's.
                        accountIdentitySeed = me.lakko.app.auth.conversationAccountSeedOf(appContext, me.lakko.app.auth.base64Encode(secrets.accountId), secrets.accountIdentitySeed),
                        accountId = secrets.accountId,
                        sessionToken = secrets.sessionToken,
                    )
                }
            },
        )
        // docs/ux/calls.md section 4.5 — the live call, put away into one
        // small window over every screen (it replaced the bar across the top
        // of the app), and, when the app leaves the screen during a call, the
        // same picture in the system's own picture-in-picture window (4.5.4).
        // The name resolvers are the ones the call screens use, so the window
        // says exactly what the screen it came from says. Drawn last, so it
        // floats over every session screen; the call screens are their own
        // windows and are never up at the same time as it.
        val miniWindow = me.lakko.app.ui.call.rememberCallMiniWindow(
            oneToOne = callHost,
            group = me.lakko.app.calls.AppCallHost.group,
            link = me.lakko.app.calls.AppCallHost.link,
            nameForOneToOne = resolveOneToOneName,
            keyForOneToOne = { chatRepository.oneToOnePeerKey(it) },
            groupNameFor = resolveGroupName,
        )
        me.lakko.app.ui.call.CallPictureInPictureEffect(miniWindow)
        // Section 4.5.4 — the call keeps running, with its ongoing
        // notification, while Lakko is not on the screen.
        me.lakko.app.ui.call.OngoingCallServiceEffect(miniWindow)
        val inPictureInPicture by me.lakko.app.calls.CallPictureInPicture.active.collectAsStateWithLifecycle()
        if (inPictureInPicture && miniWindow != null) {
            me.lakko.app.ui.call.CallPictureInPictureContent(miniWindow)
        } else {
            me.lakko.app.ui.call.CallMiniWindowHost(
                model = miniWindow,
                onExpand = { me.lakko.app.calls.AppCallHost.expandCurrentCall() },
                onToggleMute = { on -> me.lakko.app.calls.AppCallHost.setCurrentCallMicrophoneEnabled(on) },
            )
        }
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
    rollbackAlert?.let {
        me.lakko.app.ui.auth.BackupRollbackRefusedDialog(
            onDismiss = { authRepository.dismissRollbackAlert() },
        )
    }
}

/**
 * The Forget-device orchestration of the live session root, for a caller
 * outside it (the guest's `No, thanks`, pressed on the call-link landing the
 * app root draws). The session root sets it while it is composed.
 */
internal class ForgetDeviceSlot {
    @Volatile
    var orchestration: (() -> Unit)? = null

    /** Run the session root's orchestration, or [fallback] when no session
     *  root is composed. */
    fun forget(fallback: () -> Unit) {
        (orchestration ?: fallback).invoke()
    }
}

/**
 * The ViewModel store of the signed-in session, kept in the Activity's own
 * store so it survives a configuration change exactly as the Activity's does.
 *
 * Without it the session's ViewModels outlive the session. The session root
 * builds its `NavHost` under the Activity's store, where navigation keeps one
 * store per back-stack entry until the entry is popped; signing out removes
 * the `NavHost` from the composition but pops nothing, so every chat, contact
 * and channel model of the account that left stayed alive for the rest of the
 * Activity, its `viewModelScope` running: collecting the repositories, which
 * by then hold the NEXT account, and acting on what they emit.
 */
internal class SessionViewModelStore : androidx.lifecycle.ViewModel() {
    var store: androidx.lifecycle.ViewModelStore = androidx.lifecycle.ViewModelStore()
        private set

    /** The session ended: clear its models (cancelling their scopes) and
     *  start an empty store for the next one. */
    fun endSession() {
        store.clear()
        store = androidx.lifecycle.ViewModelStore()
    }

    override fun onCleared() {
        store.clear()
    }
}

/**
 * Compose [content] as one account's session: its ViewModels live in the
 * [SessionViewModelStore] and are cleared when the session leaves the
 * composition for any reason but a configuration change (sign-out, forget
 * device, the removed-device screen, the Activity finishing). Keyed on the
 * account, so a landing for another account over a live session rebuilds the
 * session's screens rather than showing the previous account's back stack.
 */
@Composable
private fun SessionViewModels(accountIdB64: String, content: @Composable () -> Unit) {
    val holder: SessionViewModelStore = viewModel()
    val activityOwner = androidx.lifecycle.viewmodel.compose.LocalViewModelStoreOwner.current
    val activity = LocalContext.current as? android.app.Activity
    androidx.compose.runtime.key(accountIdB64) {
        val owner = remember(holder, activityOwner) {
            object :
                androidx.lifecycle.ViewModelStoreOwner,
                androidx.lifecycle.HasDefaultViewModelProviderFactory {
                private val defaults =
                    activityOwner as? androidx.lifecycle.HasDefaultViewModelProviderFactory

                override val viewModelStore: androidx.lifecycle.ViewModelStore
                    get() = holder.store

                override val defaultViewModelProviderFactory: androidx.lifecycle.ViewModelProvider.Factory
                    get() = defaults?.defaultViewModelProviderFactory
                        ?: androidx.lifecycle.ViewModelProvider.NewInstanceFactory()

                override val defaultViewModelCreationExtras: androidx.lifecycle.viewmodel.CreationExtras
                    get() = defaults?.defaultViewModelCreationExtras
                        ?: androidx.lifecycle.viewmodel.CreationExtras.Empty
            }
        }
        // Declared before the content, so it is disposed after it: the
        // screens are gone when their models are cleared.
        DisposableEffect(holder) {
            onDispose {
                if (activity?.isChangingConfigurations != true) holder.endSession()
            }
        }
        androidx.compose.runtime.CompositionLocalProvider(
            androidx.lifecycle.viewmodel.compose.LocalViewModelStoreOwner provides owner,
            content = content,
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
    /** G-56: the variant of UX section 8a (the earlier keys are missing). */
    earlierKeysMissing: Boolean = false,
) {
    if (earlierKeysMissing) {
        // UX 8a: the device looks for the earlier keys by itself at entry
        // (every start comes back here) and every few minutes while the
        // screen is in the foreground. A failed look is not shown.
        val lifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
        LaunchedEffect(account.accountIdB64, lifecycle) {
            lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.RESUMED) {
                while (true) {
                    try {
                        repository.lookForEarlierKeysNow()
                    } catch (cause: Throwable) {
                        if (cause is kotlinx.coroutines.CancellationException) throw cause
                        android.util.Log.w("lakko-auth", "G-56: look for the earlier keys failed (${cause.javaClass.simpleName})")
                    }
                    kotlinx.coroutines.delay(me.lakko.app.auth.EARLIER_KEYS_RETRY_MS)
                }
            }
        }
    }
    // Keyed on the account: the model holds what was typed for it, and this
    // root's store lives as long as the Activity.
    val vm: me.lakko.app.ui.auth.RestoreIdentityPromptViewModel = viewModel(
        key = "restore-identity-${account.accountIdB64}",
        factory = singletonViewModelFactory {
            me.lakko.app.ui.auth.RestoreIdentityPromptViewModel(repo = repository)
        },
    )
    val restoreNotice by repository.restoreNotice.collectAsState()
    me.lakko.app.ui.auth.RestoreIdentityPrompt(
        viewModel = vm,
        username = account.username,
        accountIdB64 = account.accountIdB64,
        onSignOut = { repository.logout() },
        restoreNotice = if (earlierKeysMissing) null else restoreNotice,
        earlierKeysMissing = earlierKeysMissing,
    )
}

/**
 * ADR-0017 lost-everything fallback — renders the freshly-minted recovery
 * phrase for the user to save, then finishes the session.
 */
@Composable
private fun SaveFreshPhraseRoot(
    repository: AuthRepository,
    username: String,
) {
    val phrase = repository.freshPhrase() ?: ""
    // ADR-0073: while the new keys of the reset are not published to the
    // directory and the device list the words are held back and the screen
    // offers "Try again" (or a sign-in, for a reset older than 30 days).
    val publishOwed by repository.freshPublishOwed.collectAsState()
    // ADR-0075 - new words are shown only to a phrase-protected account. For
    // the others the reset or the recall lands without a phrase step once
    // its publish is done (while it is owed, the retry screen stays).
    if (publishOwed == null && !repository.showsNewWordsHere()) {
        LaunchedEffect(Unit) {
            try {
                repository.confirmFreshPhraseSaved()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                android.util.Log.w("lakko-auth", "the new keys' session did not land: ${cause.javaClass.simpleName}")
            }
        }
        LoadingScreen()
        return
    }
    me.lakko.app.ui.auth.SaveFreshPhraseScreen(
        phrase = phrase,
        onConfirm = { repository.confirmFreshPhraseSaved() },
        username = username,
        publishOwed = publishOwed,
        onRetryPublish = { repository.retryFreshPublish() },
        onLogOut = { repository.logout() },
        wordless = !repository.showsNewWordsHere(),
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
    /**
     * `docs/ux/call-links.md` section 9 — the session is a guest account's
     * (`auth/GuestAccount.kt`), or `null`. The session runs as any other (the
     * call it was made for needs it), but the app shell is never drawn for
     * it: in its place the guest gets the way back to the call, or the
     * question whether to keep the account ([guestOffer]).
     */
    guest: me.lakko.app.auth.GuestSession?,
    guestOffer: me.lakko.app.ui.auth.GuestAccountOfferViewModel?,
    /** Any call is ringing, starting or live. */
    callInProgress: Boolean,
    /** Where this root publishes its Forget-device orchestration. */
    forgetDeviceSlot: ForgetDeviceSlot,
    /** The security / update banners, drawn above the tab content. */
    banners: (@Composable () -> Unit)?,
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
        try {
            chatRepository.bootstrap(secrets)
        } catch (missing: me.lakko.app.auth.EarlierKeysMissing) {
            // G-56 defence in depth: the chat layer minted nothing for a
            // device whose account's earlier keys are not here; park it.
            authRepository.parkAfterChatRefusal(me.lakko.app.auth.base64Encode(secrets.accountId))
            return@LaunchedEffect
        }
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
        val profileRestored = authRepository.takeRestoredProfileOwed(me.lakko.app.auth.base64Encode(secrets.accountId))
        if (profileChanged || profileRestored) chatRepository.publishOwnProfileToAllGroups()
        if (profileRestored) me.lakko.app.reach.ReachManager.get(chatRepository.context).publishOwnProfileToDevices()
        // Phase 2a3e2b — bootstrap the cross-device sync
        // runtime alongside the chat bootstrap so the poll
        // loop starts at the same moment chat surfaces become
        // usable. Idempotent for the same account id.
        // ADR-0098 step 4 — this account is never marked suspended by the
        // marks other people get (its own standing is the suspended screen).
        // Bound at every landing; sign-out and forget device clear it with
        // the kept standings (`DeviceGroupSyncRepository.clear`).
        me.lakko.app.moderation.SuspensionMarks.bindOwnAccountKey { chatRepository.ownPersonKeyB64() }
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
            deviceGroupSync.bootstrap(secrets)
        }
        // ADR-0074 section 2.3 — a device removal that still owes steps (the
        // directory row, the device list, a chat that had not answered) is
        // retried at every landing until each step has an answer.
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            try {
                me.lakko.app.devices.DeviceRemovalRunner
                    .onDevice(chatRepository.context, authRepository, chatRepository)
                    ?.resume()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                android.util.Log.w("lakko-auth", "owed device removal not resumed: ${cause.javaClass.simpleName}")
            }
        }
        // ADR-0074 part 3 - a key rotation that passed its commit finishes
        // its publishing steps by itself at every landing.
        kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            try {
                me.lakko.app.devices.KeyRotationDevice
                    .runner(chatRepository.context, authRepository, chatRepository)
                    ?.resume()
            } catch (cause: Throwable) {
                if (cause is kotlinx.coroutines.CancellationException) throw cause
                android.util.Log.w("lakko-auth", "owed key rotation not resumed: ${cause.javaClass.simpleName}")
            }
        }
    }

    // ADR-0074 section 0.4 — a session that was bound to this device after
    // it was installed could not publish this device's KeyPackage before the
    // bind (`403 device-mismatch`): publish it now.
    // ADR-0074 part 3 - a `key-rotation` envelope read from the account's
    // own DeviceGroup goes to the auth side (verified, stored as pending).
    DisposableEffect(authRepository) {
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.keyRotationSink = { envelope ->
            authRepository.onKeyRotationEnvelope(envelope)
        }
        onDispose { me.lakko.app.devices.sync.DeviceGroupSyncRepository.keyRotationSink = null }
    }
    // ADR-0081 decision 4 - a device that lost or fell behind its DeviceGroup
    // knocks under this account's login handle, after its own directory
    // KeyPackage (the signed device binding a sibling checks) is published;
    // a sibling admits only knocks naming the handle.
    DisposableEffect(authRepository, chatRepository) {
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.loginHandleProvider = {
            val session = authRepository.state.value as? me.lakko.app.auth.AuthState.Session
            session?.account?.username?.takeIf { it.isNotBlank() }?.let { name ->
                runCatching { uniffi.lakko_ffi_uniffi.loginHandle(name) }.getOrNull()
            }
        }
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.directoryKeyPackagePublisher = {
            chatRepository.republishKeyPackage()
        }
        onDispose {
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.loginHandleProvider = null
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.directoryKeyPackagePublisher = null
        }
    }
    // This device adopted rotated account keys (as the device that changed
    // them, or as a kept device): every directory row of the account was
    // deleted by the rotation, so this device's KeyPackage is published
    // again under the same device id, and the DeviceGroup journal's root
    // signer follows the new seed.
    val keysAdoptedRevision by authRepository.keysAdoptedRevision.collectAsStateWithLifecycle()
    LaunchedEffect(keysAdoptedRevision) {
        if (keysAdoptedRevision > 0) {
            authRepository.identitySecrets.value?.let { deviceGroupSync.rebindRoot(it.accountIdentitySeed) }
            chatRepository.requestKeyPackageRepublish()
        }
    }

    val deviceBoundRevision by authRepository.deviceBoundRevision.collectAsStateWithLifecycle()
    LaunchedEffect(deviceBoundRevision) {
        if (deviceBoundRevision > 0) chatRepository.requestKeyPackageRepublish()
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
    // Not for a guest: a person who was sent a call link is not asked whether
    // this app may send notifications. The ask comes once they keep the
    // account (the key changes when the guest flag drops).
    LaunchedEffect(identitySecrets, guest != null) {
        if (identitySecrets == null || guest != null) return@LaunchedEffect
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
    // ADR-0087 section 15.2 — a co-publisher link opened from the /g/ page.
    var pendingCoPublisherLink by remember { mutableStateOf<String?>(null) }
    val channelLinkScope = androidx.compose.runtime.rememberCoroutineScope()
    val appContextForLinks = LocalContext.current.applicationContext
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
    // ADR-0087 section 9.5 - this account left a channel on another device: it
    // goes here too, with the same sentence a group's end shows.
    LaunchedEffect(channelsState) {
        me.lakko.app.chat.ConversationEndsMirror.registers.collect { heard ->
            val ended = heard?.ends.orEmpty()
            if (ended.none { it.channel }) return@collect
            val handles = channelsState.channels.value
            val leaving = me.lakko.app.channels.ChannelUnsubscribe.channelsToLeave(
                ended = ended,
                followed = channelsState.subscriberKeysByChannel.value.keys,
                published = channelsState.keysByChannel.value.keys,
            )
            for (id in leaving) {
                val handle = handles.firstOrNull { it.id.equals(id, ignoreCase = true) } ?: continue
                me.lakko.app.channels.ChannelUnsubscribe.leave(
                    channelsState,
                    handle.id,
                    channelsClient.baseUrl,
                    tellDevices = false,
                )
                chatRepository.noteChannelEndHeard(handle.id, handle.sharedName.ifBlank { handle.localName })
            }
        }
    }
    // docs/ux/second-device.md section 4a - the account's channels travel
    // between its devices through the channel subscription register: a
    // channel subscribed, created or co-published here is written to it, one
    // written on another device is added here (the shared subscriber key and
    // the read key installed), and one left there goes here too.
    val channelSyncContext = LocalContext.current.applicationContext
    LaunchedEffect(channelsState, deviceGroupSync) {
        me.lakko.app.channels.ChannelSubscriptionSync.appContext = channelSyncContext
        val port = me.lakko.app.channels.SyncChannelRegisterPort(deviceGroupSync)
        kotlinx.coroutines.flow.combine(
            channelsState.channels,
            channelsState.subscriberKeysByChannel,
            channelsState.keysByChannel,
            channelsState.publisherKeysReady,
            kotlinx.coroutines.flow.combine(
                deviceGroupSync.channelSubscriptions,
                me.lakko.app.channels.ChannelSubscriptionSync.readKeysVersion,
            ) { registers, version -> registers to version },
        ) { _, _, _, ready, regs -> ready to regs }
            .let { me.lakko.app.channels.debounced(it, 300L) }
            .collect { me.lakko.app.channels.ChannelSubscriptionSync.reconcile(channelsState, port) }
    }
    // docs/ux/second-device.md section 4a - the pairing state bundle (v4)
    // carries the newest seeds of each channel the register lists as live;
    // a received bundle adds seeds this device holds none of for that epoch.
    DisposableEffect(channelsState, deviceGroupSync) {
        fun sameAccount(acct: String): Boolean = channelsState.boundAccount?.let {
            me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(it) ==
                me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(acct)
        } == true
        me.lakko.app.devices.StateBundle.channelSeedSource = { acct ->
            if (!sameAccount(acct)) {
                emptyMap()
            } else {
                deviceGroupSync.channelSubscriptions.value.orEmpty()
                    .filter { it.subscribed }
                    .associate { r ->
                        val id = me.lakko.app.channels.ChannelsComposer.bytesToHex(r.channelId)
                        id to channelsState.heldEpochSeeds(id)
                    }
                    .filterValues { it.isNotEmpty() }
            }
        }
        me.lakko.app.devices.StateBundle.channelSeedSink = { acct, seeds ->
            if (sameAccount(acct)) channelsState.addCarriedSeeds(seeds) else null
        }
        // The account's reader key a rotation on this device always reseals to.
        me.lakko.app.channels.ChannelReseal.accountReaderKeyOf = { acct, channelId ->
            if (!sameAccount(acct)) {
                null
            } else {
                channelsState.subscriberKeysByChannel.value[channelId]
                    ?.let { me.lakko.app.channels.ChannelsComposer.bytesToHex(it.publicKey) }
            }
        }
        // ADR-0090 - PublisherGroups live on this device's bound identity,
        // in the chat repository's provider.
        me.lakko.app.channels.ChannelPublisherIdentity.devicePort = chatRepository.channelDevicePort
        // ADR-0090 open question 4 - the owner account's admission seeds
        // ride the account's synced register.
        me.lakko.app.channels.ChannelAdmissionKeys.register = { id ->
            deviceGroupSync.channelAdmissionSeeds(me.lakko.app.channels.ChannelsComposer.channelIdFromHex(id))
        }
        me.lakko.app.channels.ChannelAdmissionKeys.writeRegister = { id, version, seed ->
            deviceGroupSync.setChannelAdmissionSeed(me.lakko.app.channels.ChannelsComposer.channelIdFromHex(id), version, seed)
        }
        onDispose {
            me.lakko.app.channels.ChannelPublisherIdentity.devicePort = null
            me.lakko.app.channels.ChannelAdmissionKeys.register = null
            me.lakko.app.channels.ChannelAdmissionKeys.writeRegister = null
            me.lakko.app.channels.ChannelReseal.accountReaderKeyOf = null
            me.lakko.app.devices.StateBundle.channelSeedSource = null
            me.lakko.app.devices.StateBundle.channelSeedSink = null
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
                // ADR-0074 Stage A: a channel's PublisherGroup is an MLS
                // group founded under the account identity; after a rotation
                // it stays under the predecessor (no leaf is moved).
                accountIdentitySeed = me.lakko.app.auth.conversationAccountSeedOf(chatRepository.context, me.lakko.app.auth.base64Encode(secrets.accountId), secrets.accountIdentitySeed),
                // Owner requests follow the CURRENT key (section 3.5,
                // "Channels"): the owner key is handed over, the group is not.
                currentAccountSeed = secrets.accountIdentitySeed,
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
    val sweepContext = androidx.compose.ui.platform.LocalContext.current.applicationContext
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
                        // 2026-11-06 — one read of every group per sweep, no
                        // long polls held open; the pause below decides how
                        // often (`ReceiveCadence`).
                        chatRepository.feedRefresh.reporting(sweepAccount, me.lakko.app.feed.CheckKind.GROUPS) {
                            chatRepository.pollAllGroups(
                                waitSecs = 0,
                                exclude = chatRepository.screenPolledGroups.value,
                            )
                        }
                    } catch (cancel: kotlinx.coroutines.CancellationException) {
                        // The repository's session ending under one pass is
                        // not this loop ending.
                        endPassUnlessCancelled(cancel)
                    } catch (_: Throwable) {
                        kotlinx.coroutines.delay(APP_POLL_RETRY_BACKOFF_MS)
                    }
                    // The pause before the next sweep: short while a push
                    // wake's window is open or a call is up, long otherwise; a
                    // new wake ends it early.
                    val elapsed = android.os.SystemClock.elapsedRealtime() - startedAt
                    val pause = me.lakko.app.chat.ReceiveCadence.intervalMs(
                        wakeWindowOpen = me.lakko.app.chat.ReceiveCadence.wakeWindowOpen(),
                        callInProgress = me.lakko.app.calls.AppCallHost.occupantNow() != null,
                        pushReachable = me.lakko.app.push.PushDeviceTokens(sweepContext).fcm != null,
                    )
                    val wakeBefore = me.lakko.app.chat.ReceiveCadence.lastWakeAtMs.value
                    kotlinx.coroutines.withTimeoutOrNull((pause - elapsed).coerceAtLeast(APP_POLL_MIN_ITERATION_MS)) {
                        me.lakko.app.chat.ReceiveCadence.lastWakeAtMs.first { it > wakeBefore }
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
    // `backup/AutoBackup.kt` for what counts as a change. The backup carries
    // contacts, the synced state and the conversations' ids - no MLS state.
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
                    // ADR-0058 section 5.2 — the folded synced state (folders,
                    // pins, mutes, archive, read cursors, drafts, order).
                    syncedState = { deviceGroupSync.currentSyncedStateBytes() },
                    // ADR-0074 section 3.7 — the keys the conversations stay
                    // under, for a device restored from the phrase.
                    ownPredecessors = { accountIdB64 -> authRepository.backupOwnPredecessors(accountIdB64) },
                    personal = { accountIdB64 ->
                        me.lakko.app.backup.BackupPersonal.of(backupContext, accountIdB64)
                    },
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
                    endPassUnlessCancelled(cancel)
                } catch (cause: Throwable) {
                    // A failure outside the inbox's own error handling stopped
                    // this pass: said once per pass, by kind only.
                    android.util.Log.w("MainActivity", "inbox pass failed: ${cause.javaClass.simpleName}")
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
                        // The repository's session ending under one pass is
                        // not this loop ending.
                        endPassUnlessCancelled(cancel)
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
    // docs/ux/identity-recovery.md 17.5 - the new-device notification was
    // tapped: Settings > Devices.
    val openDevicesRequested by me.lakko.app.devices.NewDeviceNotices.openDevicesRequested
        .collectAsStateWithLifecycle()
    LaunchedEffect(openDevicesRequested) {
        if (!openDevicesRequested) return@LaunchedEffect
        // docs/ux/profile-screen.md §6 — a link leaving Settings > Profile
        // with unsaved changes asks first.
        me.lakko.app.ui.profile.ProfileLeaveGuard.run {
            navController.navigate(Routes.DEVICES) { launchSingleTop = true }
            me.lakko.app.devices.NewDeviceNotices.consumeOpenDevices()
        }
    }

    //
    // Not while the session is a guest's: the tabs are not drawn then, so
    // there is no graph to navigate. The link waits; it is followed if the
    // guest keeps the account (the key changes), and it goes with the session
    // if they do not. The same holds for the three effects further down.
    val guestShell = guest != null
    val pendingInviteSlug by inviteIntake.pendingInviteSlug.collectAsStateWithLifecycle()
    LaunchedEffect(pendingInviteSlug, guestShell) {
        if (guestShell) return@LaunchedEffect
        val slug = pendingInviteSlug ?: return@LaunchedEffect
        me.lakko.app.ui.profile.ProfileLeaveGuard.run {
            navController.navigate(Routes.invite(slug)) { launchSingleTop = true }
            inviteIntake.consume()
        }
    }

    // ADR-0063 section 4 — channels join the foreground poll: while the app is
    // started, the posts of every readable channel are filed into the
    // channel-post log the feed ranks. At once on start (and on every return
    // to the foreground), then every minute, and at once after this device
    // publishes a post.
    // docs/ux/channel-notifications.md — every channel read the sync makes
    // goes to the notifier: the shared rule decides which post notifies.
    DisposableEffect(channelsState, deviceGroupSync) {
        val notifier = me.lakko.app.feed.ChannelPostNotifier(
            me.lakko.app.feed.ChannelNotifiedStore.on(backupContext),
        ) { me.lakko.app.feed.ChannelNotificationPoster.show(backupContext, it) }
        val off = me.lakko.app.feed.subscribeChannelReads { account, reads ->
            // Only the reads of the account this state is bound to.
            if (channelsState.boundAccount != account) return@subscribeChannelReads
            notifier.onReads(
                account,
                reads,
                me.lakko.app.feed.liveChannelFacts(channelsState) { id ->
                    me.lakko.app.channels.ChannelFlags.of(deviceGroupSync.flags.value, id)?.muted == true
                },
            )
        }
        onDispose { off() }
    }
    val channelPostSync = remember(channelsState, channelsClient) {
        me.lakko.app.feed.ChannelPostSync(backupContext, channelsState, channelsClient)
    }
    // docs/ux/channel-notifications.md section 3 — the periodic background
    // read while Lakko is not in front (idempotent: a schedule is kept).
    LaunchedEffect(identitySecrets) {
        if (identitySecrets == null) return@LaunchedEffect
        me.lakko.app.feed.ChannelPostWorker.schedule(backupContext)
    }
    // A tapped channel post notification opens the channel.
    val openChannelRequested by me.lakko.app.feed.ChannelNotificationIntake.openRequested
        .collectAsStateWithLifecycle()
    LaunchedEffect(openChannelRequested, guestShell) {
        if (guestShell) return@LaunchedEffect
        val id = openChannelRequested ?: return@LaunchedEffect
        me.lakko.app.ui.profile.ProfileLeaveGuard.run {
            navController.navigate(Routes.channel(id)) { launchSingleTop = true }
            me.lakko.app.feed.ChannelNotificationIntake.consume(id)
        }
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

    // ADR-0065 section 3 — the keep tick, in the foreground only: every 10
    // minutes the owed blob deletes are retried and at most four due buckets
    // (one conversation's pinned files, the saved list, the own photo) is
    // kept; each bucket comes due about once a month, spread over time.
    LaunchedEffect(lifecycleOwner, identitySecrets) {
        val secrets = identitySecrets ?: return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            // The first run a minute after the landing (or the return to the
            // foreground), then every ten minutes: the same on every client.
            kotlinx.coroutines.delay(KEEP_FIRST_TICK_MS)
            while (true) {
                try {
                    val own = me.lakko.app.profile.OwnProfileStore.load(chatRepository.context, secrets.accountId)?.avatar
                    val buckets = chatRepository.pinnedKeepBuckets() +
                        me.lakko.app.storage.KeepBucket("saved", me.lakko.app.storage.blobHexes(deviceGroupSync.savedBlobIds())) +
                        me.lakko.app.storage.KeepBucket("profile", me.lakko.app.storage.blobHexes(me.lakko.app.profile.avatarBlobIds(own))) +
                        // ADR-0087 18.5 — the photos of the groups this device
                        // owns or administers, with no control or message.
                        me.lakko.app.storage.KeepBucket(
                            "conversation-photos",
                            me.lakko.app.storage.blobHexes(chatRepository.conversationPhotoBlobIds()),
                        )
                    me.lakko.app.storage.BlobLifetime.tick(chatRepository.context, buckets)
                } catch (cause: Throwable) {
                    if (cause is kotlinx.coroutines.CancellationException) throw cause
                    android.util.Log.w("MainActivity", "keep tick failed: ${cause.javaClass.simpleName}")
                }
                kotlinx.coroutines.delay(KEEP_TICK_MS)
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
    me.lakko.app.channels.SubscriberAdmission.appContext = LocalContext.current.applicationContext
    me.lakko.app.channels.ChannelRemovalWatch.notice = { id, name -> chatRepository.noteChannelEndHeard(id, name) }
    // ADR-0087 section 14.1 — the channel invitation card in a chat.
    val inviteCardContext = LocalContext.current.applicationContext
    androidx.compose.runtime.DisposableEffect(channelsState, chatRepository) {
        me.lakko.app.ui.channels.ChannelInviteActionsHolder.actions = object : me.lakko.app.ui.channels.ChannelInviteActions {
            override suspend fun inspect(link: me.lakko.app.contacts.LakkoLink.Group): me.lakko.app.ui.channels.ChannelInviteFacts? {
                val sealed = chatRepository.resolveGroupInviteSlug(link.slug).first
                val card = chatRepository.inspectGroupInvite(
                    sealed,
                    me.lakko.app.chat.parseGroupInviteFragment(link.fragment),
                    System.currentTimeMillis().toULong(),
                )
                val preview = card.preview ?: return null
                val id = preview.channelId ?: return null
                if (!card.valid) return null
                return me.lakko.app.ui.channels.ChannelInviteFacts(
                    channelIdHex = me.lakko.app.channels.ChannelsComposer.bytesToHex(id),
                    // A nameless invitation reads by the name this device
                    // holds for the channel, else the catalog word, never "?".
                    name = me.lakko.app.ui.channels.inviteChannelName(
                        preview.groupName,
                        me.lakko.app.channels.ChannelsComposer.bytesToHex(id),
                    ),
                    preview = preview,
                )
            }

            // A bare `/g/` link with no card (`BareGroupLink.kt`): which of the
            // four it is, exactly (a gone link is not a group's live one).
            override suspend fun openBareLink(
                link: me.lakko.app.contacts.LakkoLink.Group,
            ): me.lakko.app.ui.channels.BareLinkOpening {
                val sealed = try {
                    chatRepository.resolveGroupInviteSlug(link.slug).first
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    return if (me.lakko.app.ui.channels.inviteLinkUnreachable(e)) {
                        me.lakko.app.ui.channels.BareLinkOpening.Offline
                    } else {
                        me.lakko.app.ui.channels.BareLinkOpening.Dead
                    }
                }
                val card = try {
                    chatRepository.inspectGroupInvite(
                        sealed,
                        me.lakko.app.chat.parseGroupInviteFragment(link.fragment),
                        System.currentTimeMillis().toULong(),
                    )
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    return me.lakko.app.ui.channels.BareLinkOpening.Dead
                }
                if (!card.valid) return me.lakko.app.ui.channels.BareLinkOpening.Dead
                val preview = card.preview ?: return me.lakko.app.ui.channels.BareLinkOpening.NotChannel
                val channelId = preview.channelId ?: return me.lakko.app.ui.channels.BareLinkOpening.NotChannel
                val name = me.lakko.app.ui.channels.inviteChannelName(
                    preview.groupName,
                    me.lakko.app.channels.ChannelsComposer.bytesToHex(channelId),
                )
                return me.lakko.app.ui.channels.BareLinkOpening.Channel(
                    me.lakko.app.ui.channels.ChannelInviteSummary(name, preview.coPublisher),
                )
            }

            override fun heldChannelName(channelIdHex: String): String? =
                channelsState.channels.value.firstOrNull { it.id.equals(channelIdHex, ignoreCase = true) }
                    ?.let { me.lakko.app.ui.channels.channelTitle(it, me.lakko.app.l10n.Localization.strings.value) }

            override fun isFollowed(channelIdHex: String): Boolean =
                channelsState.channels.value.any { it.id == channelIdHex }

            override fun isPublisherOf(channelIdHex: String): Boolean =
                channelsState.keysByChannel.value.containsKey(channelIdHex)

            override fun publisherRequestSentAtMs(channelIdHex: String): Long? =
                channelsState.boundAccount?.let { acct ->
                    me.lakko.app.channels.PendingPublisherRequests.sentAtMs(inviteCardContext, acct, channelIdHex)
                }

            override suspend fun subscribe(facts: me.lakko.app.ui.channels.ChannelInviteFacts) {
                val invite = facts.preview.subscriber ?: return
                val idHex = me.lakko.app.channels.ChannelInviteLinks.subscribeFromLink(
                    inviteCardContext,
                    channelsState,
                    channelsClient,
                    me.lakko.app.channels.ChannelsClient.channelIdBytes(facts.channelIdHex),
                    facts.name,
                    invite,
                )
                navController.navigate(
                    Routes.channelSubscribe(idHex, null, pressed = facts.public, name = facts.name),
                ) { launchSingleTop = true }
            }

            override fun openChannel(channelIdHex: String) {
                navController.navigate(Routes.channel(channelIdHex)) { launchSingleTop = true }
            }

            override suspend fun acceptPublisher(
                facts: me.lakko.app.ui.channels.ChannelInviteFacts,
                link: me.lakko.app.contacts.LakkoLink.Group,
                onStatus: (me.lakko.app.l10n.UiText?) -> Unit,
            ): String? {
                return me.lakko.app.ds.DsClient().use { ds ->
                    me.lakko.app.ui.channels.joinAsCoPublisher(
                        inviteCardContext,
                        channelsState,
                        ds,
                        me.lakko.app.ui.channels.coPublisherJoinUrl(link),
                        facts.name,
                        me.lakko.app.channels.CoPublishedJournals.onDevice(inviteCardContext) { channelsState.boundAccount },
                        onStatus,
                    )
                }
            }
        }
        // ADR-0087 section 15, ADR-0045 decision 2 — a publisher request has
        // no deadline: the account mailbox loop lands the owner's Welcome
        // whenever it arrives, and a deleted channel's request goes.
        me.lakko.app.channels.PendingPublisherRequests.lander = { welcome ->
            me.lakko.app.ui.channels.landPendingPublisherWelcome(
                inviteCardContext,
                channelsState,
                me.lakko.app.channels.CoPublishedJournals.onDevice(inviteCardContext) { channelsState.boundAccount },
                welcome,
            )
        }
        me.lakko.app.channels.ChannelsClient.onChannelDeleted = { id ->
            channelsState.boundAccount?.let { acct ->
                me.lakko.app.channels.PendingPublisherRequests.drop(inviteCardContext, acct, id)
            }
        }
        onDispose {
            me.lakko.app.ui.channels.ChannelInviteActionsHolder.actions = null
            me.lakko.app.channels.PendingPublisherRequests.lander = null
            me.lakko.app.channels.ChannelsClient.onChannelDeleted = null
        }
    }
    // ADR-0087 section 9.5 — a removal from a private channel is noticed
    // app-wide, not only with the channel open: every few minutes each
    // channel this device only reads asks whether its key is still let in.
    val followedChannels by channelsState.subscriberKeysByChannel.collectAsStateWithLifecycle()
    LaunchedEffect(lifecycleOwner, identitySecrets, followedChannels.keys) {
        if (identitySecrets == null || followedChannels.isEmpty()) return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            while (true) {
                if (channelsState.boundAccount != null) {
                    try {
                        me.lakko.app.channels.ChannelRemovalWatch.checkAll(channelsState, channelsClient) { id, name ->
                            chatRepository.noteChannelEndHeard(id, name)
                        }
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (_: Throwable) {
                        // The next pass asks again.
                    }
                }
                kotlinx.coroutines.delay(5L * 60L * 1_000L)
            }
        }
    }
    // ADR-0087 section 12 / plan step 9 — the rotation heartbeat, the
    // co-publisher requests, owed handle and record writes and a public
    // channel's seed per epoch run here, app-wide, not on a screen.
    val publisherLoopContext = LocalContext.current.applicationContext
    val publisherLoop = remember(channelsState, channelsClient) {
        me.lakko.app.channels.ChannelPublisherLoop.forChannels(publisherLoopContext, channelsState, channelsClient)
    }
    // ADR-0090 — a device of an account that owns or co-publishes a channel
    // runs the loop even before it holds a PublisherGroup: it adopts one an
    // owner device admitted it to, and re-founds when it is the only device.
    val registeredPublisherChannels by deviceGroupSync.channelSubscriptions.collectAsStateWithLifecycle()
    val publishesSomewhere = registeredPublisherChannels.orEmpty().any {
        it.subscribed && it.role != uniffi.lakko_ffi_uniffi.ChannelSubscriptionRole.SUBSCRIBER
    }
    LaunchedEffect(lifecycleOwner, subscriberAdmission, identitySecrets, publisherChannels.keys, publishesSomewhere) {
        if (identitySecrets == null || (publisherChannels.isEmpty() && !publishesSomewhere)) return@LaunchedEffect
        lifecycleOwner.lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.STARTED) {
            var lastAdmitMs = 0L
            while (true) {
                if (channelsState.boundAccount != null) {
                    // Section 6 — a public channel's readers open its public
                    // seed; only private channels are let in one by one,
                    // once a pass (a new private subscriber waits up to it).
                    val now = android.os.SystemClock.elapsedRealtime()
                    if (now - lastAdmitMs >= PRIVATE_ADMIT_INTERVAL_MS) {
                        lastAdmitMs = now
                        val privateOnes = channelsState.keysByChannel.value.keys.filter { id ->
                            channelsState.channels.value.firstOrNull { it.id == id }?.sharedListed != true
                        }
                        subscriberAdmission.admitAll(privateOnes)
                    }
                    publisherLoop.passAll()
                }
                // ADR-0090 section 2.1 — the next pass comes at the interval,
                // or at once when the account's DeviceGroup grows or shrinks.
                val roster = me.lakko.app.devices.DeviceRosterChanges.revision.value
                kotlinx.coroutines.withTimeoutOrNull(me.lakko.app.channels.SubscriberAdmission.APP_INTERVAL_MS) {
                    me.lakko.app.devices.DeviceRosterChanges.revision.first { it != roster }
                }
            }
        }
    }

    // A tapped shared-feed link pushes the import screen.
    val pendingFeedImport by me.lakko.app.ui.feed.FeedImportIntake.pending.collectAsStateWithLifecycle()
    val pendingFeedFile by me.lakko.app.ui.feed.FeedImportIntake.pendingFile.collectAsStateWithLifecycle()
    LaunchedEffect(pendingFeedImport, pendingFeedFile, guestShell) {
        if (guestShell) return@LaunchedEffect
        if (pendingFeedImport == null && pendingFeedFile == null) return@LaunchedEffect
        me.lakko.app.ui.profile.ProfileLeaveGuard.run { navController.navigate(Routes.FEED_IMPORT) { launchSingleTop = true } }
    }

    // ADR-0028 (G-3) — a tapped `/g/<slug>` group join-ticket link pushes the
    // group-join landing the same way.
    val pendingGroupJoin by inviteIntake.pendingGroupJoin.collectAsStateWithLifecycle()
    LaunchedEffect(pendingGroupJoin, guestShell) {
        if (guestShell) return@LaunchedEffect
        val link = pendingGroupJoin ?: return@LaunchedEffect
        me.lakko.app.ui.profile.ProfileLeaveGuard.run {
            navController.navigate(Routes.groupJoin(link)) { launchSingleTop = true }
            inviteIntake.consumeGroupJoin()
        }
    }

    // A notification tap (Phase 1.20d2) opens the matching chat when the group
    // is known locally; a stale wake lands on the list its kind belongs to.
    val pendingWake by pushIntake.pendingWake.collectAsStateWithLifecycle()
    val chatState by chatRepository.state.collectAsStateWithLifecycle()
    LaunchedEffect(pendingWake, chatState.bootstrapped, guestShell) {
        if (guestShell) return@LaunchedEffect
        val wake = pendingWake ?: return@LaunchedEffect
        if (!chatState.bootstrapped) return@LaunchedEffect
        val knownThread = chatState.threads.firstOrNull {
            it.groupIdHex.equals(wake.groupHex, ignoreCase = true)
        }
        me.lakko.app.ui.profile.ProfileLeaveGuard.run {
            if (knownThread != null) {
                navController.navigate(Routes.chat(knownThread.groupIdHex)) { launchSingleTop = true }
            } else {
                navController.navigateToTab(Routes.CHATS)
            }
            pushIntake.consume()
        }
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
        me.lakko.app.channels.ChannelPublisherLoop.peek(channelsState)?.clear()
        // Before the chat side: its poll hands the reach manager envelopes
        // from the account's other devices, and one handed over after the
        // reach manager let the account go would wait there for the next
        // account to bind.
        deviceGroupSync.clear()
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
    val forgetDeviceNowBody = {
        // ADR-0042, amended 2026-09-30 — first, as in logout above.
        me.lakko.app.push.PushSignOut.clearFirst(appContext)
        me.lakko.app.calls.AppCallHost.reset()
        contactsRepository.clear()
        channelsState.unbind()
        subscriberAdmission.clear()
        me.lakko.app.channels.ChannelPublisherLoop.peek(channelsState)?.clear()
        // Device sync before chat, as in logout above.
        deviceGroupSync.clear()
        chatRepository.forgetDevice()
        authRepository.forgetDevice()
    }
    val forgetDevice: () -> Unit = {
        // ADR-0090 section 2.4 — before anything stops: this device asks an
        // owner device to retire its leaf in every channel it publishes
        // (best effort, bounded; if lost, the account's other device retires
        // it when the DeviceGroup no longer lists this one).
        val publisherLoop = me.lakko.app.channels.ChannelPublisherLoop.peek(channelsState)
        channelLinkScope.launch {
            kotlinx.coroutines.withTimeoutOrNull(RETIRE_BEFORE_FORGET_MS) {
                try {
                    publisherLoop?.retireOwnLeavesBestEffort()
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    // Best effort.
                }
            }
            forgetDeviceNowBody()
        }
        Unit
    }

    // `No, thanks` on the guest offer reaches the orchestration above from
    // the app root, where the call-link landing is drawn.
    val currentForgetDevice by rememberUpdatedState(forgetDevice)
    DisposableEffect(forgetDeviceSlot) {
        forgetDeviceSlot.orchestration = { currentForgetDevice() }
        onDispose { forgetDeviceSlot.orchestration = null }
    }

    // ADR-0074 section 2.3 / docs/ux/identity-recovery.md 18.2 — another
    // device removed this one, and this device read its own removal from the
    // account's DeviceGroup (the group turned inactive): proof of removal by
    // a device of the account. The app root replaces the whole app with the
    // removed screen and wipes everything by itself, exactly as it does for
    // auth's `401 device-revoked`; nothing is drawn here in between.
    val deviceRemoved by deviceGroupSync.deviceRemoved.collectAsStateWithLifecycle()
    LaunchedEffect(deviceRemoved) {
        if (deviceRemoved) authRepository.onRemovedFromDeviceGroup()
    }
    if (deviceRemoved) return

    // docs/ux/call-links.md section 9 — a guest never lands in the app: it is
    // an empty shell for them. Everything above keeps running (the session is
    // real), but in place of the tabs the guest gets the way back to a running
    // call, or the one question once there is none.
    if (guest != null && guestOffer != null) {
        me.lakko.app.ui.auth.GuestAccountScreen(
            viewModel = guestOffer,
            claimedUsername = guest.claimedUsername,
            callInProgress = callInProgress,
            onReturnToCall = { me.lakko.app.calls.AppCallHost.expandCurrentCall() },
        )
        return
    }

    val contactsForBadge by contactsRepository.state.collectAsState()
    val requestsWaiting = me.lakko.app.contacts.waitingRequestCount(contactsForBadge.pending)

    Scaffold(
        // Each screen pads by its own Scaffold; the shell contributes only the
        // tab bar (whose height is consumed below so a screen's Scaffold does
        // not add the navigation-bar inset a second time).
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            if (showTabs) {
                // docs/ux/identity-recovery.md 17.5 - while a new device of
                // the account is pending, the Settings destination carries
                // the attention badge.
                val deviceNotice by me.lakko.app.devices.NewDeviceNotices.view.collectAsStateWithLifecycle()
                NavigationBar {
                    for (tab in Tabs) {
                        val selected = currentRoute == tab.route
                        val attention = tab.route == Routes.SETTINGS && deviceNotice.attention
                        NavigationBarItem(
                            selected = selected,
                            onClick = { navController.navigateToTab(tab.route) },
                            icon = {
                                val label = me.lakko.app.l10n.t(tab.labelKey)
                                val waiting = if (tab.route == Routes.CONTACTS) requestsWaiting else 0
                                val name = if (attention) {
                                    "$label, ${me.lakko.app.l10n.t("nav.settings.attention_a11y")}"
                                } else {
                                    label
                                }
                                androidx.compose.material3.BadgedBox(
                                    badge = {
                                        if (attention) {
                                            me.lakko.app.ui.devices.AttentionBadge()
                                        } else if (waiting > 0) {
                                            me.lakko.app.ui.chat.UnreadBadge(count = waiting, muted = false)
                                        }
                                    },
                                    modifier = Modifier.semantics(mergeDescendants = true) {
                                        contentDescription = name
                                    },
                                ) {
                                    me.lakko.app.ui.components.LkIcon(
                                        icon = if (selected) tab.selectedIcon else tab.icon,
                                        contentDescription = null,
                                    )
                                }
                            },
                            label = { Text(me.lakko.app.l10n.t(tab.labelKey)) },
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
            // The notices at the top of the app. The strip carries the
            // status-bar inset and the NavHost consumes it, so nothing here
            // covers the status bar and no screen pads for it twice. A call
            // that was put away is not here: it floats over the app
            // (`docs/ux/calls.md` section 4.5, `CallMiniWindowHost`).
            val topStrip = banners != null
            if (topStrip) {
                Column(modifier = Modifier.windowInsetsPadding(WindowInsets.statusBars)) {
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
                    // docs/ux/identity-recovery.md 17.5 (amended 2026-10-29) -
                    // "A device was added to your account" is no longer a card
                    // here: it is a notification, the badge on Settings and
                    // the highlighted rows in Settings > Devices.
                    androidx.compose.foundation.layout.Column(modifier = Modifier.fillMaxSize()) {
                    // UX 18.4 - the session that took the account back.
                    me.lakko.app.ui.devices.RecallDoneLine(repository = authRepository)
                    // UX 18.3 - a kept device after the account keys were
                    // changed on another one: `Updating keys...`, then the
                    // dismissible notice. Never a full-screen state.
                    me.lakko.app.ui.devices.KeysRotatedNotice(
                        repository = authRepository,
                        onShowPhrase = { navController.navigate(Routes.RECOVERY) { launchSingleTop = true } },
                    )
                    val invitedAll by contactsRepository.invited.collectAsStateWithLifecycle()
                    val contactsNow by contactsRepository.state.collectAsStateWithLifecycle()
                    val invitedShown = remember(invitedAll, contactsNow.accepted) {
                        me.lakko.app.contacts.PendingContacts.shown(
                            invitedAll,
                            contactsNow.accepted.mapTo(HashSet()) { me.lakko.app.auth.base64Encode(it.request.fromPubkey) },
                        )
                    }
                    ChatListScreen(
                        modifier = Modifier.weight(1f),
                        repository = chatRepository,
                        onOpenThread = { hex -> navController.navigate(Routes.chat(hex)) },
                        onOpenChannel = { id -> navController.navigate(Routes.channel(id)) },
                        onNewChat = { navController.navigate(Routes.NEW_CHAT) },
                        onCreateGroup = { navController.navigate(Routes.NEW_GROUP) },
                        onCreateChannel = { navController.navigate(Routes.CHANNEL_NEW) },
                        onShowSuspension = { authRepository.suspensionShowScreen() },
                        // A tap on a search result opens the channel's page;
                        // it subscribes nobody.
                        onSubscribeToChannel = { id, handle ->
                            val held = channelsState.channels.value.any { it.id == id }
                            navController.navigate(
                                if (held) Routes.channel(id) else Routes.channelSubscribe(id, handle, pressed = false),
                            )
                        },
                        // ADR-0087 section 5.1 — the card's Subscribe: a
                        // press, so the channel opens subscribed, under the
                        // name the card read.
                        onSubscribeChannelPressed = { id, handle, name ->
                            val held = channelsState.channels.value.any { it.id == id }
                            navController.navigate(
                                if (held) Routes.channel(id) else Routes.channelSubscribe(id, handle, pressed = true, name = name),
                            )
                        },
                        // Only the plain lookup row (no people search wired)
                        // reaches this; the people search sends the request
                        // from its own row (who-can-find-me.md section 6.1).
                        channels = channelsState,
                        deviceGroupSync = deviceGroupSync,
                        // ADR-0079 — the people added through their invite
                        // link, listed at once.
                        invited = invitedShown,
                        onOpenPendingChat = { hex -> navController.navigate(Routes.pendingChat(hex)) },
                        // docs/ux/identity-recovery.md 7.2.
                        restoredChats = restoredChatsHint(chatRepository),
                        onOpenContacts = { navController.navigateToTab(Routes.CONTACTS) },
                        // docs/ux/chat-folders.md — the editor and Settings >
                        // Chat folders are pushed destinations.
                        onEditFolder = { id -> navController.navigate(Routes.folderEdit(id, null)) },
                        onNewFolder = { row ->
                            navController.navigate(
                                Routes.folderEdit(null, row?.let { me.lakko.app.chat.refOf(it) }),
                            )
                        },
                        onOpenFolderSettings = { navController.navigate(Routes.FOLDERS) },
                        // docs/ux/saved-messages-and-file-lifetime.md section 3.
                        onOpenSaved = { navController.navigate(Routes.SAVED) },
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
                }
                // `docs/ux/feed.md` — the ranked feed of group messages and
                // channel posts (ADR-0063).
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
                        // ordinary subscribe route under that handle. This is
                        // the card's Subscribe button: a press.
                        onSubscribeChannel = { id, handle ->
                            navController.navigate(Routes.channelSubscribe(id, handle, pressed = true))
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
                                // A created channel opens in the one
                                // conversation view.
                                navController.navigate(Routes.channel(id)) {
                                    popUpTo(Routes.CHANNEL_NEW) { inclusive = true }
                                }
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
                            initialLink = pendingCoPublisherLink,
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
                    // ADR-0087 section 12 — a channel opens where a group opens.
                    ChannelConversationScreen(
                        contactsRepository = contactsRepository,
                        channelId = id,
                        state = channelsState,
                        client = channelsClient,
                        onBack = { navController.popBackStack() },
                        onOpenInfo = { navController.navigate(Routes.channelInfo(id)) },
                        onForwardPost = { request ->
                            me.lakko.app.chat.PendingForward.publish(request)
                            navController.navigate(Routes.FORWARD)
                        },
                        // Section 9 — the membership ended: Chats, with nothing
                        // of the channel (its link or subscribe page) left
                        // under it.
                        onEnded = { navController.navigateToChatsClearing() },
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
                        androidx.navigation.navArgument("pressed") {
                            type = androidx.navigation.NavType.BoolType
                            defaultValue = false
                        },
                        androidx.navigation.navArgument("name") {
                            type = androidx.navigation.NavType.StringType
                            nullable = true
                            defaultValue = null
                        },
                    ),
                ) { entry ->
                    val id = entry.arguments?.getString("channelId") ?: return@composable
                    ChannelConversationScreen(
                        contactsRepository = contactsRepository,
                        channelId = id,
                        state = channelsState,
                        client = channelsClient,
                        handle = entry.arguments?.getString("handle")?.takeIf { it.isNotEmpty() },
                        subscribeOnOpen = entry.arguments?.getBoolean("pressed") == true,
                        name = entry.arguments?.getString("name")?.takeIf { it.isNotBlank() },
                        onBack = { navController.popBackStack() },
                        onOpenInfo = { navController.navigate(Routes.channelInfo(id)) },
                        onForwardPost = { request ->
                            me.lakko.app.chat.PendingForward.publish(request)
                            navController.navigate(Routes.FORWARD)
                        },
                        // Section 9 — the membership ended: Chats, with nothing
                        // of the channel (its link or subscribe page) left
                        // under it.
                        onEnded = { navController.navigateToChatsClearing() },
                    )
                }
                composable(Routes.CHANNEL_INFO) { entry ->
                    val id = entry.arguments?.getString("channelId") ?: return@composable
                    ChannelInfoScreen(
                        channelId = id,
                        state = channelsState,
                        client = channelsClient,
                        chatRepository = chatRepository,
                        contactsRepository = contactsRepository,
                        onBack = { navController.popBackStack() },
                        onLeft = {
                            // Section 9.5 — the row and its posts are gone:
                            // back to the list.
                            navController.popBackStack(Routes.channel(id), inclusive = true)
                        },
                    )
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
                    ContactsScreen(
                        viewModel = contactsVm,
                        onOpenContact = { key -> navController.navigate(Routes.contact(key)) },
                        onOpenChat = { hex -> navController.navigate(Routes.chat(hex)) },
                        onOpenPendingChat = { hex -> navController.navigate(Routes.pendingChat(hex)) },
                        // docs/ux/who-can-find-me.md section 6.1 — the same
                        // people search as the Chats field, people only.
                        peopleSearch = peopleSearch,
                        // A scanned /i/, /g/ or /c/ link takes the route an
                        // opened app link takes.
                        onScannedLink = { link -> inviteIntake.publishLink(link) },
                        channelsState = channelsState,
                        channelsClient = channelsClient,
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
                        // Section 2.1 (amended 2026-10-24) - a person the
                        // viewer removed keeps the one-to-one; the header
                        // still opens their card through it.
                        removedPeerChat = { key -> chatRepository.chatGroupIdHexFor(null, key) },
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
                    val quickReactionScope = androidx.compose.runtime.rememberCoroutineScope()
                    SettingsScreen(
                        displayName = ownProfile?.displayName,
                        ownAvatar = ownProfile?.avatar,
                        username = account.username.takeIf { it.isNotBlank() },
                        onOpenProfile = { navController.navigate(Routes.PROFILE) },
                        onOpenPasskeys = { navController.navigate(Routes.PASSKEYS) },
                        onDeleteAccount = { navController.navigate(Routes.DELETE_ACCOUNT) },
                        onOpenPassword = { navController.navigate(Routes.PASSWORD) },
                        onOpenDevices = { navController.navigate(Routes.DEVICES) },
                        onOpenRecovery = { navController.navigate(Routes.RECOVERY) },
                        onOpenAntiCensorship = { navController.navigate(Routes.ANTI_CENSORSHIP) },
                        onOpenChannelsIShare = { navController.navigate(Routes.CHANNELS_I_SHARE) },
                        onOpenLanguage = { navController.navigate(Routes.LANGUAGE) },
                        onOpenTextSize = { navController.navigate(Routes.TEXT_SIZE) },
                        onOpenSupport = { navController.navigate(Routes.DONATIONS) },
                        onOpenSupporters = { navController.navigate(Routes.SUPPORTERS) },
                        onOpenChatFolders = { navController.navigate(Routes.FOLDERS) },
                        onLogout = logout,
                        onForgetDevice = forgetDevice,
                        onOpenWhoCanFindMe = { navController.navigate(Routes.WHO_CAN_FIND_ME) },
                        onOpenBlockedPeople = { navController.navigate(Routes.BLOCKED_PEOPLE) },
                        onOpenYourReports = { navController.navigate(Routes.YOUR_REPORTS) },
                        onSetQuickReaction = { emoji ->
                            quickReactionScope.launch { runCatching { deviceGroupSync.setQuickReaction(emoji) } }
                        },
                        accountProtection = {
                            me.lakko.app.ui.auth.AccountProtectionSection(
                                repository = authRepository,
                                onOpenRecoveryEmail = { navController.navigate(Routes.RECOVERY_EMAIL) },
                                onOpenSavePhrase = { navController.navigate(Routes.SAVE_PHRASE) },
                                onOpenShowPhrase = { navController.navigate(Routes.RECOVERY) },
                            )
                        },
                    )
                }
                // ADR-0075 / UX 19.2 - Settings > Account > Recovery email.
                composable(Routes.RECOVERY_EMAIL) {
                    me.lakko.app.ui.auth.RecoveryEmailScreen(
                        repository = authRepository,
                        onDone = { navController.popBackStack() },
                        onBack = { navController.popBackStack() },
                    )
                }
                // ADR-0075 / UX 19.5 - Settings > Account > Recovery phrase.
                composable(Routes.SAVE_PHRASE) {
                    me.lakko.app.ui.auth.SaveRecoveryPhraseScreen(
                        repository = authRepository,
                        chatRepository = chatRepository,
                        onDone = { navController.popBackStack() },
                        onBack = { navController.popBackStack() },
                    )
                }

                // ADR-0098 — Settings > Privacy > Blocked people / Your reports.
                composable(Routes.BLOCKED_PEOPLE) {
                    me.lakko.app.ui.moderation.BlockedPeopleScreen(
                        contactsRepository = contactsRepository,
                        onBack = { navController.popBackStack() },
                    )
                }
                composable(Routes.YOUR_REPORTS) {
                    me.lakko.app.ui.moderation.YourReportsScreen(onBack = { navController.popBackStack() })
                }

                composable(Routes.WHO_CAN_FIND_ME) {
                    // docs/ux/who-can-find-me.md — Settings > Privacy > Who can
                    // find me. The groups and channels this account is in are
                    // what "Only the ones I choose" lists.
                    val chatState by chatRepository.state.collectAsStateWithLifecycle()
                    val channels by channelsState.channels.collectAsStateWithLifecycle()
                    val ctx = chatRepository.context
                    val groupFallback = me.lakko.app.l10n.t("chat.group.fallback_name")
                    val channelFallback = me.lakko.app.l10n.t("channels.fallback_name")
                    val scopes = remember(chatState.threads, channels, groupFallback, channelFallback) {
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
                                        ?.takeIf { it.isNotBlank() } ?: groupFallback,
                                )
                            }
                        val chans = channels.map { handle ->
                            me.lakko.app.ui.settings.FindMeScope(
                                kind = me.lakko.app.ui.settings.FindMeScopeKind.CHANNEL,
                                idHex = handle.id,
                                name = handle.sharedName.ifBlank { handle.localName }.ifBlank { channelFallback },
                            )
                        }
                        groups + chans
                    }
                    // Section 4.1 — a contact with no known account id cannot
                    // be chosen.
                    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
                    val names = remember(ctx) { me.lakko.app.profile.ProfileDirectory.get(ctx) }
                    val nameVersion by names.version.collectAsStateWithLifecycle()
                    val findMeContacts = remember(contactsState.accepted, nameVersion) {
                        contactsState.accepted.mapNotNull { contact ->
                            val id = contact.request.fromAccountId
                                ?.takeIf { it.size == me.lakko.app.contacts.CONTACT_ACCOUNT_ID_LEN }
                                ?: return@mapNotNull null
                            val resolved = names.resolve(contact.request.fromPubkey)
                            me.lakko.app.ui.settings.FindMeContact(
                                accountIdHex = me.lakko.app.reach.reachHex(id),
                                keyB64 = java.util.Base64.getEncoder().encodeToString(contact.request.fromPubkey),
                                name = resolved.primary,
                                username = resolved.verifiedUsername,
                                placeholder = resolved.isPlaceholder,
                            )
                        }
                    }
                    me.lakko.app.ui.settings.WhoCanFindMeScreen(
                        reach = me.lakko.app.reach.ReachManager.get(ctx),
                        scopes = scopes,
                        contacts = findMeContacts,
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
                    val routeGroupIdHex = entry.arguments?.getString("groupIdHex") ?: return@composable
                    // ADR-0073 R.5 — the route may name ANY segment of a
                    // conversation (the list's conversation id, an older
                    // group a notification or a forward names, the group
                    // this screen was opened on before the conversation
                    // moved): the screen is the conversation's CURRENT group.
                    val routeConversations by chatRepository.state.collectAsStateWithLifecycle()
                    val groupIdHex = remember(routeGroupIdHex, routeConversations.conversations) {
                        me.lakko.app.chat.conversationThreadOf(routeConversations.conversations, routeGroupIdHex)
                            ?.groupIdHex ?: routeGroupIdHex
                    }
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
                                // docs/ux/conversation-info.md section 2.4 — a
                                // person a search found through a grant they
                                // handed out: keep the grant, so the Welcome the
                                // sweep owes them can be written with it. A grant
                                // that cannot be kept is logged and the add goes
                                // on (the Welcome then waits for another grant).
                                keepFoundGrant = { accountKey, wireB64 ->
                                    try {
                                        val grant = uniffi.lakko_ffi_uniffi.decodeReachGrant(
                                            java.util.Base64.getDecoder().decode(wireB64),
                                        )
                                        me.lakko.app.reach.ReachManager.get(appContext)
                                            .fileHeld(accountKey, grant, scopeHex = "")
                                    } catch (cause: Throwable) {
                                        me.lakko.app.rethrowIfCancelled(cause)
                                        android.util.Log.w("MainActivity", "found grant not kept: ${cause.javaClass.simpleName}")
                                    }
                                },
                                // ADR-0060 section 6 (amended 2026-10-29) — a
                                // person found in Everyone: record the binding
                                // their public record carried for this add
                                // (verified under their account key first). A
                                // binding that is refused is logged and the add
                                // goes on (the Welcome then stays owed).
                                keepFoundBinding = { gidHex, accountKey, bindingB64 ->
                                    try {
                                        val recorded = me.lakko.app.reach.ReachManager.get(appContext).recordOpenBinding(
                                            gidHex,
                                            accountKey,
                                            java.util.Base64.getDecoder().decode(bindingB64),
                                        )
                                        if (!recorded) android.util.Log.w("MainActivity", "found binding not recorded")
                                    } catch (cause: Throwable) {
                                        me.lakko.app.rethrowIfCancelled(cause)
                                        android.util.Log.w("MainActivity", "found binding not recorded: ${cause.javaClass.simpleName}")
                                    }
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
                    // docs/ux/call-links.md section 3 — the `Join` of a call
                    // invitation in the timeline opens the same `/c/` landing a
                    // tapped or a pasted link does.
                    androidx.compose.runtime.CompositionLocalProvider(
                        me.lakko.app.ui.chat.LocalOpenCallLink provides { argument: String ->
                            inviteIntake.publishCallJoin(argument)
                        },
                    ) {
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
                            // docs/ux/conversation-info.md section 2.4 — the Add
                            // members screen asks the network the way the
                            // search fields do, with the same session memory.
                            peopleSearch = peopleSearch,
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
                            repository = authRepository,
                            deps = remember(accountId) {
                                // docs/ux/profile-screen.md §5 — the three steps of Save.
                                object : me.lakko.app.ui.profile.ProfileSaveDeps {
                                    override suspend fun upload(
                                        bytes: ByteArray,
                                        mime: String,
                                        side: Int,
                                    ): me.lakko.app.profile.AvatarBlob {
                                        val ref = me.lakko.app.chat.AttachmentBridge.encryptAndUpload(
                                            bytes = bytes,
                                            mime = mime,
                                            name = null,
                                        )
                                        val blob = me.lakko.app.profile.avatarBlobOf(
                                            ref.blobId,
                                            ref.key,
                                            ref.nonce,
                                            bytes.size.toLong(),
                                            side,
                                        )
                                        // The owner's own photo is never downloaded again.
                                        me.lakko.app.avatar.AvatarImages.putOwn(ctx, blob, bytes)
                                        return blob
                                    }

                                    override fun proofLive(): Boolean = authRepository.freshProofLive()

                                    override suspend fun changeUsername(
                                        newUsername: String,
                                    ): me.lakko.app.auth.RenameResult = authRepository.changeUsername(newUsername)

                                    override fun publish(
                                        name: String?,
                                        avatar: me.lakko.app.profile.AvatarRefs?,
                                    ) {
                                        val session = (authRepository.state.value as? me.lakko.app.auth.AuthState.Session)
                                            ?.account?.username?.takeIf { it.isNotBlank() }
                                        val previous = me.lakko.app.profile.OwnProfileStore.load(ctx, accountId)?.avatar
                                        val stored = me.lakko.app.profile.OwnProfileStore.update(ctx, accountId) { held ->
                                            me.lakko.app.profile.applyOwnProfileChange(
                                                current = held,
                                                newName = name,
                                                newAvatar = avatar,
                                                username = held?.username ?: session,
                                                nowMs = System.currentTimeMillis(),
                                            )
                                        }
                                        if (stored) {
                                            chatRepository.publishOwnProfileToAllGroups()
                                            // ADR-0048 section 4a — and to the account's
                                            // other devices, with the name's stored stamp.
                                            me.lakko.app.reach.ReachManager.get(ctx).publishOwnProfileToDevices()
                                            // ADR-0065 section 6 — a replaced or removed photo's
                                            // blobs go from the server (retried when it fails).
                                            val gone = me.lakko.app.profile.replacedAvatarBlobs(previous, avatar)
                                            if (gone.isNotEmpty()) me.lakko.app.storage.BlobLifetime.delete(ctx, gone)
                                        }
                                    }
                                }
                            },
                            onBack = { navController.popBackStack() },
                        )
                    } else {
                        LoadingScreen()
                    }
                }
                for (route in listOf(Routes.PASSKEYS, Routes.DELETE_ACCOUNT)) {
                    composable(route) {
                        val passkeysAppContext = androidx.compose.ui.platform.LocalContext.current.applicationContext
                        val passkeysVm: me.lakko.app.webauthn.PasskeysViewModel = viewModel(
                            factory = singletonViewModelFactory {
                                me.lakko.app.webauthn.PasskeysViewModel(
                                    repo = authRepository,
                                    onAccountDeleted = forgetDevice,
                                    appContext = passkeysAppContext,
                                )
                            },
                        )
                        me.lakko.app.webauthn.PasskeysScreen(
                            viewModel = passkeysVm,
                            onBack = { navController.popBackStack() },
                            deleteAccountOnly = route == Routes.DELETE_ACCOUNT,
                        )
                    }
                }
                composable(Routes.DEVICES) {
                    DevicesScreen(
                        authRepository = authRepository,
                        onBack = { navController.popBackStack() },
                        onAddDevice = { navController.navigate(Routes.DEVICES_ADD) },
                        chatRepository = chatRepository,
                        onChangePassword = { navController.navigate(Routes.PASSWORD) },
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
                // ADR-0065 section 4 / UX section 3 — the saved messages.
                composable(Routes.SAVED) {
                    me.lakko.app.ui.chat.SavedScreen(
                        sync = deviceGroupSync,
                        repository = chatRepository,
                        onBack = { navController.popBackStack() },
                        onOpenMessage = { hex, messageId ->
                            me.lakko.app.chat.PendingMessageJump.publish(hex, messageId)
                            navController.navigate(Routes.chat(hex))
                        },
                        channelTitle = { hex ->
                            channelsState.channels.value.firstOrNull { it.id.equals(hex, ignoreCase = true) }
                                ?.let { me.lakko.app.ui.channels.channelTitle(it, me.lakko.app.l10n.Localization.strings.value) }
                        },
                        onOpenChannel = { hex ->
                            val id = channelsState.channels.value.firstOrNull { it.id.equals(hex, ignoreCase = true) }?.id ?: hex
                            navController.navigate(Routes.channel(id))
                        },
                    )
                }
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
                        onOpenPasskeys = { navController.navigate(Routes.PASSKEYS) },
                        onOpenDevices = { navController.navigate(Routes.DEVICES) },
                    )
                }
                composable(Routes.PASSWORD) {
                    me.lakko.app.ui.settings.PasswordScreen(
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
                composable(Routes.LANGUAGE) {
                    me.lakko.app.ui.settings.LanguageScreen(onBack = { navController.popBackStack() })
                }
                composable(Routes.TEXT_SIZE) {
                    me.lakko.app.ui.settings.TextSizeScreen(onBack = { navController.popBackStack() })
                }
                // `docs/ux/supporters.md` section 2.2.
                composable(Routes.SUPPORTERS) {
                    me.lakko.app.ui.settings.SupportersScreen(onBack = { navController.popBackStack() })
                }
                composable(Routes.DONATIONS) {
                    me.lakko.app.ui.donate.DonationsScreen(onBack = { navController.popBackStack() })
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
                        // ADR-0079 — the page closes into the chat with the inviter.
                        onOpenChat = { target ->
                            val route = when (target) {
                                is me.lakko.app.ui.contacts.InviteChatTarget.Pending ->
                                    Routes.pendingChat(target.accountIdHex)
                                is me.lakko.app.ui.contacts.InviteChatTarget.Conversation ->
                                    Routes.chat(target.groupIdHex)
                                is me.lakko.app.ui.contacts.InviteChatTarget.Contact ->
                                    Routes.contact(target.routeKey)
                            }
                            navController.navigate(route) {
                                popUpTo(Routes.INVITE) { inclusive = true }
                            }
                        },
                        // Amended 2026-10-19 — a link that does not work:
                        // Contacts, nothing else shown.
                        onOpenContacts = {
                            navController.popBackStack(Routes.INVITE, inclusive = true)
                            navController.navigateToTab(Routes.CONTACTS)
                        },
                    )
                }
                composable(Routes.PENDING_CHAT) { entry ->
                    val accountIdHex = entry.arguments?.getString("accountIdHex") ?: return@composable
                    me.lakko.app.ui.chat.PendingChatScreen(
                        accountIdHex = accountIdHex,
                        contacts = contactsRepository,
                        onBack = { navController.popBackStack() },
                        onConnected = { hex ->
                            navController.navigate(Routes.chat(hex)) {
                                popUpTo(Routes.PENDING_CHAT) { inclusive = true }
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
                        // ADR-0087 section 7.2 — a channel's link page.
                        onSubscribeChannel = { preview ->
                            val channelBytes = preview.channelId
                            val invite = preview.subscriber
                            if (channelBytes != null && invite != null) {
                                channelLinkScope.launch {
                                    try {
                                        val idHex = me.lakko.app.channels.ChannelInviteLinks.subscribeFromLink(
                                            appContextForLinks,
                                            channelsState,
                                            channelsClient,
                                            channelBytes,
                                            preview.groupName,
                                            invite,
                                        )
                                        val pressedPublic = invite.access is uniffi.lakko_ffi_uniffi.FfiChannelAccess.Public
                                        // The link page is replaced by the channel,
                                        // never left under it.
                                        navController.navigate(
                                            Routes.channelSubscribe(idHex, null, pressed = pressedPublic, name = preview.groupName),
                                        ) {
                                            popUpTo(Routes.GROUP_JOIN) { inclusive = true }
                                            launchSingleTop = true
                                        }
                                    } catch (e: kotlinx.coroutines.CancellationException) {
                                        throw e
                                    } catch (e: Throwable) {
                                        android.util.Log.w("SessionRoot", "channel subscribe from link failed", e)
                                    }
                                }
                            }
                        },
                        onJoinAsPublisher = {
                            pendingCoPublisherLink = "https://lakko.me/g/$groupJoinArgument"
                            navController.popBackStack()
                            navController.navigate(Routes.CHANNEL_JOIN)
                        },
                        // Section 15.2 — Accept joins here, then the channel
                        // replaces the link page.
                        onAcceptPublisher = { onStatus ->
                            val joined = me.lakko.app.ds.DsClient().use { ds ->
                                me.lakko.app.ui.channels.joinAsCoPublisher(
                                    inviteCardContext,
                                    channelsState,
                                    ds,
                                    "https://lakko.me/g/$groupJoinArgument",
                                    groupJoinVm.card.value?.preview?.groupName.orEmpty()
                                        .ifBlank { me.lakko.app.l10n.UiText("channels.join.default_name").render() },
                                    me.lakko.app.channels.CoPublishedJournals.onDevice(inviteCardContext) { channelsState.boundAccount },
                                    onStatus,
                                )
                            }
                            if (joined != null) {
                                navController.navigate(Routes.channel(joined)) {
                                    popUpTo(Routes.GROUP_JOIN) { inclusive = true }
                                    launchSingleTop = true
                                }
                            }
                            joined
                        },
                        publisherRequestSentAtMs = { channelIdHex ->
                            channelsState.boundAccount?.let { acct ->
                                me.lakko.app.channels.PendingPublisherRequests.sentAtMs(inviteCardContext, acct, channelIdHex)
                            }
                        },
                        isPublisherOf = { channelIdHex -> channelsState.keysByChannel.value.containsKey(channelIdHex) },
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

/** ADR-0087 section 7 — between let-in passes over private channels (chosen). */
private const val PRIVATE_ADMIT_INTERVAL_MS: Long = 0L

/** App-level receive loop: pause after a failed pass. */
private const val APP_POLL_RETRY_BACKOFF_MS: Long = 5_000L

/** App-level receive loop: a pass that returned sooner than this waits out the rest. */
private const val APP_POLL_MIN_ITERATION_MS: Long = 1_000L

/** ADR-0065 section 3 — the keep tick's cadence while the app is in front;
 *  chosen. Each bucket is due about once a month, so this only bounds how
 *  late a due keep can be. */
private const val KEEP_TICK_MS: Long = 10L * 60L * 1000L

/** ADR-0065 section 3 — the first keep tick, after the landing. */
private const val KEEP_FIRST_TICK_MS: Long = 60L * 1000L

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
    const val CHANNEL_SUBSCRIBE = "channel/{channelId}/subscribe?handle={handle}&pressed={pressed}&name={name}"

    /** [pressed]: the person pressed Subscribe on the way here. Without it the
     *  page only offers the button. [name]: the channel's name as the card
     *  that opened the page read it, its list name until the channel's own
     *  record arrives. */
    fun channelSubscribe(channelId: String, handle: String?, pressed: Boolean, name: String? = null): String =
        "channel/$channelId/subscribe?handle=${android.net.Uri.encode(handle.orEmpty())}&pressed=$pressed" +
            "&name=${android.net.Uri.encode(name.orEmpty())}"
    /** ADR-0087 section 12.1 — a channel's info screen (the Manage page is gone). */
    const val CHANNEL_INFO = "channel/{channelId}/info"
    fun channelInfo(channelId: String): String = "channel/$channelId/info"
    const val PROFILE = "settings/profile"
    const val WHO_CAN_FIND_ME = "settings/privacy/who-can-find-me"
    const val BLOCKED_PEOPLE = "settings/privacy/blocked"
    const val YOUR_REPORTS = "settings/privacy/reports"
    const val PASSKEYS = "settings/passkeys"
    const val DELETE_ACCOUNT = "settings/delete-account"
    const val PASSWORD = "settings/password"
    const val DEVICES = "settings/devices"
    const val DEVICES_ADD = "settings/devices/add"
    const val RECOVERY = "settings/recovery"
    const val RECOVERY_EMAIL = "settings/recovery_email"
    const val SAVE_PHRASE = "settings/save_phrase"
    const val ANTI_CENSORSHIP = "settings/anti_censorship"
    const val CHANNELS_I_SHARE = "settings/channels_i_share"
    const val LANGUAGE = "settings/language"
    const val TEXT_SIZE = "settings/text-size"
    /** `docs/ux/supporters.md` section 2.2. */
    const val SUPPORTERS = "settings/supporters"
    const val DONATIONS = "settings/donations"
    /** docs/ux/chat-folders.md section 4. */
    const val FOLDERS = "settings/folders"
    const val SAVED = "chats/saved"
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
    /** ADR-0079 section 5 — the chat with a person added through their
     *  invite link before their app answered; hex (URL-safe by construction). */
    const val PENDING_CHAT = "invited/{accountIdHex}"
    fun pendingChat(accountIdHex: String): String = "invited/$accountIdHex"
    const val GROUP_JOIN = "group_join/{slug}"
    fun groupJoin(link: me.lakko.app.chat.GroupJoinLink): String =
        "group_join/${android.net.Uri.encode(me.lakko.app.chat.composeGroupJoinArgument(link))}"
}

private data class TabSpec(
    val route: String,
    /** Catalog key of the tab label (ADR-0061). */
    val labelKey: String,
    @androidx.annotation.DrawableRes val icon: Int,
    /** The `-fill` variant shown while the tab is selected (design system, section "Icons"). */
    @androidx.annotation.DrawableRes val selectedIcon: Int,
)

/**
 * The five primary destinations, in the order every client shows them
 * (`docs/ux/navigation.md` section 1; Feed added by ADR-0063).
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
    TabSpec(Routes.CHATS, "nav.chats", R.drawable.ic_lk_chat, R.drawable.ic_lk_chat_fill),
    // `docs/ux/feed.md` section 2 — second, one tap from the home.
    TabSpec(Routes.FEED, "nav.feed", R.drawable.ic_lk_feed, R.drawable.ic_lk_feed_fill),
    TabSpec(Routes.CONTACTS, "nav.contacts", R.drawable.ic_lk_person_square, R.drawable.ic_lk_person_square_fill),
    TabSpec(Routes.CALLS, "nav.calls", R.drawable.ic_lk_phone, R.drawable.ic_lk_phone_fill),
    TabSpec(Routes.SETTINGS, "nav.settings", R.drawable.ic_lk_gear, R.drawable.ic_lk_gear_fill),
)

/** Standard tab switch: one back-stack per tab, state saved and restored. */
/** Chats with every pushed screen above it removed (section 9: a
 *  conversation that ended leaves nothing of itself in the back stack). */
private fun NavHostController.navigateToChatsClearing() {
    navigate(Routes.CHATS) {
        popUpTo(graph.findStartDestination().id) { inclusive = false }
        launchSingleTop = true
    }
}

private fun NavHostController.navigateToTab(route: String) {
    navigate(route) {
        popUpTo(graph.findStartDestination().id) { saveState = true }
        launchSingleTop = true
        restoreState = true
    }
}

/** Safe-drawing insets for the roots outside the tabbed shell (auth, recovery). */
/**
 * What the chat list says after a restore. The list being empty is the chat
 * layer's; the marker is dropped for good when the first conversation arrives.
 */
@Composable
private fun restoredChatsHint(chatRepository: ChatRepository): me.lakko.app.backup.RestoredChatsHint {
    val context = androidx.compose.ui.platform.LocalContext.current
    val state by chatRepository.state.collectAsStateWithLifecycle()
    val accountIdB64 = chatRepository.activeAccountIdB64
    val listEmpty = state.bootstrapped && state.threads.isEmpty()
    // What this device's DeviceGroup rejoin came to (another device answered
    // its knock, or it founded): recorded by the rejoin, kept per account.
    val rejoinFlow = remember(accountIdB64) {
        accountIdB64?.let { me.lakko.app.backup.BackupRestoreStore.rejoinAnswerFlow(context, it) }
            ?: kotlinx.coroutines.flow.flowOf(null)
    }
    val rejoinInitial = remember(accountIdB64) {
        accountIdB64?.let { me.lakko.app.backup.BackupRestoreStore.rejoinAnswer(context, it) }
    }
    val rejoin by rejoinFlow.collectAsStateWithLifecycle(initialValue = rejoinInitial)
    // Read again when the rejoin decides: the restore may finish after the
    // list first drew.
    val knew = remember(accountIdB64, state.bootstrapped, state.threads.size, rejoin) {
        accountIdB64?.let { me.lakko.app.backup.BackupRestoreStore.restoredChats(context, it) }
    }
    LaunchedEffect(accountIdB64, knew != null, state.threads.isNotEmpty()) {
        if (knew != null && state.threads.isNotEmpty() && accountIdB64 != null) {
            me.lakko.app.backup.BackupRestoreStore.clearRestoredChats(context, accountIdB64)
        }
    }
    return me.lakko.app.backup.restoredChatsHint(knew, listEmpty, rejoin)
}

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
    /** The screen the signed-out invite page chose (sign in or create an
     *  account), or `null` for Welcome. Opened over Welcome, so Back from it
     *  returns there. */
    startRoute: String? = null,
) {
    val appContext = LocalContext.current.applicationContext
    val nav = rememberNavController()
    var startRouteOpened by androidx.compose.runtime.saveable.rememberSaveable {
        androidx.compose.runtime.mutableStateOf(false)
    }
    LaunchedEffect(startRoute) {
        if (startRoute != null && !startRouteOpened) {
            startRouteOpened = true
            nav.navigate(startRoute) { launchSingleTop = true }
        }
    }

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
                        // ADR-0097 - supporter codes on the full state.
                        supporterPlaces = me.lakko.app.auth.SupporterPlacesOnDevice.get(appContext),
                    )
                },
            )
            SignupScreen(
                viewModel = signupVm,
                onBack = { nav.popBackStack() },
                onLogIn = {
                    nav.navigate(AnonRoutes.LOGIN) {
                        popUpTo(AnonRoutes.SIGNUP) { inclusive = true }
                        launchSingleTop = true
                    }
                },
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

/** ADR-0090 section 2.4 - how long Forget device waits for the retire
 *  requests of this device's channel leaves before it stops everything
 *  (chosen, not measured). */
private const val RETIRE_BEFORE_FORGET_MS: Long = 4_000L
