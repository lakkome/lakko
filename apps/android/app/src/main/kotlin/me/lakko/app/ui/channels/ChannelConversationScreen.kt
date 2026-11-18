// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.util.Log
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.layout.positionOnScreen
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.lifecycle.repeatOnLifecycle
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.channels.ChannelFeedback
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelPostItem
import me.lakko.app.ui.chat.withSendAsFile
import me.lakko.app.ui.chat.without
import me.lakko.app.channels.ChannelPublisherLoop
import me.lakko.app.channels.ChannelPublisherSigners
import me.lakko.app.channels.ChannelReader
import me.lakko.app.channels.ChannelTimeline
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.ChannelsComposerException
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.CollapsedMore
import me.lakko.app.channels.channelHeaderLineKey
import me.lakko.app.channels.channelIsOrphaned
import me.lakko.app.channels.collapsedMore
import me.lakko.app.channels.fullRecord
import me.lakko.app.channels.sharedMeta
import me.lakko.app.channels.subscriberBand
import me.lakko.app.chat.AttachmentBridge
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.chat.Bubble
import me.lakko.app.ui.chat.ChannelPostActions
import me.lakko.app.ui.chat.Composer
import me.lakko.app.ui.chat.EditMessageDialog
import me.lakko.app.ui.chat.OwnReadState
import me.lakko.app.ui.chat.PendingMediaBubble
import me.lakko.app.ui.chat.UploadProgress
import me.lakko.app.ui.chat.VideoNoteOverlay
import me.lakko.app.ui.chat.copyMessageText
import me.lakko.app.ui.chat.rememberRecordController
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import uniffi.lakko_ffi_uniffi.AttachmentRef
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

private const val TAG = "ChannelConversation"

/** While open: posts re-read about every 5 s, the envelopes every 30 s. */
/** The open channel is read every 10 s (chosen; it was 5 s). */
private const val READ_INTERVAL_MS: Long = 10_000L
private const val SEED_WAIT_MS: Long = 3_000L
private const val ENVELOPE_RECHECK_EVERY: Int = 6

/** ADR-0087 section 13 — a channel file: 30 days of posts and 15 of margin. */
internal const val CHANNEL_FILE_IDLE_DAYS: Int = 45

/**
 * ADR-0087 section 12 — a channel opens where a group opens, in the same view:
 * the conversation header (avatar, title, one line), the timeline of posts as
 * incoming bubbles with no sender line (the channel is the sender), the
 * group's composer for the owner and publishers and none for a subscriber,
 * and no call controls. What only a channel has: a post is a card with the
 * width of the pane, with its reactions and comments under it (section 13a),
 * and Subscribe where a group has joining.
 *
 * [subscribeOnOpen] is the search card's or link page's Subscribe: pressed
 * already, so the channel opens subscribed.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChannelConversationScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onBack: () -> Unit,
    onOpenInfo: () -> Unit,
    handle: String? = null,
    name: String? = null,
    subscribeOnOpen: Boolean = false,
    /** UX 12 — a post's Forward: the forward picker with the post. */
    onForwardPost: ((me.lakko.app.chat.ForwardRequest) -> Unit)? = null,
    /** Section 9 — the membership ended while open: after OK, Chats. */
    onEnded: () -> Unit = onBack,
    /** ADR-0092 — the Contact item of the attachment menu lists these. */
    contactsRepository: me.lakko.app.contacts.ContactsRepository? = null,
) {
    val context = LocalContext.current
    val strings = LocalStrings.current
    val scope = rememberCoroutineScope()
    val channels by state.channels.collectAsState()
    val keysByChannel by state.keysByChannel.collectAsState()
    val subscriberKeys by state.subscriberKeysByChannel.collectAsState()
    val orphaned by state.orphaned.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    val isOrphaned = channelIsOrphaned(channelId, orphaned)
    val isPublisher = keysByChannel.containsKey(channelId) && !isOrphaned
    // docs/ux/second-device.md section 4a — the account owns or co-publishes
    // this channel from another device: read it here, with one line in
    // place of the composer.
    val publisherKeysReady by state.publisherKeysReady.collectAsState()
    val readerRole = me.lakko.app.channels.readerDeviceRole(
        role = remember(channels, keysByChannel, channelId) { state.accountRoleOf(channelId) },
        publishesHere = isPublisher,
        orphaned = isOrphaned,
        keysReady = publisherKeysReady,
    )
    val followed = channel != null && subscriberKeys.containsKey(channelId)
    var pressed by rememberSaveable(channelId) { mutableStateOf(subscribeOnOpen) }
    val reading = isPublisher || followed || pressed
    val record = remember(channel) { channel?.sharedMeta()?.fullRecord() }
    // Section 12 — unknown visibility shows no line, never "Private channel".
    val knownPublic = remember(channel, channelId) {
        me.lakko.app.channels.channelVisibilityOf(context, state.boundAccount, channel, channelId)
    }
    val public = knownPublic ?: false
    val counts by (ChannelPublisherLoop.peek(state)?.subscriberCounts
        ?: kotlinx.coroutines.flow.MutableStateFlow(emptyMap())).collectAsState()
    val band = counts[channelId]?.let(::subscriberBand)
    val headerLine = channelHeaderLineKey(knownPublic, band)?.let { (key, args) -> strings.format(key, args) }
    val title = channel?.let { channelTitle(it, strings) }
        ?: name?.trim()?.takeIf { it.isNotEmpty() }
        ?: handle?.trim()?.removePrefix("@")?.takeIf { it.isNotEmpty() }?.let { "@$it" }
        ?: t("channels.fallback_name")
    val avatar = record?.avatar?.let { me.lakko.app.profile.avatarRefsOf(it) }

    // docs/ux/channel-notifications.md section 2 — while this screen is in
    // front, a post of this channel does not notify.
    val visibilityOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    androidx.compose.runtime.DisposableEffect(channelId, visibilityOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            when (event) {
                androidx.lifecycle.Lifecycle.Event.ON_RESUME -> me.lakko.app.feed.VisibleChannel.shown(channelId)
                androidx.lifecycle.Lifecycle.Event.ON_PAUSE -> me.lakko.app.feed.VisibleChannel.hidden(channelId)
                else -> Unit
            }
        }
        visibilityOwner.lifecycle.addObserver(observer)
        onDispose {
            visibilityOwner.lifecycle.removeObserver(observer)
            me.lakko.app.feed.VisibleChannel.hidden(channelId)
        }
    }

    // A channel this device reads belongs in the one Chats list.
    LaunchedEffect(channelId, reading) {
        if (reading && state.channels.value.none { it.id == channelId }) {
            val bare = handle?.trim()?.removePrefix("@").orEmpty()
            state.addChannel(
                ChannelHandle(
                    id = channelId,
                    localName = name?.trim()?.takeIf { it.isNotEmpty() } ?: bare,
                    localDescription = "",
                    addedAtMs = System.currentTimeMillis().toULong(),
                    handle = bare,
                ),
            )
        }
    }

    // Section 9 — the membership ended while the channel is open (removed,
    // or the channel deleted): "You are no longer in {name}." with the
    // channel's body, one OK, then Chats.
    val removedNow by me.lakko.app.channels.ChannelRemovalWatch.removed.collectAsState()
    removedNow[channelId]?.let { removedName ->
        val leave = {
            me.lakko.app.channels.ChannelRemovalWatch.acknowledge(channelId)
            onEnded()
        }
        LakkoDialog(
            onDismissRequest = leave,
            title = t("chat.ended.notice", mapOf("name" to removedName.ifBlank { title })),
            body = t("channels.ended.notice_body"),
            confirm = LakkoDialogAction(t("common.ok")) { leave() },
            cancel = null,
        )
    }
    Scaffold(
        topBar = {
            TopAppBar(
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        LkIcon(R.drawable.ic_lk_chevron_back, contentDescription = t("chat.header.back"))
                    }
                },
                title = {
                    // Section 12 — the name and the avatar open the info
                    // screen, as a group's do. No call controls, for anyone.
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.clickable(enabled = channel != null, onClick = onOpenInfo),
                    ) {
                        // UX profile-screen section 7: the chat header plays a
                        // profile video always on a touch device.
                        LakkoAvatar(name = title, seed = channelId, size = 32.dp, group = true, avatar = avatar, playVideo = true)
                        Spacer(Modifier.width(10.dp))
                        Column {
                            Text(title, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            headerLine?.let {
                                Text(
                                    it,
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        }
                    }
                },
            )
        },
    ) { inner ->
        Box(Modifier.fillMaxSize().padding(inner)) {
            when {
                isOrphaned -> OrphanedNotice(onRemove = {
                    state.removeOrphanedFromDevice(channelId)
                    onBack()
                })
                !reading -> SubscribeCard(
                    title = title,
                    handle = record?.handle?.handle ?: channel?.handle?.takeIf { it.isNotBlank() } ?: handle,
                    line = headerLine,
                    description = channel?.let { channelDescription(it) },
                    avatar = avatar,
                    seed = channelId,
                    onSubscribe = { pressed = true },
                )
                else -> ChannelTimelineView(
                    contactsRepository = contactsRepository,
                    channelId = channelId,
                    state = state,
                    client = client,
                    isPublisher = isPublisher,
                    readerRole = readerRole,
                    publicChannel = public,
                    register = !isPublisher && !followed,
                    onRegistered = {
                        scope.launch { me.lakko.app.channels.ChannelUnsubscribe.noteFollowing(channelId) }
                    },
                    channelName = title,
                    onForwardPost = onForwardPost,
                )
            }
        }
    }
}

/** Section 5.1 — the card of a channel this account does not read: avatar,
 *  name, `@name`, the line, the description, Subscribe. It registers nothing
 *  until Subscribe is pressed. */
@Composable
private fun SubscribeCard(
    title: String,
    handle: String?,
    line: String?,
    description: String?,
    avatar: me.lakko.app.profile.AvatarRefs?,
    seed: String,
    onSubscribe: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterVertically),
    ) {
        LakkoAvatar(name = title, seed = seed, size = 96.dp, group = true, avatar = avatar, large = true)
        Text(title, style = MaterialTheme.typography.titleLarge)
        handle?.trim()?.removePrefix("@")?.takeIf { it.isNotEmpty() }?.let {
            Text("@$it", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        line?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
        description?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
        Spacer(Modifier.height(8.dp))
        Button(onClick = onSubscribe) { Text(t("channels.subscribe.action")) }
    }
}

@Composable
private fun OrphanedNotice(onRemove: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text(t("channels.keys_reset.notice"), style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.height(16.dp))
        Button(onClick = onRemove) { Text(t("channels.keys_reset.remove")) }
    }
}

private enum class ReaderPhase { Registering, Waiting, Active, Failed }

/**
 * The timeline, the reactions and comments under each post, and, for the
 * owner and publishers, the composer.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ChannelTimelineView(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    isPublisher: Boolean,
    /** docs/ux/second-device.md section 4a — the account's role when this
     *  device reads a channel it owns or co-publishes elsewhere, else `null`. */
    readerRole: me.lakko.app.channels.ChannelAccountRole?,
    publicChannel: Boolean,
    /** A subscriber's first read: register this device's key first. */
    register: Boolean,
    onRegistered: () -> Unit,
    channelName: String,
    onForwardPost: ((me.lakko.app.chat.ForwardRequest) -> Unit)?,
    contactsRepository: me.lakko.app.contacts.ContactsRepository? = null,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val savedSync = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active
    val savedList by (savedSync?.saved ?: kotlinx.coroutines.flow.MutableStateFlow(emptyList())).collectAsState()
    val channelBytes = remember(channelId) { me.lakko.app.channels.ChannelsComposer.channelIdFromHex(channelId) }
    // The GIF tab, through the GIF proxy, as in a chat.
    val gifClient = remember(channelId) {
        me.lakko.app.media.GifClient(sessionToken = { me.lakko.app.auth.SessionTokens.provider?.peek()?.accessToken })
    }
    androidx.compose.runtime.DisposableEffect(gifClient) { onDispose { gifClient.close() } }
    val gifModel = remember(channelId) {
        me.lakko.app.media.GifPickerModel(scope = scope, fetch = { q, o -> gifClient.page(q, o) })
    }
    val reader = remember(channelId) { ChannelReader(context.applicationContext, state, client, channelId) }
    val feedback = remember(channelId) { ChannelFeedback(context.applicationContext, state, client, channelId, reader) }
    var timeline by remember(channelId) { mutableStateOf<ChannelTimeline?>(null) }
    var phase by remember(channelId) { mutableStateOf(if (isPublisher) ReaderPhase.Active else ReaderPhase.Registering) }
    var failure by remember(channelId) { mutableStateOf<UiText?>(null) }
    var attempt by remember(channelId) { mutableIntStateOf(0) }
    val chips = remember(channelId) { mutableStateMapOf<Long, List<ChannelFeedback.Chip>>() }
    // Section 13a (phase B) — the comment count the service keeps per post.
    val commentCounts = remember(channelId) { mutableStateMapOf<Long, Long>() }
    var refreshTick by remember(channelId) { mutableIntStateOf(0) }

    suspend fun readNow() {
        try {
            val read = reader.read()
            timeline = read
            if (!isPublisher) {
                phase = if (read.posts.isNotEmpty() || state.subscribedSeeds(channelId).isNotEmpty() ||
                    reader.seedFor(state.epochByChannel.value[channelId] ?: 0u) != null
                ) {
                    ReaderPhase.Active
                } else {
                    phase.takeIf { it != ReaderPhase.Registering } ?: ReaderPhase.Waiting
                }
            }
            try {
                chips.putAll(feedback.chips(read.posts.filter { !it.unavailable }.map { it.cursor }))
                for (p in read.posts) feedback.commentCount(p.cursor)?.let { commentCounts[p.cursor] = it }
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                // Chips keep their last counts.
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            if (me.lakko.app.channels.ChannelRemovalWatch.isDeleted(e)) {
                // Section 15 — the owner deleted the channel.
                val name = state.channels.value.firstOrNull { it.id == channelId }
                    ?.let { it.sharedName.ifBlank { it.localName } }.orEmpty()
                me.lakko.app.channels.ChannelRemovalWatch.endEverywhere(state, channelId, name, null)
                return
            }
            Log.w(TAG, "channel read failed", e)
            if (timeline == null) failure = UiText("channels.posts.error.load")
        }
    }

    // A subscriber: register (once), catch up on its envelopes, then read
    // every few seconds while open. A publisher only reads.
    LaunchedEffect(channelId, attempt) {
        failure = null
        if (!isPublisher) {
            try {
                // docs/ux/second-device.md section 4a — the account's one key
                // for the channel when another device subscribed it.
                val kp = me.lakko.app.channels.ChannelSubscriptionSync.keyForSubscribing(state, channelId)
                val pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
                if (register || attempt > 0) {
                    phase = ReaderPhase.Registering
                    client.subscribe(channelId, pubHex)
                    onRegistered()
                }
                catchUpEnvelopes(state, client, channelId, pubHex)
                phase = ReaderPhase.Waiting
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "subscribe failed", e)
                failure = me.lakko.app.chat.anonTokenFailureText(e, me.lakko.app.chat.AllowanceKind.SUBSCRIBE)
                    ?: UiText("channels.subscribe.error.prefix")
                phase = ReaderPhase.Failed
                return@LaunchedEffect
            }
        }
        readNow()
        // ADR-0060 section 3 — the reader's card is uploaded once a seed of
        // the current epoch is held: a private channel's subscriber holds
        // none until the owner's device lets it in, so the card goes when the
        // seed arrives and again after each rotation (base walk 2026-11-13).
        var cardEpoch: UInt? = null
        suspend fun refreshReaderCardIfDue() {
            val epoch = state.epochByChannel.value[channelId]
            val held = epoch?.takeIf { state.subscribedSeeds(channelId)[it] != null }
            if (!me.lakko.app.channels.ChannelSubscriberListView.readerCardDue(cardEpoch, held)) return
            if (me.lakko.app.channels.ChannelMemberCards.refreshReaderCard(context, state, client, channelId)) cardEpoch = held
        }
        if (!isPublisher) {
            refreshReaderCardIfDue()
        } else {
            me.lakko.app.channels.ChannelMemberCards.refreshPublisherCard(context, state, client, channelId)
        }
        var ticks = 0
        while (true) {
            delay(if (phase == ReaderPhase.Active) READ_INTERVAL_MS else SEED_WAIT_MS)
            ticks += 1
            if (!isPublisher && (phase != ReaderPhase.Active || ticks % ENVELOPE_RECHECK_EVERY == 0)) {
                state.subscriberKeysByChannel.value[channelId]?.let {
                    catchUpEnvelopes(state, client, channelId, ChannelsComposer.bytesToHex(it.publicKey))
                }
                refreshReaderCardIfDue()
            }
            readNow()
        }
    }
    LaunchedEffect(refreshTick) { if (refreshTick > 0) readNow() }

    // ---- the publisher's composer (section 13: what a message can carry)
    val mediaHub = remember(channelId) { me.lakko.app.media.ChatMediaHub(context, scope) }
    var draft by rememberSaveable(channelId) { mutableStateOf("") }
    var draftRevision by remember { mutableIntStateOf(0) }
    var sending by remember { mutableStateOf(false) }
    var sendError by remember { mutableStateOf<UiText?>(null) }
    val pending = remember { mutableStateOf<List<AttachmentRef>>(emptyList()) }
    val inFlight = remember { mutableStateOf<List<UploadProgress>>(emptyList()) }
    var emojiInsert by remember { mutableStateOf<Pair<Long, String>?>(null) }
    var emojiSeq by remember { mutableStateOf(0L) }
    var pickerOpen by remember { mutableStateOf(false) }
    var editing by remember { mutableStateOf<ChannelPostItem?>(null) }
    var deleting by remember { mutableStateOf<ChannelPostItem?>(null) }

    suspend fun publish(plaintext: ByteArray) {
        val keys = state.keysByChannel.value[channelId] ?: throw IllegalStateException("not a publisher")
        val current = state.epochByChannel.value[channelId]
            ?: client.fetchEpoch(channelId).also { state.setEpoch(channelId, it) }
        // Section 15 — sealed under an epoch the channel service has reached;
        // with no seed of one on this device the post is refused (a retry
        // goes out once the owner's reseal lands).
        val sealing = state.sealingSeed(channelId, current)
            ?: throw IllegalStateException("no seed of an epoch the channel has reached")
        val wire = ChannelsComposer.compose(
            keys = keys.copy(seed = sealing.seed),
            channelIdHex = channelId,
            epochId = sealing.epoch,
            messageCounter = state.nextCounter(channelId),
            plaintext = plaintext,
        )
        val pub = state.publisher ?: throw IllegalStateException("channel publisher identity not available")
        ChannelPublisherSigners.derive(pub, channelId).use { client.publishPost(channelId, wire, it) }
        me.lakko.app.feed.requestChannelPostSync()
    }

    val mediaQueue = remember(channelId) {
        me.lakko.app.media.MediaSendQueue(
            scope = scope,
            uploadThumb = { poster ->
                AttachmentBridge.encryptAndUploadThumb(
                    bytes = poster.bytes,
                    mime = poster.mime,
                    width = poster.width.toUInt(),
                    height = poster.height.toUInt(),
                )
            },
            uploadMain = { bytes, mime, fileName, meta, onProgress ->
                AttachmentBridge.encryptAndUpload(bytes = bytes, mime = mime, name = fileName, media = meta, onProgress = onProgress, idleDays = CHANNEL_FILE_IDLE_DAYS)
            },
            publish = { ref, _, onRowShown ->
                publish(uniffi.lakko_ffi_uniffi.composeEnvelope("", listOf(ref)))
                onRowShown()
                refreshTick += 1
            },
            onLog = { line -> Log.w(TAG, line) },
            deleteUnsent = { ids -> me.lakko.app.storage.BlobLifetime.delete(context, ids) },
        )
    }
    val pendingMedia by mediaQueue.rows.collectAsState()
    val recorder = rememberRecordController(mediaHub) { take ->
        val stamp = System.currentTimeMillis()
        val voice = take.kind == me.lakko.app.media.CaptureKind.VOICE
        val file = take.file
        mediaQueue.enqueue(
            me.lakko.app.media.MediaSendQueue.Job(
                tempId = mediaQueue.newTempId(),
                kind = if (voice) me.lakko.app.media.KIND_VOICE else me.lakko.app.media.KIND_VIDEO_NOTE,
                mime = take.mime,
                name = if (voice) "voice-$stamp.m4a" else "video-note-$stamp.mp4",
                durationMs = take.durationMs,
                waveform = take.waveform,
                width = take.width,
                height = take.height,
                poster = take.poster,
                loadBytes = {
                    kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                        try {
                            if (file.length() > AttachmentBridge.MAX_ATTACHMENT_BYTES) null else file.readBytes() to take.mime
                        } catch (_: java.io.IOException) {
                            null
                        }
                    }
                },
            ),
        )
    }

    fun uploadPicked(uris: List<android.net.Uri>, audio: Boolean = false, visual: Boolean = false) {
        for (uri in uris) {
            scope.launch {
                val id = System.nanoTime()
                val fileName = uri.lastPathSegment ?: "file"
                inFlight.value = inFlight.value + UploadProgress(id = id, fileName = fileName, stage = null)
                try {
                    val resolver = context.contentResolver
                    val mime = resolver.getType(uri)
                    val bytes = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                        resolver.openInputStream(uri)?.use { it.readBytes() }
                    } ?: throw java.io.IOException("unreadable")
                    val ref = if (audio) {
                        // ADR-0092 / UX 6 — the file's own tags, read here.
                        val tags = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                            me.lakko.app.media.readAudioTags(context, uri)
                        }
                        val cover = tags.cover?.let { art ->
                            runCatching {
                                AttachmentBridge.encryptAndUploadThumb(art.bytes, "image/jpeg", art.width.toUInt(), art.height.toUInt())
                            }.getOrNull()
                        }
                        AttachmentBridge.encryptAndUpload(
                            bytes = bytes,
                            mime = mime,
                            name = fileName,
                            media = me.lakko.app.chat.AttachmentMediaMeta(kind = "audio", durationMs = tags.durationMs, thumb = cover),
                            idleDays = CHANNEL_FILE_IDLE_DAYS,
                        ).copy(title = tags.title ?: fileName.substringBeforeLast('.'), artist = tags.artist)
                    } else if (visual && mime?.startsWith("image/") == true && mime != "image/gif") {
                        // ADR-0092 / UX 3 — a photo re-encoded, its metadata dropped.
                        val max = runCatching { uniffi.lakko_ffi_uniffi.photoMaxLongestSidePx().toInt() }.getOrDefault(2560)
                        val jpeg = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Default) {
                            me.lakko.app.media.reencodePhoto(bytes, max)
                        } ?: throw java.io.IOException("unreadable photo")
                        AttachmentBridge.encryptAndUpload(bytes = jpeg, mime = "image/jpeg", name = fileName.substringBeforeLast('.') + ".jpg", idleDays = CHANNEL_FILE_IDLE_DAYS)
                    } else {
                        AttachmentBridge.encryptAndUpload(bytes = bytes, mime = mime, name = fileName, idleDays = CHANNEL_FILE_IDLE_DAYS)
                    }
                    pending.value = pending.value + ref
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    Log.w(TAG, "attachment upload failed", e)
                    sendError = UiText("channels.publish.error.generic")
                } finally {
                    inFlight.value = inFlight.value.filterNot { it.id == id }
                }
            }
        }
    }
    // ADR-0092 / attachments UX 3-4 — the same send preview as a chat: each
    // item its own post, the caption on the first, `Send as file`, the
    // video rule; a channel file lives CHANNEL_FILE_IDLE_DAYS idle days.
    var channelPreview by remember { mutableStateOf<me.lakko.app.ui.chat.SendPreview?>(null) }
    val previewIds = remember { java.util.concurrent.atomic.AtomicLong(1) }
    fun openPreview(uris: List<android.net.Uri>, filesOnly: Boolean) {
        if (uris.isEmpty()) return
        scope.launch {
            val items = me.lakko.app.ui.chat.buildPreviewItems(context, uris, filesOnly) { previewIds.getAndIncrement() }
            val tooBig = items.filter { it.problemKey == "attach.preview.file_too_big" }
            if (tooBig.isNotEmpty()) sendError = UiText("attach.preview.file_too_big")
            val kept = items - tooBig.toSet()
            if (kept.isNotEmpty()) channelPreview = me.lakko.app.ui.chat.SendPreview(kept, filesOnly = filesOnly)
        }
    }
    fun sendPreview(p: me.lakko.app.ui.chat.SendPreview) {
        channelPreview = null
        val maxSide = runCatching { uniffi.lakko_ffi_uniffi.photoMaxLongestSidePx().toInt() }.getOrDefault(2560)
        scope.launch {
            for ((item, caption) in me.lakko.app.ui.chat.previewSendOrder(p.items.filter { it.problemKey == null }, p.caption)) {
                try {
                    val ref = me.lakko.app.ui.chat.uploadPreviewItem(context, item, p.sendAsFile, maxSide, CHANNEL_FILE_IDLE_DAYS) ?: continue
                    publish(uniffi.lakko_ffi_uniffi.composeEnvelope(caption, listOf(ref)))
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    Log.w(TAG, "preview item not published", e)
                    sendError = channelsErrorText(e, "channels.publish.error.generic", "channels.publish.error.prefix")
                }
            }
            readNow()
        }
    }
    channelPreview?.let { p ->
        me.lakko.app.ui.chat.SendPreviewScreen(
            preview = p,
            onCaption = { channelPreview = p.copy(caption = it) },
            onSendAsFile = { channelPreview = p.withSendAsFile(it) },
            onRemove = { channelPreview = p.without(it) },
            onSend = { sendPreview(p) },
            onClose = { channelPreview = null },
        )
    }
    val pickVisual = rememberLauncherForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { uris ->
        openPreview(uris, filesOnly = false)
    }
    val pickAudio = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        uploadPicked(uris, audio = true)
    }
    val pickFiles = rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        openPreview(uris, filesOnly = true)
    }
    // ADR-0092 / attachments UX section 2 — the publisher's one menu.
    var attachMenuOpen by remember { mutableStateOf(false) }
    var channelContent by remember { mutableStateOf<ChannelContentScreen?>(null) }
    fun publishContent(content: uniffi.lakko_ffi_uniffi.MessageContent, refs: List<AttachmentRef> = emptyList()) {
        scope.launch {
            try {
                publish(
                    uniffi.lakko_ffi_uniffi.composeContentEnvelope(
                        content,
                        refs,
                        uniffi.lakko_ffi_uniffi.ContentEnvelopeParts(),
                    ),
                )
                readNow()
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "content publish failed", e)
                sendError = channelsErrorText(e, "channels.publish.error.generic", "channels.publish.error.prefix")
            }
        }
    }
    ChannelAttachSurfaces(
        menuOpen = attachMenuOpen,
        onMenuDismiss = { attachMenuOpen = false },
        isOwner = true,
        screen = channelContent,
        onScreen = { channelContent = it },
        contactsRepository = contactsRepository,
        onPickVisual = {
            pickVisual.launch(
                androidx.activity.result.PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo),
            )
        },
        onPickFiles = { pickFiles.launch(arrayOf("*/*")) },
        onPickAudio = { pickAudio.launch(arrayOf("audio/*")) },
        onCaptured = { uri -> openPreview(listOf(uri), filesOnly = false) },
        onPublish = ::publishContent,
        draftKey = channelId,
    )

    // ADR-0092 section 4 — a channel poll's card: counts from the channels
    // service, one vote per account through the anonymous credential; a
    // publisher closes it by editing the post with the frozen tally.
    var failedVotes by remember { mutableStateOf<Map<Long, ByteArray>>(emptyMap()) }
    var pollTick by remember { mutableIntStateOf(0) }
    fun channelVote(post: ChannelPostItem, choices: ByteArray) {
        val body = (post.content as? uniffi.lakko_ffi_uniffi.MessageContent.Poll)?.poll?.body ?: return
        scope.launch {
            val ok = try {
                feedback.votePoll(post.cursor, choices, body.options.size)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                Log.w(TAG, "channel vote not sent (${e.javaClass.simpleName})")
                false
            }
            failedVotes = if (ok) failedVotes - post.cursor else failedVotes + (post.cursor to choices)
            try {
                chips.putAll(feedback.chips(listOf(post.cursor)))
            } catch (_: Throwable) {
            }
            pollTick += 1
        }
    }
    val channelContentCtx: (ChannelPostItem) -> me.lakko.app.ui.chat.ContentBubbleContext? = { post ->
        if (post.content == null) {
            null
        } else {
            me.lakko.app.ui.chat.ContentBubbleContext(
                groupId = null,
                ownKey = null,
                isOwnerOrAdmin = isPublisher,
                isOneToOne = false,
                nameOf = { null },
                onVote = { _, choices -> channelVote(post, choices) },
                onRetryVote = { _ -> failedVotes[post.cursor]?.let { channelVote(post, it) } },
                onViewResults = {},
                onTick = { target, item, done ->
                    scope.launch {
                        runCatching { publish(uniffi.lakko_ffi_uniffi.composeChecklistTickEnvelope(target, item, done)); readNow() }
                            .onFailure { if (it is kotlinx.coroutines.CancellationException) throw it }
                    }
                },
                onAddItem = { target, text ->
                    scope.launch {
                        runCatching { publish(uniffi.lakko_ffi_uniffi.composeChecklistAddEnvelope(target, text.trim())); readNow() }
                            .onFailure { if (it is kotlinx.coroutines.CancellationException) throw it }
                    }
                },
                banner = me.lakko.app.ui.chat.articleBanner(mediaHub),
                onOpenLocation = { loc -> me.lakko.app.ui.chat.openGeo(context, loc) },
                contactCardAction = { me.lakko.app.ui.chat.ContactCardAction.ADD },
                onContactCard = { _, _ -> },
                onOpenArticle = {},
                channel = true,
                onClose = { msg ->
                    val body = (post.content as? uniffi.lakko_ffi_uniffi.MessageContent.Poll)?.poll?.body
                    scope.launch {
                        try {
                            // A checklist closes with no tally.
                            val tally = body?.let { feedback.currentTallyBytes(post.cursor, it.options.size) }
                            publish(uniffi.lakko_ffi_uniffi.composePollCloseEnvelope(msg.messageId, tally))
                            readNow()
                        } catch (e: kotlinx.coroutines.CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            Log.w(TAG, "poll close not published (${e.javaClass.simpleName})")
                        }
                    }
                },
                pollViewOverride = { _ ->
                    pollTick.let { }
                    (post.content as? uniffi.lakko_ffi_uniffi.MessageContent.Poll)?.poll?.body?.let { body ->
                        feedback.pollView(post.cursor, body, post.closed, post.frozenTally, System.currentTimeMillis())
                            .let { v -> if (post.cursor in failedVotes) v.copy(ownBallotStatus = me.lakko.app.chat.OwnBallotStatus.Failed) else v }
                    }
                },
            )
        }
    }
    Column(Modifier.fillMaxSize()) {
        BoxWithConstraints(Modifier.weight(1f).fillMaxWidth()) {
            val width = maxWidth - 24.dp
            val items = timeline?.posts.orEmpty().asReversed()
            val listState = androidx.compose.foundation.lazy.rememberLazyListState()
            // docs/ux/feed.md section 5 — the list at its newest end, in the
            // foreground: the newest post shown is read and the channel's
            // count in Chats clears (as a group chat does, and as web and iOS).
            val lifecycle = androidx.lifecycle.compose.LocalLifecycleOwner.current.lifecycle
            val newestCursor = timeline?.let { me.lakko.app.channels.channelReadThroughOnScreen(it) }
            LaunchedEffect(channelId, newestCursor, pendingMedia.size) {
                val cursor = newestCursor ?: return@LaunchedEffect
                val account = state.boundAccount ?: return@LaunchedEffect
                lifecycle.repeatOnLifecycle(androidx.lifecycle.Lifecycle.State.RESUMED) {
                    androidx.compose.runtime.snapshotFlow {
                        me.lakko.app.channels.channelListAtNewestEnd(listState.firstVisibleItemIndex, pendingMedia.size)
                    }.collect { atEnd ->
                        if (atEnd) {
                            me.lakko.app.feed.FeedReadingStore.update(context, account) {
                                me.lakko.app.feed.advanceChannelRead(it, channelId, cursor)
                            }
                        }
                    }
                }
            }
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                state = listState,
                reverseLayout = true,
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                items(items = pendingMedia, key = { "pending-${it.tempId}" }) { row ->
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.Start) {
                        PendingMediaBubble(
                            row = row,
                            onRetry = { mediaQueue.retry(row.tempId) },
                            onDiscard = { mediaQueue.discard(row.tempId) },
                        )
                    }
                }
                itemsIndexed(items = items, key = { _, it -> it.cursor }) { index, post ->
                    // One Column: in a reversed list the parts of an item are
                    // reversed too, which drew the day separator under its post.
                    Column(Modifier.fillMaxWidth()) {
                        val older = items.getOrNull(index + 1)
                        val zone = java.time.ZoneId.systemDefault()
                        if (post.tsMs > 0 && (older == null || older.tsMs <= 0 ||
                                java.time.Instant.ofEpochMilli(older.tsMs).atZone(zone).toLocalDate() !=
                                java.time.Instant.ofEpochMilli(post.tsMs).atZone(zone).toLocalDate())
                        ) {
                            me.lakko.app.ui.chat.DateSeparatorPill(
                                me.lakko.app.ui.chat.dateSeparatorLabel(
                                    post.tsMs,
                                    System.currentTimeMillis(),
                                    zone,
                                    java.util.Locale.getDefault(),
                                ),
                            )
                        }
                        if (post.unavailable) {
                            Text(
                                t("channels.post.unavailable"),
                                style = MaterialTheme.typography.bodyMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(8.dp),
                            )
                        } else {
                            PostBlock(
                                contentCtx = channelContentCtx(post),
                                commentCount = commentCounts[post.cursor],
                                post = post,
                                width = width,
                                isPublisher = isPublisher,
                                mediaHub = mediaHub,
                                chips = chips[post.cursor].orEmpty(),
                                feedback = feedback,
                                onToggleReaction = { emoji ->
                                    scope.launch {
                                        try {
                                            feedback.toggle(post.cursor, emoji)
                                        } catch (e: kotlinx.coroutines.CancellationException) {
                                            throw e
                                        } catch (e: Throwable) {
                                            // Section 13.1 — nothing is refused and no
                                            // error shows; the own record stays.
                                            Log.i(TAG, "reaction write not sent yet")
                                        }
                                        try {
                                            chips.putAll(feedback.chips(listOf(post.cursor)))
                                        } catch (_: Throwable) {
                                        }
                                    }
                                },
                                onEdit = { editing = post },
                                onDelete = { deleting = post },
                                saved = me.lakko.app.chat.isSavedIn(
                                    savedList,
                                    me.lakko.app.chat.SavedKey(channelId.lowercase(), post.cursor),
                                ),
                                canSave = savedSync != null,
                                onToggleSaved = { msg ->
                                    val sync = savedSync ?: return@PostBlock
                                    scope.launch {
                                        try {
                                            val ref = uniffi.lakko_ffi_uniffi.FolderConversationRef(
                                                kind = uniffi.lakko_ffi_uniffi.FolderConversationKind.CHANNEL,
                                                id = channelBytes,
                                            )
                                            val removing = me.lakko.app.chat.isSavedIn(
                                                sync.saved.value,
                                                me.lakko.app.chat.SavedKey(channelId.lowercase(), post.cursor),
                                            )
                                            val register = if (removing) {
                                                uniffi.lakko_ffi_uniffi.composeSavedRemoval(
                                                    me.lakko.app.devices.sync.Hlc.mint(),
                                                    ref,
                                                    post.cursor.toULong(),
                                                )
                                            } else {
                                                val max = uniffi.lakko_ffi_uniffi.savedMaxClient().toInt()
                                                if (!me.lakko.app.chat.saveAllowed(sync.saved.value.size, 1, max)) {
                                                    throw IllegalStateException("saved list is full")
                                                }
                                                uniffi.lakko_ffi_uniffi.composeSavedMessage(
                                                    hlc = me.lakko.app.devices.sync.Hlc.mint(),
                                                    conversation = ref,
                                                    messageId = post.cursor.toULong(),
                                                    savedAtMs = System.currentTimeMillis().toULong(),
                                                    sentAtMs = post.tsMs.coerceAtLeast(0L).toULong(),
                                                    author = channelName,
                                                    excerpt = me.lakko.app.chat.savedExcerpt(msg),
                                                    attachmentKind = me.lakko.app.chat.savedAttachmentKind(msg),
                                                    blobIds = me.lakko.app.chat.messageBlobIds(msg),
                                                )
                                            }
                                            sync.applySavedChanges(listOf(register))
                                        } catch (e: kotlinx.coroutines.CancellationException) {
                                            throw e
                                        } catch (e: Throwable) {
                                            Log.w(TAG, "save failed: ${e.javaClass.simpleName}")
                                            sendError = UiText("chat.error.save")
                                        }
                                    }
                                },
                                onForward = onForwardPost?.let { forward ->
                                    { msg ->
                                        // Web's shape: the channel's name as the
                                        // author, no origin to jump to.
                                        forward(
                                            me.lakko.app.chat.ForwardRequest(
                                                sourceGroupIdHex = channelId,
                                                message = msg,
                                                authorLabel = channelName,
                                                sourceIsOneToOne = true,
                                            ),
                                        )
                                    }
                                },
                            )
                        }
                    }
                }
                if (timeline?.earlierUnavailable == true) {
                    item("earlier") {
                        Text(
                            t("channels.posts.earlier_unavailable"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.fillMaxWidth().padding(8.dp),
                        )
                    }
                }
                if (items.isEmpty() && pendingMedia.isEmpty()) {
                    item("empty") {
                        Box(Modifier.fillMaxWidth().padding(vertical = 24.dp), contentAlignment = Alignment.Center) {
                            when {
                                failure != null -> Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                    Text(failure!!.text(), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodyMedium)
                                    TextButton(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
                                }
                                phase == ReaderPhase.Registering -> Text(t("channels.subscribe.status.subscribing"))
                                phase == ReaderPhase.Waiting && !publicChannel ->
                                    Text(
                                        t("channels.link_page.waiting"),
                                        style = MaterialTheme.typography.bodyMedium,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                timeline == null -> CircularProgressIndicator()
                                else -> Text(
                                    if (isPublisher) t("channels.posts.empty_owner") else t("channels.posts.empty"),
                                    style = MaterialTheme.typography.bodyMedium,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                        }
                    }
                }
            }
        }
        if (isPublisher) {
            // Section 12 — the owner and publishers have the group's
            // composer. A subscriber has none: the timeline runs to the
            // bottom, and they write under a post.
            Box(Modifier.imePadding()) {
                Composer(
                    recorder = recorder,
                    emojiInsert = emojiInsert,
                    onEmojiInserted = { emojiInsert = null },
                    onOpenPicker = { pickerOpen = true },
                    draft = draft,
                    draftRevision = draftRevision,
                    sending = sending,
                    sendError = sendError,
                    pendingAttachments = pending.value,
                    uploading = inFlight.value.isNotEmpty(),
                    inFlightUploads = inFlight.value,
                    pendingPreviews = emptyMap(),
                    onDraftChange = { draft = it },
                    onSendClicked = {
                        val text = draft.trim()
                        val refs = pending.value
                        if (sending || (text.isEmpty() && refs.isEmpty())) return@Composer
                        sending = true
                        sendError = null
                        scope.launch {
                            try {
                                publish(uniffi.lakko_ffi_uniffi.composeEnvelope(text, refs))
                                draft = ""
                                draftRevision += 1
                                pending.value = emptyList()
                                readNow()
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                Log.w(TAG, "publish failed", e)
                                sendError = if (e is me.lakko.app.channels.ChannelsApiException && e.status == 401) {
                                    UiText("channels.post.not_publisher")
                                } else {
                                    channelsErrorText(e, "channels.publish.error.generic", "channels.publish.error.prefix")
                                }
                            } finally {
                                sending = false
                            }
                        }
                    },
                    onFocusChanged = {},
                    onPickMedia = {
                        pickVisual.launch(
                            androidx.activity.result.PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo),
                        )
                    },
                    onAttachClicked = { attachMenuOpen = true },
                    onRemoveAttachment = { index ->
                        pending.value = pending.value.filterIndexed { i, _ -> i != index }
                    },
                )
            }
        } else {
            // docs/ux/second-device.md section 4a — one line in secondary
            // text where the composer would be: no button, no link.
            readerRole?.let { me.lakko.app.channels.readerDeviceLineKey(it) }?.let { key ->
                Text(
                    t(key),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    textAlign = androidx.compose.ui.text.style.TextAlign.Center,
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
                )
            }
        }
    }
    if (isPublisher) VideoNoteOverlay(recorder)

    if (pickerOpen) {
        ChannelEmojiPicker(
            mediaHub = mediaHub,
            gifModel = if (isPublisher) gifModel else null,
            loadGifPreview = { url ->
                (gifClient.media(url, me.lakko.app.media.MAX_GIF_PREVIEW_BYTES) as? me.lakko.app.media.GifResult.Ok)
                    ?.value?.bytes
            },
            onGif = { item ->
                pickerOpen = false
                mediaQueue.enqueue(
                    me.lakko.app.media.MediaSendQueue.Job(
                        tempId = mediaQueue.newTempId(),
                        kind = me.lakko.app.media.KIND_GIF,
                        mime = "image/gif",
                        name = "gif-${System.currentTimeMillis()}.gif",
                        durationMs = null,
                        waveform = null,
                        width = item.width,
                        height = item.height,
                        poster = null,
                        loadBytes = {
                            when (val r = gifClient.media(item.fullUrl, me.lakko.app.media.MAX_GIF_FULL_BYTES)) {
                                is me.lakko.app.media.GifResult.Ok -> r.value.bytes to r.value.mime
                                else -> null
                            }
                        },
                    ),
                )
            },
            onEmoji = { emoji ->
                emojiSeq += 1
                emojiInsert = emojiSeq to emoji
            },
            onDismiss = { pickerOpen = false },
        )
    }

    editing?.let { post ->
        EditMessageDialog(
            initialText = post.text,
            onCancel = { editing = null },
            onSave = { newText ->
                editing = null
                scope.launch {
                    try {
                        publish(uniffi.lakko_ffi_uniffi.composeEditEnvelope(post.cursor, newText))
                        readNow()
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(TAG, "edit failed", e)
                        sendError = UiText("channels.publish.error.generic")
                    }
                }
            },
        )
    }
    deleting?.let { post ->
        LakkoDialog(
            onDismissRequest = { deleting = null },
            title = t("chat.select.delete_title", emptyMap(), mapOf("count" to 1uL)),
            confirm = LakkoDialogAction(t("common.delete"), LakkoDialogTone.Danger) {
                deleting = null
                scope.launch {
                    try {
                        publish(uniffi.lakko_ffi_uniffi.composeDeleteEnvelope(post.cursor))
                        readNow()
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: Throwable) {
                        Log.w(TAG, "delete failed", e)
                        sendError = UiText("chat.error.delete_message")
                    }
                }
            },
        )
    }
    androidx.compose.runtime.DisposableEffect(channelId) {
        onDispose { mediaHub.dispose() }
    }
}

/** ADR-0031 catch-up: open every retained envelope of this subscriber key
 *  into the per-channel seed map. Best effort per envelope. A removal from a
 *  private channel (section 9.5) ends it on this device. */
private suspend fun catchUpEnvelopes(state: ChannelsUiState, client: ChannelsClient, channelId: String, pubHex: String) {
    val kp = state.subscriberKeysByChannel.value[channelId] ?: return
    val retained = try {
        client.fetchEnvelopes(channelId, pubHex)
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Throwable) {
        val channel = state.channels.value.firstOrNull { it.id == channelId }
        val name = channel?.let { it.sharedName.ifBlank { it.localName } }.orEmpty()
        if (me.lakko.app.channels.ChannelRemovalWatch.isDeleted(e)) {
            me.lakko.app.channels.ChannelRemovalWatch.endEverywhere(state, channelId, name, null)
        } else if (me.lakko.app.channels.ChannelRemovalWatch.isRemoval(e)) {
            me.lakko.app.channels.ChannelRemovalWatch.ended(state, channelId, name) { id, n ->
                me.lakko.app.channels.ChannelRemovalWatch.notice?.invoke(id, n)
            }
        }
        emptyList()
    }
    for ((epoch, wire) in retained) {
        try {
            val seed = ChannelsComposer.openSubscriberSeedEnvelope(keys = kp, channelIdHex = channelId, epochId = epoch, wire = wire)
            state.setSubscribedSeed(channelId, epoch, seed)
        } catch (_: ChannelsComposerException) {
            // A wrong-epoch envelope is skipped, never a wrong seed.
        } catch (_: Throwable) {
        }
    }
    try {
        state.setEpoch(channelId, client.fetchEpoch(channelId))
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (_: Throwable) {
    }
}

/** A post: the bubble at the width of the pane, its reaction chips, then
 *  its comments (section 13a). */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun PostBlock(
    commentCount: Long?,
    post: ChannelPostItem,
    width: androidx.compose.ui.unit.Dp,
    isPublisher: Boolean,
    mediaHub: me.lakko.app.media.ChatMediaHub,
    chips: List<ChannelFeedback.Chip>,
    feedback: ChannelFeedback,
    onToggleReaction: (String) -> Unit,
    onEdit: () -> Unit,
    onDelete: () -> Unit,
    saved: Boolean = false,
    canSave: Boolean = false,
    onToggleSaved: (ChatMessage) -> Unit = {},
    onForward: ((ChatMessage) -> Unit)? = null,
    /** ADR-0092 — what a content post's card answers to. */
    contentCtx: me.lakko.app.ui.chat.ContentBubbleContext? = null,
) {
    val context = LocalContext.current
    val time = remember(post.tsMs) {
        android.text.format.DateFormat.getTimeFormat(context).format(java.util.Date(post.tsMs))
    }
    val msg = remember(post) {
        ChatMessage(
            messageId = post.cursor,
            direction = Direction.Incoming,
            text = post.text,
            attachments = post.attachments,
            createdAt = java.time.Instant.ofEpochMilli(post.tsMs).toString(),
            edited = post.edited,
            content = post.content,
            authorKey = me.lakko.app.channels.CHANNEL_PUBLISHER_KEY,
            contentExtras = me.lakko.app.chat.ContentExtras(ops = post.ops),
        )
    }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
        androidx.compose.runtime.CompositionLocalProvider(me.lakko.app.ui.chat.LocalContentBubble provides contentCtx) {
        Bubble(
            msg = msg,
            time = time,
            maxBubbleWidth = width,
            mediaHub = mediaHub,
            onOpenReactionPicker = {},
            ownIdentity = null,
            isOwn = false,
            actionsAvailable = true,
            readState = OwnReadState.None,
            background = MaterialTheme.colorScheme.surfaceVariant,
            contentColor = MaterialTheme.colorScheme.onSurfaceVariant,
            onPreview = {},
            onSave = {},
            onEdit = onEdit,
            onDelete = onDelete,
            onReact = onToggleReaction,
            onRetrySend = {},
            onForward = { onForward?.invoke(msg) },
            savedMessage = saved,
            onToggleSaved = { onToggleSaved(msg) },
            channelPost = ChannelPostActions(
                publisher = isPublisher,
                canForward = onForward != null,
                canSave = canSave,
            ),
        )
        }
        if (chips.isNotEmpty()) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                for (chip in chips) {
                    Surface(
                        onClick = { onToggleReaction(chip.emoji) },
                        shape = RoundedCornerShape(12.dp),
                        color = if (chip.own) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surfaceVariant,
                    ) {
                        Text(
                            "${chip.emoji} ${chip.count}",
                            style = MaterialTheme.typography.labelMedium,
                            modifier = Modifier.padding(horizontal = 8.dp, vertical = 4.dp),
                        )
                    }
                }
            }
        }
        CommentsBlock(post = post, isPublisher = isPublisher, feedback = feedback, count = commentCount, mediaHub = mediaHub)
    }
}

/**
 * Section 13.2 — comments under the post, in place: the first 2 and
 * `Show {n} more comments`, or a quiet `Comment` link; opened, a page at a
 * time with the field under it. New comments do not move an open thread: a
 * line says how many arrived.
 */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun CommentsBlock(
    post: ChannelPostItem,
    isPublisher: Boolean,
    feedback: ChannelFeedback,
    /** The comment count the service keeps for the post (`null` before it
     *  was read): what the collapsed thread says, not a count of a page. */
    count: Long?,
    mediaHub: me.lakko.app.media.ChatMediaHub,
) {
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val strings = LocalStrings.current
    var loaded by remember(post.cursor) { mutableStateOf<List<ChannelFeedback.Comment>>(emptyList()) }
    var nextAfter by remember(post.cursor) { mutableStateOf(0L) }
    var more by remember(post.cursor) { mutableStateOf(false) }
    // Rows the service returned, opened or not, and whether the first page
    // was read: what the collapsed count is judged by (section 13.2).
    var rowsRead by remember(post.cursor) { mutableIntStateOf(0) }
    var firstRead by remember(post.cursor) { mutableStateOf(false) }
    var open by remember(post.cursor) { mutableStateOf(false) }
    var loading by remember(post.cursor) { mutableStateOf(false) }
    var failed by remember(post.cursor) { mutableStateOf(false) }
    var field by remember(post.cursor) { mutableStateOf("") }
    var replyTo by remember(post.cursor) { mutableStateOf<ChannelFeedback.Comment?>(null) }
    var menuFor by remember(post.cursor) { mutableStateOf<ChannelFeedback.Comment?>(null) }
    // Each comment's rectangle on screen, for the message action menu.
    val commentBounds = remember(post.cursor) { mutableStateMapOf<Long, androidx.compose.ui.geometry.Rect>() }
    var sendFailed by remember(post.cursor) { mutableStateOf(false) }
    var arrived by remember(post.cursor) { mutableIntStateOf(0) }
    // Section 13a (phase B) — reactions on comments, as on posts.
    val commentChips = remember(post.cursor) { mutableStateMapOf<Long, List<ChannelFeedback.Chip>>() }
    var reactingTo by remember(post.cursor) { mutableStateOf<ChannelFeedback.Comment?>(null) }

    suspend fun loadChips(comments: List<ChannelFeedback.Comment>) {
        try {
            commentChips.putAll(feedback.commentChips(post.cursor, comments.map { it.cursor }))
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            // Chips keep their last counts.
        }
    }

    fun toggleCommentReaction(c: ChannelFeedback.Comment, emoji: String) {
        scope.launch {
            try {
                feedback.toggleCommentReaction(post.cursor, c.cursor, emoji)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                // Owed: the outbox sends it again.
                Log.i(TAG, "comment reaction write not sent yet")
            }
            loadChips(listOf(c))
        }
    }

    suspend fun loadFirst() {
        loading = true
        failed = false
        try {
            val page = feedback.comments(post.cursor, 0)
            if (open && loaded.isNotEmpty()) {
                arrived = (page.comments.size - loaded.size).coerceAtLeast(0)
            } else {
                loaded = page.comments
                rowsRead = page.rowsRead
            }
            firstRead = true
            nextAfter = page.nextAfter
            more = page.more
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            failed = true
        } finally {
            loading = false
        }
    }
    LaunchedEffect(post.cursor) {
        loadFirst()
        loadChips(loaded)
    }

    val shown = if (open) loaded else loaded.take(2)
    Column(Modifier.padding(start = 8.dp)) {
        if (loading && open) LinearProgressIndicator(Modifier.fillMaxWidth().height(2.dp))
        if (failed && open) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(t("channels.comments.failed"), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
                TextButton(onClick = { scope.launch { loadFirst() } }) { Text(t("common.try_again")) }
            }
        }
        for (c in shown) {
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .onGloballyPositioned { coords ->
                        commentBounds[c.cursor] = androidx.compose.ui.geometry.Rect(
                            coords.positionOnScreen(),
                            androidx.compose.ui.geometry.Size(coords.size.width.toFloat(), coords.size.height.toFloat()),
                        )
                    }
                    .combinedClickable(onClick = {}, onLongClick = { menuFor = c })
                    .padding(vertical = 2.dp),
                verticalAlignment = Alignment.Top,
            ) {
                val photo = remember(c.cursor) {
                    c.avatar?.let { ref ->
                        me.lakko.app.profile.AvatarRefs(
                            still = me.lakko.app.profile.avatarBlobOf(ref.blobId, ref.key, ref.nonce, ref.size.toLong(), ref.side.toInt()),
                        )
                    }
                }
                LakkoAvatar(
                    name = c.name,
                    seed = c.authorKey.joinToString("") { "%02x".format(it) },
                    size = 24.dp,
                    avatar = photo,
                )
                Spacer(Modifier.width(6.dp))
                Column {
                    Row {
                        Text(c.name, style = MaterialTheme.typography.labelLarge)
                        Spacer(Modifier.width(6.dp))
                        Text(c.text, style = MaterialTheme.typography.bodyMedium)
                    }
                    val reactions = commentChips[c.cursor].orEmpty()
                    if (reactions.isNotEmpty()) {
                        Row(horizontalArrangement = Arrangement.spacedBy(4.dp), modifier = Modifier.padding(top = 2.dp)) {
                            for (chip in reactions) {
                                Surface(
                                    onClick = { toggleCommentReaction(c, chip.emoji) },
                                    shape = RoundedCornerShape(10.dp),
                                    color = if (chip.own) {
                                        MaterialTheme.colorScheme.primaryContainer
                                    } else {
                                        MaterialTheme.colorScheme.surfaceVariant
                                    },
                                ) {
                                    Text(
                                        "${chip.emoji} ${chip.count}",
                                        style = MaterialTheme.typography.labelSmall,
                                        modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp),
                                    )
                                }
                            }
                        }
                    }
                }
            }
        }
        // Section 13.2 — never a count of comments this reader hides: the
        // service's count (13a, phase B) stands only for rows not read yet.
        if (!open) {
            when (val m = collapsedMore(loaded.size, rowsRead, firstRead, more, count)) {
                is CollapsedMore.Count -> TextButton(onClick = { open = true }) {
                    Text(strings.format("channels.comments.show_more", emptyMap(), mapOf("count" to m.n.toULong())))
                }
                CollapsedMore.Unknown -> TextButton(onClick = { open = true }) { Text(t("channels.comments.show_more_unknown")) }
                CollapsedMore.Comment -> TextButton(onClick = { open = true }) { Text(t("channels.comments.comment")) }
                CollapsedMore.None -> TextButton(onClick = { open = true }) { Text(t("channels.comments.write")) }
            }
        }
        if (open) {
            if (more) {
                TextButton(onClick = {
                    scope.launch {
                        try {
                            val page = feedback.comments(post.cursor, nextAfter)
                            loaded = loaded + page.comments
                            rowsRead += page.rowsRead
                            loadChips(page.comments)
                            nextAfter = page.nextAfter
                            more = page.more
                        } catch (e: kotlinx.coroutines.CancellationException) {
                            throw e
                        } catch (_: Throwable) {
                            failed = true
                        }
                    }
                }) { Text(t("channels.comments.more")) }
            }
            if (arrived > 0) {
                TextButton(onClick = {
                    arrived = 0
                    loaded = emptyList()
                    scope.launch { loadFirst() }
                }) { Text(strings.format("channels.comments.new", emptyMap(), mapOf("count" to arrived.toULong()))) }
            }
            replyTo?.let { r ->
                Text("${r.name}: ${r.text}", style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                OutlinedTextField(
                    value = field,
                    onValueChange = { field = it },
                    placeholder = { Text(t("channels.comments.write")) },
                    modifier = Modifier.weight(1f),
                    maxLines = 4,
                )
                IconButton(
                    enabled = field.isNotBlank(),
                    onClick = {
                        val text = field.trim()
                        scope.launch {
                            try {
                                feedback.comment(post.cursor, text, replyTo?.cursor)
                                field = ""
                                replyTo = null
                                sendFailed = false
                                loaded = emptyList()
                                loadFirst()
                            } catch (e: kotlinx.coroutines.CancellationException) {
                                throw e
                            } catch (e: Throwable) {
                                Log.w(TAG, "comment failed", e)
                                sendFailed = true
                            }
                        }
                    },
                ) {
                    LkIcon(
                        R.drawable.ic_lk_arrow_up_circle_fill,
                        contentDescription = t("chat.composer.send"),
                        tint = if (field.isNotBlank()) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant,
                        size = 31.dp,
                    )
                }
            }
            if (sendFailed) Text(t("channels.publish.error.generic"), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
            TextButton(onClick = { open = false }) { Text(t("channels.comments.hide")) }
        }
    }

    // The same action menu as chat messages and posts: the quick reactions on
    // top, then the actions that apply to a comment.
    menuFor?.let { c ->
        val bounds = commentBounds[c.cursor]
        if (bounds == null) {
            menuFor = null
        } else {
            val own = commentChips[c.cursor].orEmpty().filter { it.own }.map { it.emoji }.toSet()
            me.lakko.app.ui.chat.MessageActionMenuOverlay(
                bubbleBounds = bounds,
                alignEnd = false,
                actions = me.lakko.app.ui.chat.MessageActions(
                    canCopy = c.text.isNotEmpty(),
                    canEdit = false,
                    canDelete = c.own || isPublisher,
                    canReply = true,
                    canBan = isPublisher && !c.own,
                ),
                reactions = me.lakko.app.ui.chat.REACTION_EMOJIS,
                ownReactions = own,
                capReached = own.size >= me.lakko.app.ui.chat.MAX_OWN_REACTIONS,
                onReact = { emoji ->
                    menuFor = null
                    toggleCommentReaction(c, emoji)
                },
                onOpenReactionPicker = {
                    menuFor = null
                    reactingTo = c
                },
                onReply = {
                    menuFor = null
                    open = true
                    replyTo = c
                },
                onCopy = {
                    menuFor = null
                    copyMessageText(context, c.text)
                },
                onEdit = { menuFor = null },
                onDelete = {
                    menuFor = null
                    scope.launch {
                        try {
                            feedback.deleteComment(post.cursor, c.cursor, c.writerKey)
                            loaded = loaded.filterNot { it.cursor == c.cursor }
                        } catch (e: kotlinx.coroutines.CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            Log.w(TAG, "comment delete failed", e)
                        }
                    }
                },
                onBan = {
                    menuFor = null
                    scope.launch {
                        try {
                            feedback.ban(c.authorKey, banned = true)
                            loaded = loaded.filterNot { it.authorKey.contentEquals(c.authorKey) }
                        } catch (e: kotlinx.coroutines.CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            Log.w(TAG, "ban failed", e)
                        }
                    }
                },
                onDismiss = { menuFor = null },
            )
        }
    }
    reactingTo?.let { c ->
        val context = LocalContext.current
        var dataset by remember { mutableStateOf<me.lakko.app.media.EmojiDataset?>(null) }
        LaunchedEffect(Unit) {
            dataset = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                me.lakko.app.media.EmojiAssets.load(context)
            }
        }
        me.lakko.app.ui.chat.EmojiPickerSheet(
            mode = me.lakko.app.ui.chat.PickerMode.REACTION,
            dataset = dataset,
            prefs = mediaHub.prefs,
            gifModel = null,
            loadGifPreview = { null },
            onEmoji = { emoji ->
                reactingTo = null
                toggleCommentReaction(c, emoji)
            },
            onGif = {},
            onDismiss = { reactingTo = null },
        )
    }
}

/** The composer's emoji picker, with the GIF tab for a publisher. */
@Composable
private fun ChannelEmojiPicker(
    mediaHub: me.lakko.app.media.ChatMediaHub,
    gifModel: me.lakko.app.media.GifPickerModel?,
    loadGifPreview: suspend (String) -> ByteArray?,
    onGif: (me.lakko.app.media.GifItem) -> Unit,
    onEmoji: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    var dataset by remember { mutableStateOf<me.lakko.app.media.EmojiDataset?>(null) }
    LaunchedEffect(Unit) {
        dataset = kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
            me.lakko.app.media.EmojiAssets.load(context)
        }
    }
    me.lakko.app.ui.chat.EmojiPickerSheet(
        mode = me.lakko.app.ui.chat.PickerMode.COMPOSER,
        dataset = dataset,
        prefs = mediaHub.prefs,
        gifModel = gifModel,
        loadGifPreview = loadGifPreview,
        onEmoji = onEmoji,
        onGif = onGif,
        onDismiss = onDismiss,
    )
}
