// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import android.text.format.DateFormat
import android.util.Log
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Tune
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.rememberModalBottomSheetState
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.channels.CHANNEL_NEEDS_A_NAME
import me.lakko.app.channels.ChannelDiscovery
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelMetaPublisher
import me.lakko.app.channels.ClassifiedPost
import me.lakko.app.channels.classifyChannelPlaintext
import me.lakko.app.channels.sharedMeta
import me.lakko.app.channels.ChannelKeyStore
import me.lakko.app.channels.ChannelPublisherIdentity
import me.lakko.app.channels.ChannelPublisherKeyStore
import me.lakko.app.channels.ChannelPublisherKeys
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.ChannelsComposerException
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.CoPublishedJournals
import me.lakko.app.channels.CoPublisher
import me.lakko.app.channels.LegacyChannelException
import me.lakko.app.channels.LegacyChannelV1
import me.lakko.app.channels.PostView
import me.lakko.app.channels.RotationCron
import me.lakko.app.channels.SharedPreferencesChannelLogStore
import me.lakko.app.channels.StoredRotationState
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.ds.DsClient
import me.lakko.app.ui.components.LakkoAvatar
import me.lakko.app.ui.contacts.copyToClipboard
import me.lakko.app.ui.contacts.launchShareIntent
import uniffi.lakko_ffi_uniffi.ChannelInfoCapabilities
import uniffi.lakko_ffi_uniffi.ChannelRotationState
import uniffi.lakko_ffi_uniffi.InfoSection
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelInfoSections
import uniffi.lakko_ffi_uniffi.channelRotationPolicyDefault
import uniffi.lakko_ffi_uniffi.channelRotationStateAfterRotation
import java.time.ZoneId
import java.util.Locale

/**
 * The Channels screens (docs/ux/client-redesign.md section 5.4): the list,
 * the channel view, the subscribe route, the owner's Manage screen, New
 * channel and Join as co-publisher. Each is a destination of the app's
 * `NavHost` in `MainActivity` (Material back arrow, system and predictive
 * back); the shared [ChannelsUiState] and [ChannelsClient] are created once
 * by the session shell and threaded into every destination.
 *
 * Every repository call — publish, subscribe, envelope fan-out, rotation,
 * co-publisher approve with its reseal — is the audited path it was before
 * this restyle; only the chrome and the copy changed.
 */

private const val TAG = "ChannelsScreen"

/**
 * Build the process-scoped [ChannelsUiState], persisting the subscriber
 * keypairs AND the publisher signing seeds into the hardware-backed Keystore
 * (G-29) rather than plaintext `SharedPreferences`. The per-account channel
 * log (list, subscriber seeds, rotation state) rides the same encrypted
 * subscriber-key file, and the publisher MLS identity snapshots its provider
 * through [ChannelPublisherIdentity.onDevice]. The session shell binds the
 * state to the account once the identity secrets land.
 */
@Composable
public fun rememberChannelsUiState(): ChannelsUiState {
    val context = LocalContext.current
    return remember {
        val subscriberPrefs = ChannelKeyStore.encryptedPrefs(context)
        ChannelsUiState(
            prefs = subscriberPrefs,
            publisherPrefs = ChannelPublisherKeyStore.encryptedPrefs(context),
            logStore = SharedPreferencesChannelLogStore(subscriberPrefs),
            publisher = ChannelPublisherIdentity.onDevice(context),
        )
    }
}

// ============================================================================
// Channels list
// ============================================================================

// ============================================================================
// Screen chrome shared by the pushed destinations
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelTopBar(
    title: String,
    subtitle: String? = null,
    onBack: () -> Unit,
    /** docs/ux/conversation-info.md section 1 — open this channel's info.
     *  `null` on the bars that title a form rather than a conversation. */
    onShowInfo: (() -> Unit)? = null,
    actions: @Composable () -> Unit = {},
) {
    TopAppBar(
        title = {
            Column(
                modifier = if (onShowInfo != null) {
                    Modifier.clickable(onClick = onShowInfo)
                } else {
                    Modifier
                },
            ) {
                Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis)
                if (subtitle != null) {
                    Text(
                        subtitle,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        },
        navigationIcon = {
            IconButton(onClick = onBack) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
            }
        },
        actions = { actions() },
    )
}

/** A one-sentence error line under a form or a composer. */
@Composable
private fun ErrorLine(message: String, modifier: Modifier = Modifier) {
    Text(
        message,
        color = MaterialTheme.colorScheme.error,
        style = MaterialTheme.typography.bodySmall,
        modifier = modifier,
    )
}

// ============================================================================
// New channel
// ============================================================================

/**
 * Create a channel: a local name and description, an optional public handle.
 * The handle is claimed first so a taken handle aborts before anything is
 * created; the channel key seed comes from the account's publisher group
 * (G-27 / ADR-0017) on the shared [ChannelPublisherIdentity] the state owns.
 */
@Composable
public fun NewChannelScreen(
    state: ChannelsUiState,
    client: ChannelsClient,
    sessionToken: String,
    onBack: () -> Unit,
    onCreated: (channelId: String) -> Unit,
    /**
     * Join somebody else's channel as a co-publisher (ADR-0028 OQ3).
     *
     * It lives here rather than in the create menu because
     * `docs/ux/navigation.md` section 3 fixes that menu at exactly three items,
     * and because this is the same moment — setting a channel up — reached by
     * the same route. `null` hides it.
     */
    onJoinAsCoPublisher: (() -> Unit)? = null,
) {
    var name by remember { mutableStateOf("") }
    var description by remember { mutableStateOf("") }
    var handle by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    Scaffold(topBar = { ChannelTopBar(title = "New channel", onBack = onBack) }) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text("Name") },
                enabled = !busy,
                singleLine = true,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = description,
                onValueChange = { description = it },
                label = { Text("Description (optional)") },
                enabled = !busy,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = handle,
                onValueChange = { handle = it },
                label = { Text("Handle (optional)") },
                placeholder = { Text("@handle") },
                supportingText = { Text("A handle lets people find this channel by name.") },
                enabled = !busy,
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            error?.let { ErrorLine(it) }
            Button(
                onClick = {
                    if (busy || name.trim().isEmpty()) return@Button
                    busy = true
                    error = null
                    val id = ChannelsComposer.freshChannelIdHex()
                    val wantedHandle = handle.trim().removePrefix("@")
                    scope.launch {
                        try {
                            val publisher = state.publisher
                                ?: throw IllegalStateException("channel publisher identity not available")
                            // ADR-0057 finding 3 — the channel's PublisherGroup
                            // journal is claimed with the owner's root BEFORE
                            // anything announces the channel; a claim that does
                            // not land aborts the creation with nothing sent.
                            try {
                                publisher.withIdentity { identity ->
                                    DsClient().use { ds ->
                                        CoPublisher.foundAndClaimAtCreation(ds, identity, id)
                                    }
                                }
                            } catch (e: LegacyChannelException) {
                                throw e
                            } catch (e: Throwable) {
                                Log.w(TAG, "channel journal claim did not land; not created", e)
                                error = "Could not reach the server. Try again."
                                busy = false
                                return@launch
                            }
                            // Claim the optional handle FIRST so a first-come
                            // collision aborts cleanly without a half-created
                            // channel. An empty handle skips discovery.
                            if (wantedHandle.isNotEmpty()) {
                                try {
                                    ChannelDiscovery.registerChannelHandle(
                                        handle = wantedHandle,
                                        channelIdHex = id,
                                        sessionToken = sessionToken,
                                    )
                                } catch (e: DiscoveryHttpError) {
                                    Log.w(TAG, "handle registration failed: ${e.status}", e)
                                    error = if (e.status == 409) {
                                        "@$wantedHandle is taken. Pick another or leave it blank."
                                    } else {
                                        "Could not claim the handle. Try again."
                                    }
                                    busy = false
                                    return@launch
                                } catch (e: me.lakko.app.anon.AnonTokenException) {
                                    // ADR-0054 UX section 3 - the handle costs a
                                    // lookup and a register token.
                                    Log.w(TAG, "handle registration: no token", e)
                                    error = me.lakko.app.anon.anonTokenFailureCopy(
                                        e,
                                        me.lakko.app.anon.ACTION_ALLOWANCE_USED_COPY,
                                    )
                                    busy = false
                                    return@launch
                                }
                            }
                            client.createChannel(id)
                            // G-27 / ADR-0017: the channel content-key seed
                            // comes from a real MLS publisher group exporter,
                            // created in the shared publisher identity's
                            // provider and snapshotted with it so the seed
                            // survives a relaunch.
                            val keys = publisher.withIdentity { identity ->
                                ChannelsComposer.freshPublisherKeys(
                                    identity = identity,
                                    channelIdHex = id,
                                    existingSigningSeed = state.persistedPublisherSigningSeed(id),
                                )
                            }
                            state.addChannel(
                                ChannelHandle(
                                    id = id,
                                    localName = name.trim(),
                                    localDescription = description.trim(),
                                    addedAtMs = System.currentTimeMillis().toULong(),
                                    handle = wantedHandle,
                                ),
                            )
                            state.setKeys(id, keys)
                            // ADR-0051 §4 — the metadata record is the first
                            // post: subscribers title the channel from it. A
                            // failed post leaves the channel created; the next
                            // Save or rotation re-issues it.
                            try {
                                ChannelMetaPublisher.post(
                                    state = state,
                                    client = client,
                                    channelId = id,
                                    name = name.trim(),
                                    description = description.trim(),
                                    updatedAtMs = System.currentTimeMillis().toULong(),
                                )
                            } catch (e: Throwable) {
                                Log.w(TAG, "channel record not posted at creation", e)
                            }
                            onCreated(id)
                        } catch (e: LegacyChannelException) {
                            error = e.message
                        } catch (e: Throwable) {
                            Log.w(TAG, "create channel failed", e)
                            error = "Could not create the channel. Try again."
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy && name.trim().isNotEmpty(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (busy) "Creating..." else "Create")
            }
            if (onJoinAsCoPublisher != null) {
                TextButton(
                    onClick = onJoinAsCoPublisher,
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text("Join a channel as co-publisher")
                }
            }
        }
    }
}

// ============================================================================
// Channel view
// ============================================================================

/**
 * Per-post render state. `null` means "still decoding"; the two non-readable
 * cases render "Not available" with their own one-line reason and are never
 * force-decrypted (invariant #4).
 */
internal sealed interface PostRender {
    data class Plain(val value: String) : PostRender
    object Unreadable : PostRender
    object OlderKeyWindow : PostRender

    /** ADR-0051 §4 — a metadata record (adopted into the channel log) or a
     *  malformed one (dropped): never a card. */
    object Hidden : PostRender
}

/**
 * ADR-0051 §4 — the render for one decrypted plaintext: text renders, a
 * metadata record is filed as the channel's newest (when newer) and hidden, a
 * malformed record is hidden. Runs before the text path on every post.
 */
private fun renderDecrypted(state: ChannelsUiState, channelId: String, plaintext: ByteArray): PostRender =
    when (val c = classifyChannelPlaintext(plaintext)) {
        is ClassifiedPost.Text -> PostRender.Plain(c.text)
        is ClassifiedPost.Meta -> {
            state.adoptSharedMeta(channelId, c.meta)
            PostRender.Hidden
        }
        ClassifiedPost.Malformed -> PostRender.Hidden
    }

/**
 * The channel: posts as cards, newest at the bottom like a chat. An owner
 * gets the publish composer and a Manage action; a subscriber reads through
 * its own key; anyone else gets a Subscribe button.
 */
@Composable
public fun ChannelDetailScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onBack: () -> Unit,
    onSubscribe: () -> Unit,
    onManage: () -> Unit,
) {
    val channels by state.channels.collectAsState()
    val keysByChannel by state.keysByChannel.collectAsState()
    val subscriberKeysByChannel by state.subscriberKeysByChannel.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    val keys = keysByChannel[channelId]
    val isSubscriber = subscriberKeysByChannel.containsKey(channelId)
    val title = channel?.let { channelTitle(it) } ?: "Channel"
    // The header card below carries the name, the handle, the description and
    // the status; the top bar shows the title once and nothing else.
    val header = channel?.let { channelHeaderModel(it, isOwner = keys != null) }
    // docs/ux/conversation-info.md section 1 — the third destination. Tapping
    // the name opens the channel, the way a chat's name opens the person and a
    // group's opens the group.
    var infoOpen by remember { mutableStateOf(false) }
    // Only a publisher ever lists subscribers, so only a publisher has a count
    // (section 2.3: "where the client has it").
    val subscriberCount = SubscriberFanoutRegistry.subscriberCounts()[channelId]

    if (infoOpen && channel != null) {
        ChannelInfoSheet(
            channel = channel,
            isPublisher = keys != null,
            isSubscriber = isSubscriber,
            subscriberCount = subscriberCount,
            onManage = {
                infoOpen = false
                onManage()
            },
            onStopFollowing = {
                // Local by design AND by what exists: forget the reader
                // keypair and the seeds it recovered, then drop the channel
                // from this device's list. Nothing here tells the server —
                // no channels route retires a subscriber — which is why the
                // copy only ever speaks for this device.
                infoOpen = false
                state.clearSubscriberKeys(channelId)
                state.removeChannel(channelId)
                // The channel this screen is about is gone from the list, so
                // staying on it would show an empty reader for something no
                // longer followed.
                onBack()
            },
            onDismiss = { infoOpen = false },
        )
    }

    Scaffold(
        topBar = {
            ChannelTopBar(
                title = title,
                onBack = onBack,
                onShowInfo = channel?.let { { infoOpen = true } },
                actions = {
                    if (keys != null) {
                        IconButton(onClick = onManage) {
                            Icon(Icons.Filled.Tune, contentDescription = "Manage channel")
                        }
                    }
                },
            )
        },
    ) { inner ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner),
        ) {
            when {
                keys != null -> OwnerChannelView(
                    channelId = channelId,
                    header = header,
                    state = state,
                    client = client,
                    keys = keys,
                )
                isSubscriber -> SubscriberChannelView(
                    channelId = channelId,
                    header = header,
                    state = state,
                    client = client,
                )
                else -> Column(
                    modifier = Modifier
                        .fillMaxSize()
                        .padding(24.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center,
                ) {
                    channel?.let { channelDescription(it) }?.let {
                        Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
                        Spacer(Modifier.height(12.dp))
                    }
                    Text("Subscribe to read this channel.", style = MaterialTheme.typography.bodyMedium)
                    Spacer(Modifier.height(12.dp))
                    Button(onClick = onSubscribe) { Text("Subscribe") }
                }
            }
        }
    }
}

/**
 * The subscribe route for a channel found by handle or id: registers this
 * device's reader key, waits for the channel owner's key, then shows the
 * posts — the same view the channel screen shows a subscriber afterwards.
 */
@Composable
public fun ChannelSubscribeScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    /** The handle the channel was found by, if any: its list name until it has a better one. */
    handle: String?,
    onBack: () -> Unit,
) {
    val channels by state.channels.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    // A channel this device subscribes to belongs in its list, else there is
    // no way back to it after leaving this screen.
    LaunchedEffect(channelId) {
        if (state.channels.value.none { it.id == channelId }) {
            val bare = handle?.trim()?.removePrefix("@").orEmpty()
            state.addChannel(
                ChannelHandle(
                    id = channelId,
                    localName = bare,
                    localDescription = "",
                    addedAtMs = System.currentTimeMillis().toULong(),
                    handle = bare,
                ),
            )
        }
    }
    Scaffold(
        topBar = {
            ChannelTopBar(
                title = channel?.let { channelTitle(it) } ?: "Channel",
                onBack = onBack,
            )
        },
    ) { inner ->
        Box(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner),
        ) {
            SubscriberChannelView(
                channelId = channelId,
                header = channel?.let { channelHeaderModel(it, isOwner = false) },
                state = state,
                client = client,
            )
        }
    }
}

/** The owner's view: the post list, the publish composer, the headless key fan-out. */
@Composable
private fun OwnerChannelView(
    channelId: String,
    header: ChannelHeaderModel?,
    state: ChannelsUiState,
    client: ChannelsClient,
    keys: ChannelPublisherKeys,
) {
    val postsByChannel by state.postsByChannel.collectAsState()
    val epochByChannel by state.epochByChannel.collectAsState()
    val posts = postsByChannel[channelId] ?: emptyList()
    val epoch = epochByChannel[channelId] ?: 0u

    var composeText by remember { mutableStateOf("") }
    var publishBusy by remember { mutableStateOf(false) }
    var publishError by remember { mutableStateOf<String?>(null) }
    var loadError by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(true) }
    val decodedByCursor = remember { mutableStateMapOf<ULong, PostRender>() }
    val scope = rememberCoroutineScope()

    // New subscribers receive the channel key from this device while the
    // channel is open (or its Manage screen is): the same 5 s fan-out as before.
    SubscriberFanoutEffect(channelId = channelId, state = state, client = client)

    suspend fun refresh() {
        loading = true
        loadError = null
        try {
            val fetched = client.fetchPosts(channelId, 0u)
            val ep = client.fetchEpoch(channelId)
            state.setPosts(channelId, fetched.posts)
            state.setEpoch(channelId, ep)
            // The publisher holds the current content seed only; a post it
            // published under an earlier key shows as not available here
            // (the subscriber-side key map, ADR-0031, is what recovers older
            // windows; the publisher does not retain old seeds).
            val currentSeed = state.keysByChannel.value[channelId]?.seed ?: keys.seed
            decodedByCursor.clear()
            for (p in fetched.posts) {
                decodedByCursor[p.cursor] = try {
                    // ADR-0051 §4 — classify before the text path: the
                    // publisher's own metadata record is never a card.
                    renderDecrypted(
                        state,
                        channelId,
                        ChannelsComposer.verifyAndDecrypt(
                            seed = currentSeed,
                            channelIdHex = channelId,
                            postWireB64Url = p.payloadB64,
                        ),
                    )
                } catch (_: Throwable) {
                    PostRender.Unreadable
                }
            }
        } catch (e: Throwable) {
            Log.w(TAG, "fetch posts failed", e)
            loadError = "Could not load posts."
        } finally {
            loading = false
        }
    }

    LaunchedEffect(channelId) { refresh() }

    Column(modifier = Modifier.fillMaxSize()) {
        PostList(
            posts = posts,
            renderOf = { decodedByCursor[it.cursor] },
            loading = loading,
            header = header,
            emptyLine = "No posts yet. Write the first one below.",
            modifier = Modifier.weight(1f),
        )
        PublishComposer(
            text = composeText,
            onTextChange = { composeText = it },
            busy = publishBusy,
            error = publishError ?: loadError,
            onRetry = if (loadError != null) ({ scope.launch { refresh() } }) else null,
            onPublish = {
                val text = composeText.trim()
                if (publishBusy || text.isEmpty()) return@PublishComposer
                publishBusy = true
                publishError = null
                scope.launch {
                    try {
                        val current = state.keysByChannel.value[channelId] ?: keys
                        val counter = state.nextCounter(channelId)
                        val wire = ChannelsComposer.compose(
                            keys = current,
                            channelIdHex = channelId,
                            epochId = epoch,
                            messageCounter = counter,
                            plaintext = text.toByteArray(Charsets.UTF_8),
                        )
                        client.publishPost(channelId, wire)
                        composeText = ""
                        refresh()
                    } catch (e: Throwable) {
                        Log.w(TAG, "publish failed", e)
                        publishError = "Could not publish. Retry."
                    } finally {
                        publishBusy = false
                    }
                }
            },
        )
    }
}

/**
 * The publish composer, in the chat composer's shape: a hairline, a pill
 * field on the sunken surface and a filled send button.
 */
@Composable
private fun PublishComposer(
    text: String,
    onTextChange: (String) -> Unit,
    busy: Boolean,
    error: String?,
    onRetry: (() -> Unit)?,
    onPublish: () -> Unit,
) {
    Column(modifier = Modifier.imePadding()) {
        HorizontalDivider()
        if (error != null) {
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.padding(horizontal = 16.dp),
            ) {
                ErrorLine(error, modifier = Modifier.weight(1f))
                if (onRetry != null) {
                    TextButton(onClick = onRetry) { Text("Retry") }
                }
            }
        }
        Row(
            verticalAlignment = Alignment.Bottom,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 6.dp),
        ) {
            OutlinedTextField(
                value = text,
                onValueChange = onTextChange,
                placeholder = { Text("Write a post") },
                singleLine = false,
                maxLines = 5,
                enabled = !busy,
                shape = RoundedCornerShape(24.dp),
                colors = OutlinedTextFieldDefaults.colors(
                    focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                    unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                    disabledContainerColor = MaterialTheme.colorScheme.surfaceVariant,
                    focusedBorderColor = Color.Transparent,
                    unfocusedBorderColor = Color.Transparent,
                    disabledBorderColor = Color.Transparent,
                ),
                textStyle = MaterialTheme.typography.bodyLarge,
                keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(8.dp))
            FilledIconButton(
                onClick = onPublish,
                enabled = !busy && text.isNotBlank(),
                colors = IconButtonDefaults.filledIconButtonColors(
                    containerColor = MaterialTheme.colorScheme.primary,
                    contentColor = MaterialTheme.colorScheme.onPrimary,
                ),
                modifier = Modifier
                    .size(48.dp)
                    .semantics { contentDescription = "Publish" },
            ) {
                if (busy) {
                    CircularProgressIndicator(
                        modifier = Modifier.size(20.dp),
                        strokeWidth = 2.dp,
                        color = MaterialTheme.colorScheme.onPrimary,
                    )
                } else {
                    Icon(Icons.AutoMirrored.Filled.Send, contentDescription = null)
                }
            }
        }
    }
}

/**
 * The reader (docs/ux/group-and-channel-names.md section 3.2, redesign 5.4):
 * a header card — avatar, name, `@handle`, the description when present and
 * the standing status — then the posts as cards, newest at the bottom (a
 * reversed lazy list over the newest-first order starts scrolled to the bottom
 * like a chat). A post whose render is [PostRender.Hidden] (a metadata record)
 * is left out; with nothing visible the empty sentence sits under the header.
 */
@Composable
private fun PostList(
    posts: List<PostView>,
    renderOf: (PostView) -> PostRender?,
    loading: Boolean,
    header: ChannelHeaderModel?,
    emptyLine: String,
    modifier: Modifier = Modifier,
    statusLine: (@Composable () -> Unit)? = null,
) {
    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val ordered = remember(posts) { posts.sortedByDescending { it.serverTsMs } }
    val visible = ordered.filter { renderOf(it) != PostRender.Hidden }
    val now = System.currentTimeMillis()

    LazyColumn(
        modifier = modifier.fillMaxWidth(),
        reverseLayout = true,
        contentPadding = PaddingValues(horizontal = 16.dp, vertical = 12.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        items(items = visible, key = { it.cursor.toLong() }) { post ->
            PostCard(
                time = postRelativeTimeLabel(post.serverTsMs.toLong(), now, zone, locale),
                render = renderOf(post),
            )
        }
        if (visible.isEmpty()) {
            item("empty") {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(vertical = 24.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    when {
                        statusLine != null -> statusLine()
                        loading -> CircularProgressIndicator()
                        else -> Text(
                            emptyLine,
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        } else if (statusLine != null) {
            item("status") { statusLine() }
        }
        if (header != null) {
            item("header") { ChannelHeaderCard(header) }
        }
    }
}

/**
 * The channel a conversation's header opens — docs/ux/conversation-info.md
 * sections 2.3 and 3.
 *
 * A layer over the channel, which stays where it is behind it: the sheet's
 * handle and its scrim are the one obvious way out. The common shape the three
 * info surfaces share — the identity, then the actions, then the detail.
 *
 * Which blocks those are is asked, never decided here: the one copy of the
 * rule is `crates/lakko-ui-rules/src/info.rs`, and `Unsubscribe` arrives from
 * it for a subscriber and not for a publisher, because a publisher reads its
 * own channel from its own keys and has no subscription to end.
 *
 * This comment used to say the client had no unsubscribe at all. That was
 * wrong for as long as it stood: `ChannelsUiState.clearSubscriberKeys` and
 * `removeChannel` both existed and were simply never called from anywhere, so
 * a person who followed a channel could not stop.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelInfoSheet(
    channel: ChannelHandle,
    isPublisher: Boolean,
    isSubscriber: Boolean,
    subscriberCount: Int?,
    onManage: () -> Unit,
    /** Stop following on this device. See [STOP_FOLLOWING_SUPPORTING]. */
    onStopFollowing: () -> Unit,
    onDismiss: () -> Unit,
) {
    // Remembered because asking now crosses the FFI: the rule is a native
    // call, and its answer changes only when one of its inputs does.
    val countKnown = subscriberCount != null
    val sections = remember(isPublisher, isSubscriber, countKnown) {
        channelInfoSections(
            ChannelInfoCapabilities(
                isPublisher = isPublisher,
                isSubscriber = isSubscriber,
                subscriberCountKnown = countKnown,
            ),
        )
    }
    val header = channelHeaderModel(channel, isOwner = isPublisher)
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    var confirmingStopFollowing by remember { mutableStateOf(false) }
    if (confirmingStopFollowing) {
        AlertDialog(
            onDismissRequest = { confirmingStopFollowing = false },
            title = { Text(stopFollowingTitle(header.title)) },
            text = { Text(STOP_FOLLOWING_BODY) },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmingStopFollowing = false
                        onStopFollowing()
                    },
                    colors = ButtonDefaults.textButtonColors(
                        contentColor = MaterialTheme.colorScheme.error,
                    ),
                ) { Text(STOP_FOLLOWING_CONTROL) }
            },
            dismissButton = {
                TextButton(onClick = { confirmingStopFollowing = false }) { Text("Cancel") }
            },
        )
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            LakkoAvatar(name = header.title, seed = channel.id, size = 72.dp)
            Spacer(Modifier.height(12.dp))
            Text(
                header.title,
                style = MaterialTheme.typography.headlineSmall,
                textAlign = TextAlign.Center,
            )
            if (InfoSection.CHANNEL_HANDLE in sections) {
                header.handleLine?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Text(
                header.statusLine,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (InfoSection.SUBSCRIBER_COUNT in sections) {
                subscriberCountLine(subscriberCount)?.let {
                    Text(
                        it,
                        style = MaterialTheme.typography.labelMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            if (InfoSection.MANAGE_CHANNEL in sections) {
                Spacer(Modifier.height(16.dp))
                OutlinedButton(
                    onClick = onManage,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text("Manage channel") }
            }
            if (InfoSection.CHANNEL_DESCRIPTION in sections) {
                header.description?.let {
                    Spacer(Modifier.height(16.dp))
                    HorizontalDivider()
                    Spacer(Modifier.height(16.dp))
                    Text(it, style = MaterialTheme.typography.bodyMedium)
                }
            }
            if (InfoSection.UNSUBSCRIBE in sections) {
                Spacer(Modifier.height(16.dp))
                HorizontalDivider()
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clickable { confirmingStopFollowing = true }
                        .padding(vertical = 14.dp),
                ) {
                    Text(
                        STOP_FOLLOWING_CONTROL,
                        style = MaterialTheme.typography.bodyLarge,
                        color = MaterialTheme.colorScheme.error,
                    )
                    Spacer(Modifier.height(2.dp))
                    Text(
                        STOP_FOLLOWING_SUPPORTING,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}

// docs/ux/conversation-info.md section 2.3 — the copy is deliberately about
// THIS DEVICE. No channels route retires a subscriber, so the registration on
// the server outlives the act; wording that claimed the channel had been told
// would be false. The web client says the same words for the same reason.

/** The control, and the confirming button of its dialog. */
private const val STOP_FOLLOWING_CONTROL: String = "Stop following"

/** The line under the control. */
private const val STOP_FOLLOWING_SUPPORTING: String =
    "Takes the channel off your list and stops this device reading it. " +
        "You can subscribe again later."

/** The dialog body. */
private const val STOP_FOLLOWING_BODY: String =
    "It leaves your list and this device stops reading it. " +
        "You can subscribe again later."

/** The dialog title, which names the channel being left. */
private fun stopFollowingTitle(name: String): String = "Stop following $name?"

/** The card at the top of the reader: who this channel is. */
@Composable
private fun ChannelHeaderCard(header: ChannelHeaderModel) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(bottom = 6.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        shape = MaterialTheme.shapes.medium,
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .padding(20.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            LakkoAvatar(name = header.title, seed = header.title, size = 72.dp)
            Spacer(Modifier.height(12.dp))
            Text(
                header.title,
                style = MaterialTheme.typography.titleLarge,
                fontWeight = FontWeight.SemiBold,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
            header.handleLine?.let {
                Spacer(Modifier.height(2.dp))
                Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            header.description?.let {
                Spacer(Modifier.height(10.dp))
                Text(it, style = MaterialTheme.typography.bodyMedium)
            }
            Spacer(Modifier.height(10.dp))
            Text(
                header.statusLine,
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun PostCard(time: String, render: PostRender?) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        shape = MaterialTheme.shapes.medium,
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(horizontal = 16.dp, vertical = 14.dp)) {
            when (render) {
                null -> Text(
                    "Loading...",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                PostRender.Unreadable, PostRender.OlderKeyWindow -> {
                    Text("Not available", style = MaterialTheme.typography.bodyMedium, fontWeight = FontWeight.Medium)
                    Text(
                        unreadableReasonLine(olderThanKeys = render == PostRender.OlderKeyWindow),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                is PostRender.Plain -> Text(
                    render.value,
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurface,
                )
                PostRender.Hidden -> Unit
            }
            Spacer(Modifier.height(8.dp))
            Text(
                time,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.align(Alignment.End),
            )
        }
    }
}

// ============================================================================
// Subscriber view — register, wait for the channel key, read
// ============================================================================

private enum class SubscribeBootstrap { Registering, Waiting, Active, Failed }

/**
 * Generates (or restores) this device's reader keypair, registers it on the
 * channel, catches up on the retained key envelopes (ADR-0031), then renders
 * the posts decrypted with the key matching each post's window. Unchanged
 * behaviour; the states read in plain words.
 */
@Composable
private fun SubscriberChannelView(
    channelId: String,
    header: ChannelHeaderModel?,
    state: ChannelsUiState,
    client: ChannelsClient,
) {
    // Collected to recompose when a seed lands; the helpers read the latest
    // value off `state` directly.
    state.subscribedSeedsByChannel.collectAsState()
    val postsByChannel by state.postsByChannel.collectAsState()
    val posts = postsByChannel[channelId] ?: emptyList()

    var bootstrap by remember(channelId) { mutableStateOf(SubscribeBootstrap.Registering) }
    var attempt by remember(channelId) { mutableIntStateOf(0) }
    val decodedByCursor = remember(channelId) { mutableStateMapOf<ULong, PostRender>() }

    // ADR-0031 — open one epoch's seed envelope into the per-channel
    // epoch->seed map; a wrong epoch fails the open and is skipped by the
    // caller, never a wrong seed.
    suspend fun openEnvelopeIntoMap(epoch: UInt, wire: ByteArray) {
        val kp = state.subscriberKeysByChannel.value[channelId]
            ?: throw ChannelsComposerException.Ffi("missing subscriber keypair")
        val recovered = ChannelsComposer.openSubscriberSeedEnvelope(
            keys = kp,
            channelIdHex = channelId,
            epochId = epoch,
            wire = wire,
        )
        state.setSubscribedSeed(channelId, epoch, recovered)
    }

    // ADR-0031 catch-up + re-sync: the retained set, the singular latest as a
    // fallback, and an idempotent re-subscribe when the current key is still
    // missing. Best-effort per step. True once at least one key is available.
    suspend fun catchUp(pubHex: String): Boolean {
        val retained = try {
            client.fetchEnvelopes(channelId, pubHex)
        } catch (_: Throwable) {
            emptyList()
        }
        for ((epoch, wire) in retained) {
            try {
                openEnvelopeIntoMap(epoch, wire)
            } catch (_: Throwable) {
                // Skip a single bad / wrong-epoch envelope.
            }
        }
        val currentEpoch = try {
            client.fetchEpoch(channelId)
        } catch (_: Throwable) {
            null
        }
        if (currentEpoch != null) {
            state.setEpoch(channelId, currentEpoch)
            if (state.subscribedSeeds(channelId)[currentEpoch] == null) {
                val latest = try {
                    client.fetchEnvelope(channelId, pubHex)
                } catch (_: Throwable) {
                    null
                }
                val wire = latest?.let { ChannelsComposer.base64UrlDecodeOrNull(it) }
                if (wire != null) {
                    try {
                        openEnvelopeIntoMap(currentEpoch, wire)
                    } catch (_: Throwable) {
                        // Wrong-epoch guess — skip, never a wrong seed.
                    }
                }
            }
            if (state.subscribedSeeds(channelId)[currentEpoch] == null) {
                try {
                    client.subscribe(channelId, pubHex)
                } catch (_: Throwable) {
                    // Best-effort; the poll loop retries.
                }
            }
        }
        return state.subscribedSeeds(channelId).isNotEmpty()
    }

    suspend fun loadAndDecodePosts() {
        val seeds = state.subscribedSeeds(channelId)
        if (seeds.isEmpty()) return
        try {
            val page = client.fetchPosts(channelId, 0u)
            state.setPosts(channelId, page.posts)
            decodedByCursor.clear()
            for (p in page.posts) {
                decodedByCursor[p.cursor] = try {
                    when (
                        val r = ChannelsComposer.verifyAndDecryptForEpochs(
                            seedByEpoch = seeds,
                            channelIdHex = channelId,
                            postWireB64Url = p.payloadB64,
                        )
                    ) {
                        // ADR-0051 §4 — classify before the text path: a
                        // metadata record titles the channel, never a card.
                        is ChannelsComposer.SubscriberPostResult.Decoded ->
                            renderDecrypted(state, channelId, r.plaintext)
                        is ChannelsComposer.SubscriberPostResult.MissingEpochSeed ->
                            PostRender.OlderKeyWindow
                        ChannelsComposer.SubscriberPostResult.Unreadable ->
                            PostRender.Unreadable
                    }
                } catch (_: Throwable) {
                    PostRender.Unreadable
                }
            }
        } catch (_: Throwable) {
            // The status line already shows the bootstrap state.
        }
    }

    // Bootstrap on first composition; `attempt` re-runs it after a failure.
    // The 3 s catch-up loop lives inside this effect, so leaving the screen
    // cancels it.
    LaunchedEffect(channelId, attempt) {
        bootstrap = SubscribeBootstrap.Registering
        try {
            val kp = state.subscriberKeysByChannel.value[channelId]
                ?: ChannelsComposer.freshSubscriberKeys().also {
                    state.setSubscriberKeys(channelId, it)
                }
            val pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
            client.subscribe(channelId, pubHex)

            if (state.subscribedSeeds(channelId).isNotEmpty()) {
                bootstrap = SubscribeBootstrap.Active
                loadAndDecodePosts()
            } else {
                bootstrap = SubscribeBootstrap.Waiting
            }
            // Runs until the screen is left (the effect is cancelled). Before
            // the current epoch's seed lands: catch up every 3 s. After it:
            // reload the posts every 5 s so a post published while the reader
            // is open appears, and re-check the epoch about every 30 s so a
            // rotation is noticed without fetching the envelope set per tick.
            var haveCurrent = catchUp(pubHex)
            if (haveCurrent) {
                bootstrap = SubscribeBootstrap.Active
                loadAndDecodePosts()
            }
            var ticks = 0
            while (true) {
                delay(if (haveCurrent) READER_REFRESH_MS else SEED_WAIT_MS)
                ticks += 1
                try {
                    if (!haveCurrent || ticks % EPOCH_RECHECK_EVERY_TICKS == 0) {
                        haveCurrent = catchUp(pubHex)
                    }
                    if (state.subscribedSeeds(channelId).isNotEmpty()) {
                        bootstrap = SubscribeBootstrap.Active
                        loadAndDecodePosts()
                    }
                } catch (cause: kotlinx.coroutines.CancellationException) {
                    throw cause
                } catch (_: Throwable) {
                    // Transient — keep polling.
                }
            }
        } catch (e: Throwable) {
            Log.w(TAG, "subscribe failed", e)
            bootstrap = SubscribeBootstrap.Failed
        }
    }

    val status: (@Composable () -> Unit)? = when (bootstrap) {
        SubscribeBootstrap.Registering -> {
            { StatusLine("Subscribing...") }
        }
        SubscribeBootstrap.Waiting -> {
            { StatusLine("Subscribed. Waiting for the channel owner to share the posts.") }
        }
        SubscribeBootstrap.Failed -> {
            {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    ErrorLine("Could not subscribe.")
                    TextButton(onClick = { attempt += 1 }) { Text("Try again") }
                }
            }
        }
        SubscribeBootstrap.Active -> null
    }

    PostList(
        posts = if (bootstrap == SubscribeBootstrap.Active) posts else emptyList(),
        renderOf = { decodedByCursor[it.cursor] },
        loading = bootstrap != SubscribeBootstrap.Active,
        header = header,
        emptyLine = "No posts yet.",
        statusLine = status,
        modifier = Modifier.fillMaxSize(),
    )
}

@Composable
private fun StatusLine(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 24.dp),
    )
}

// ============================================================================
// Subscriber key fan-out (owner side) — the headless part of the old panel
// ============================================================================

/** Fan-out cadence: the owner re-lists subscribers and seals the key for each new one. */
private const val SUBSCRIBERS_POLL_INTERVAL_MS: Long = 5_000L
private const val SEED_WAIT_MS: Long = 3_000L
private const val READER_REFRESH_MS: Long = 5_000L
private const val EPOCH_RECHECK_EVERY_TICKS: Int = 6

internal enum class DeliveryStatus { Sent, Failed }

/** One channel's subscriber fan-out state, shared by the channel view and Manage. */
internal class SubscriberFanout {
    var subscribers by mutableStateOf<List<String>?>(null)
    val statusByPk = mutableStateMapOf<String, DeliveryStatus>()
}

/**
 * Process-wide registry so the channel view and the Manage screen (which
 * replace each other in the composition) share one fan-out record per channel,
 * and the list can show a subscriber count the owner already learned.
 */
internal object SubscriberFanoutRegistry {
    private val byChannel = mutableStateMapOf<String, SubscriberFanout>()

    fun forChannel(channelId: String): SubscriberFanout =
        byChannel.getOrPut(channelId) { SubscriberFanout() }

    /** Channel id -> known subscriber count (snapshot-read, so callers recompose). */
    @Composable
    fun subscriberCounts(): Map<String, Int> =
        byChannel.entries.mapNotNull { (id, f) -> f.subscribers?.let { id to it.size } }.toMap()
}

/**
 * Seal the current channel key for [pubkeyHex] and file it at the current
 * epoch (ADR-0031 — never a future one). Returns the resulting status.
 */
private suspend fun deliverKeyTo(
    channelId: String,
    pubkeyHex: String,
    state: ChannelsUiState,
    client: ChannelsClient,
): DeliveryStatus {
    val keys = state.keysByChannel.value[channelId] ?: return DeliveryStatus.Failed
    val parsed = hexToBytesOrNull(pubkeyHex) ?: return DeliveryStatus.Failed
    return try {
        // The current server epoch — fetched here when nothing on this screen
        // has learned it yet, so a delivery is never filed at a guessed 0.
        val epoch = state.epochByChannel.value[channelId]
            ?: client.fetchEpoch(channelId).also { state.setEpoch(channelId, it) }
        val wire = ChannelsComposer.sealSubscriberSeedEnvelope(
            subscriberPub = parsed,
            channelIdHex = channelId,
            epochId = epoch,
            seed = keys.seed,
        )
        client.deliverEnvelope(channelId, pubkeyHex, wire, epoch = epoch.toInt())
        DeliveryStatus.Sent
    } catch (e: Throwable) {
        Log.w(TAG, "key delivery failed", e)
        DeliveryStatus.Failed
    }
}

/**
 * The owner's fan-out loop: every 5 s re-list the subscribers and seal the
 * key for each one not yet delivered (or whose delivery failed). Sequential,
 * cancelled when the hosting screen leaves the composition.
 */
@Composable
private fun SubscriberFanoutEffect(channelId: String, state: ChannelsUiState, client: ChannelsClient) {
    val fanout = remember(channelId) { SubscriberFanoutRegistry.forChannel(channelId) }
    LaunchedEffect(channelId) {
        while (true) {
            try {
                fanout.subscribers = client.listSubscribers(channelId)
            } catch (e: Throwable) {
                Log.w(TAG, "list subscribers failed", e)
            }
            val list = fanout.subscribers
            if (list != null) {
                for (pk in list) {
                    if (fanout.statusByPk[pk] != DeliveryStatus.Sent) {
                        fanout.statusByPk[pk] = deliverKeyTo(channelId, pk, state, client)
                    }
                }
            }
            delay(SUBSCRIBERS_POLL_INTERVAL_MS)
        }
    }
}

private fun hexToBytesOrNull(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val byte = hex.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null
        out[i] = byte.toByte()
    }
    return out
}

// ============================================================================
// Manage (owners): subscribers, co-publishers, keys
// ============================================================================

/**
 * The owner's Manage screen. Opens the channel's publisher group once —
 * rotation and co-publisher management mutate the SAME MLS group, so one
 * handle serves both. Every call on the group goes through the state's
 * shared [ChannelPublisherIdentity]: serialised, off the main thread, and
 * followed by a provider snapshot after each epoch-advancing operation so a
 * relaunch reopens the group at the epoch the subscribers hold.
 */
@Composable
public fun ChannelManageScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onBack: () -> Unit,
) {
    val channels by state.channels.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    val context = LocalContext.current
    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val publisher = state.publisher

    var pubGroup by remember(channelId) { mutableStateOf<PublisherGroup?>(null) }
    var openError by remember(channelId) { mutableStateOf<String?>(null) }
    LaunchedEffect(channelId) {
        try {
            val pub = publisher ?: throw IllegalStateException("channel publisher identity not available")
            // Open-or-create: persisted so a group minted here is not lost.
            val opened = pub.withIdentity { id -> CoPublisher.openPublisherGroup(id, channelId) }
            pubGroup = opened
            // ADR-0057 — the channel OWNER claims the publisher journal (at
            // creation the first registration, afterwards an idempotent
            // re-send). A co-publisher device never claims: its root would
            // squat the journal against the owner. Best effort.
            val coPublished = CoPublishedJournals.onDevice(context)
            pub.withIdentity(persist = false) { id ->
                DsClient().use { ds -> CoPublisher.claimJournal(ds, id, opened, coPublished) }
            }
        } catch (e: Throwable) {
            Log.w(TAG, "open publisher group failed", e)
            openError = LegacyChannelV1.describe(e) { "Could not open this channel's keys on this device." }
        }
    }

    // The key fan-out keeps running while Manage is open, so a subscriber
    // who registers now is served here too.
    SubscriberFanoutEffect(channelId = channelId, state = state, client = client)

    /** After a rotation or an approve the group's current key changed: refresh
     *  the composer's copy so the next post seals under the key the
     *  subscribers were just given. */
    suspend fun refreshPublisherKey() {
        val pub = publisher ?: return
        val pg = pubGroup ?: return
        val current = state.keysByChannel.value[channelId] ?: return
        try {
            val seed = pub.withIdentity(persist = false) { id -> pg.channelSeed(id) }
            state.setKeys(channelId, current.copy(seed = seed))
            state.setEpoch(channelId, client.fetchEpoch(channelId))
        } catch (e: Throwable) {
            Log.w(TAG, "publisher key refresh failed", e)
        }
    }

    Scaffold(
        topBar = {
            ChannelTopBar(
                title = "Manage",
                subtitle = channel?.let { channelTitle(it) },
                onBack = onBack,
            )
        },
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            openError?.let { ErrorLine(it) }
            channel?.let { ChannelDetailsCard(channel = it, state = state, client = client) }
            SubscribersCard(channelId = channelId, state = state, client = client)
            CoPublishersCard(
                channelId = channelId,
                client = client,
                publisher = publisher,
                publisherGroup = pubGroup,
                onCopy = { copyToClipboard(context, it) },
                onShare = { launchShareIntent(context, it) },
                afterApprove = { refreshPublisherKey() },
            )
            KeysCard(
                channelId = channelId,
                state = state,
                client = client,
                publisher = publisher,
                publisherGroup = pubGroup,
                zone = zone,
                locale = locale,
                afterRotate = {
                    refreshPublisherKey()
                    // ADR-0051 §4 — re-issue the record under the new key so a
                    // late subscriber, who holds only the newest epochs, still
                    // finds one it can read. The stored stamp, verbatim.
                    ChannelMetaPublisher.reissue(state, client, channelId)
                },
            )
            Spacer(Modifier.height(8.dp))
        }
    }
}

/**
 * ADR-0051 §4 / UX 3.2 — the channel's name and description, editable by the
 * publisher. Save posts a new metadata record (a fresh stamp) and updates the
 * local copy; an empty name is refused with "A channel needs a name".
 */
@Composable
private fun ChannelDetailsCard(channel: ChannelHandle, state: ChannelsUiState, client: ChannelsClient) {
    val current = remember(channel.id) { channel.sharedMeta() }
    var name by remember(channel.id) { mutableStateOf(current?.name ?: channel.localName) }
    var description by remember(channel.id) { mutableStateOf(current?.description ?: channel.localDescription) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val dirty = name.trim() != channelTitle(channel) || description.trim() != channelDescription(channel).orEmpty()

    SectionCard(
        title = "Channel",
        description = "Every subscriber sees the name and the description.",
    ) {
        OutlinedTextField(
            value = name,
            onValueChange = {
                name = it
                error = null
            },
            label = { Text("Name") },
            singleLine = true,
            enabled = !busy,
            isError = error != null && name.trim().isEmpty(),
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = description,
            onValueChange = {
                description = it
                error = null
            },
            label = { Text("Description (optional)") },
            enabled = !busy,
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences),
            modifier = Modifier.fillMaxWidth(),
        )
        error?.let { ErrorLine(it) }
        Button(
            onClick = {
                if (busy) return@Button
                val trimmedName = name.trim()
                if (trimmedName.isEmpty()) {
                    error = CHANNEL_NEEDS_A_NAME
                    return@Button
                }
                busy = true
                error = null
                scope.launch {
                    try {
                        ChannelMetaPublisher.post(
                            state = state,
                            client = client,
                            channelId = channel.id,
                            name = trimmedName,
                            description = description.trim(),
                            updatedAtMs = System.currentTimeMillis().toULong(),
                        )
                        state.updateLocalMeta(channel.id, localName = trimmedName, localDescription = description.trim())
                    } catch (e: uniffi.lakko_ffi_uniffi.FfiException.ChannelMeta) {
                        // The message opens with the frozen reason code; the
                        // human-readable half follows the ": ".
                        error = e.message?.substringAfter(": ", missingDelimiterValue = "")
                            ?.takeIf { it.isNotEmpty() } ?: "The name or description is not allowed."
                    } catch (e: Throwable) {
                        Log.w(TAG, "channel record not posted", e)
                        error = "Could not save. Retry."
                    } finally {
                        busy = false
                    }
                }
            },
            enabled = !busy && dirty,
        ) {
            Text(if (busy) "Saving..." else "Save")
        }
    }
}

@Composable
private fun SectionCard(title: String, description: String? = null, content: @Composable () -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        shape = MaterialTheme.shapes.medium,
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            if (description != null) {
                Text(description, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            content()
        }
    }
}

/** "Subscriber N" rows with their key-delivery state; never a key. */
@Composable
private fun SubscribersCard(channelId: String, state: ChannelsUiState, client: ChannelsClient) {
    val fanout = remember(channelId) { SubscriberFanoutRegistry.forChannel(channelId) }
    val subs = fanout.subscribers
    val scope = rememberCoroutineScope()
    SectionCard(
        title = subs?.let { "Subscribers (${it.size})" } ?: "Subscribers",
        description = "Everyone who subscribed to this channel.",
    ) {
        when {
            subs == null -> Text("Loading...", style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            subs.isEmpty() -> Text(
                "No subscribers yet. Share the channel handle so people can find it.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            else -> subs.forEachIndexed { index, pk ->
                val status = fanout.statusByPk[pk]
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    LakkoAvatar(name = "S ${index + 1}", seed = pk, size = 32.dp)
                    Spacer(Modifier.width(12.dp))
                    Text("Subscriber ${index + 1}", style = MaterialTheme.typography.bodyLarge, modifier = Modifier.weight(1f))
                    when (status) {
                        DeliveryStatus.Sent -> Text(
                            "Receiving posts",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                        DeliveryStatus.Failed -> TextButton(
                            onClick = {
                                scope.launch {
                                    fanout.statusByPk[pk] = deliverKeyTo(channelId, pk, state, client)
                                }
                            },
                        ) { Text("Retry") }
                        null -> Text(
                            "Setting up...",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

/**
 * Co-publishers (ADR-0028 OQ3): the invite link with Copy / Share, and the
 * pending requests with Approve / Reject. Approve is the audited path that
 * adds the joiner AND reseals the new key to every subscriber.
 */
@Composable
private fun CoPublishersCard(
    channelId: String,
    client: ChannelsClient,
    publisher: ChannelPublisherIdentity?,
    publisherGroup: PublisherGroup?,
    onCopy: (String) -> Unit,
    onShare: (String) -> Unit,
    afterApprove: suspend () -> Unit,
) {
    var link by remember(channelId) { mutableStateOf<String?>(null) }
    var pending by remember(channelId) { mutableStateOf<List<ByteArray>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val ds = remember { DsClient() }
    val scope = rememberCoroutineScope()

    // Poll the publisher group's journal for join requests with the SHARED
    // handle the screen owns; re-armed if the handle re-opens. A poll that
    // applied a peer's commit advanced the epoch, so the provider is
    // snapshotted then (and only then).
    LaunchedEffect(publisherGroup) {
        val pub = publisher ?: return@LaunchedEffect
        val pg = publisherGroup ?: return@LaunchedEffect
        while (true) {
            try {
                val poll = pub.withIdentity(persist = false) { id -> CoPublisher.pollJournal(ds, id, pg) }
                if (poll.applied > 0) pub.persist()
                pending = pub.withIdentity(persist = false) { CoPublisher.pending(pg) }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (_: Throwable) {
                // Best-effort — keep the last good pending list.
            }
            delay(5_000L)
        }
    }

    SectionCard(
        title = "Co-publishers",
        description = "People with the invite link can ask to publish to this channel.",
    ) {
        val current = link
        if (current == null) {
            OutlinedButton(
                onClick = {
                    val pub = publisher
                    val pg = publisherGroup
                    if (busy || pub == null || pg == null) return@OutlinedButton
                    busy = true
                    error = null
                    scope.launch {
                        try {
                            link = pub.withIdentity(persist = false) { id ->
                                CoPublisher.createCoPublisherLink(id, pg)
                            }
                        } catch (e: Throwable) {
                            Log.w(TAG, "create co-publisher link failed", e)
                            error = "Could not create the link. Try again."
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy && publisherGroup != null,
            ) { Text("Create invite link") }
        } else {
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                Column(modifier = Modifier.weight(1f)) {
                    Text("Invite link", style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
                    Text(
                        "Anyone with it can request to publish.",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                IconButton(onClick = { onCopy(current) }) {
                    Icon(Icons.Filled.ContentCopy, contentDescription = "Copy link")
                }
                IconButton(onClick = { onShare(current) }) {
                    Icon(Icons.Filled.Share, contentDescription = "Share link")
                }
            }
        }

        if (pending.isNotEmpty()) {
            HorizontalDivider()
            Text("Requests (${pending.size})", style = MaterialTheme.typography.bodyLarge, fontWeight = FontWeight.Medium)
            pending.forEachIndexed { index, ref ->
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    Text("Request ${index + 1}", style = MaterialTheme.typography.bodyMedium, modifier = Modifier.weight(1f))
                    TextButton(
                        onClick = {
                            val pub = publisher
                            val pg = publisherGroup
                            if (busy || pub == null || pg == null) return@TextButton
                            error = null
                            scope.launch {
                                try {
                                    // A drop of the retained request: no commit,
                                    // no epoch advance. The retained set is part
                                    // of the provider snapshot, so this persists
                                    // (the default) or the request reappears on
                                    // the next open.
                                    pending = pub.withIdentity { id ->
                                        CoPublisher.reject(id, pg, ref)
                                        CoPublisher.pending(pg)
                                    }
                                } catch (e: Throwable) {
                                    Log.w(TAG, "reject failed", e)
                                    error = "Could not reject the request. Try again."
                                }
                            }
                        },
                        enabled = !busy,
                    ) { Text("Reject", color = MaterialTheme.colorScheme.error) }
                    TextButton(
                        onClick = {
                            val pub = publisher
                            val pg = publisherGroup
                            if (busy || pub == null || pg == null) return@TextButton
                            busy = true
                            error = null
                            scope.launch {
                                try {
                                    // The audited approve: adds the joiner and
                                    // reseals the new key to every subscriber.
                                    // Persisted after success — the Add advanced
                                    // the epoch the subscribers were resealed to.
                                    pending = pub.withIdentity { id ->
                                        CoPublisher.approve(ds, client, id, pg, ref, channelId)
                                        CoPublisher.pending(pg)
                                    }
                                    afterApprove()
                                } catch (e: Throwable) {
                                    Log.w(TAG, "approve failed", e)
                                    error = "Could not approve the request. Try again."
                                } finally {
                                    busy = false
                                }
                            }
                        },
                        enabled = !busy,
                    ) { Text("Approve") }
                }
            }
        }
        error?.let { ErrorLine(it) }
    }
}

/**
 * Key rotation (ADR-0010 / C-54). The Rust core owns the *when*
 * (`channelShouldRotate`), [RotationCron] the *how* (rotate the group, reseal
 * every subscriber, one atomic fan-out). The heartbeat state is persisted per
 * channel in the state's channel log (like the web and iOS stores), so the
 * schedule survives a relaunch: a channel opened past its rotation deadline
 * rotates on the first tick. A channel with no persisted state is seeded as
 * "rotated just now" on first open, so the policy does not fire against a
 * genesis state. "Rotate now" is the immediate control. Every group call
 * runs on the shared [ChannelPublisherIdentity]; the provider is snapshotted
 * after a rotation that landed.
 */
@Composable
private fun KeysCard(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    publisher: ChannelPublisherIdentity?,
    publisherGroup: PublisherGroup?,
    zone: ZoneId,
    locale: Locale,
    afterRotate: suspend () -> Unit,
) {
    val policy = remember { channelRotationPolicyDefault() }
    var cronState by remember(channelId) {
        mutableStateOf(
            state.rotationState(channelId)?.toFfi()
                ?: channelRotationStateAfterRotation(System.currentTimeMillis().toULong())
                    .also { seeded -> state.saveRotationState(channelId, seeded.toStored()) },
        )
    }
    var lastRotatedAtMs by remember(channelId) {
        mutableStateOf(cronState.lastRotationMs.takeIf { it > 0uL }?.toLong())
    }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    /** Persist + adopt a new heartbeat state. */
    fun adoptState(next: ChannelRotationState) {
        cronState = next
        state.saveRotationState(channelId, next.toStored())
    }

    // The rotation heartbeat, armed once the publisher handle is open,
    // cancelled when this card leaves the composition. One pass at once, then
    // one per interval, the [RotationCron.start] cadence — driven here rather
    // than by `start` so each pass runs on the shared identity and a landed
    // rotation is snapshotted before the next tick.
    LaunchedEffect(publisher, publisherGroup) {
        val pub = publisher ?: return@LaunchedEffect
        val pg = publisherGroup ?: return@LaunchedEffect
        while (true) {
            try {
                val result = pub.withIdentity(persist = false) { id ->
                    RotationCron(
                        publisherGroup = pg,
                        identity = id,
                        channelIdHex = channelId,
                        channelsClient = client,
                        saveState = { next -> adoptState(next) },
                    ).runOnce(
                        policy = policy,
                        state = cronState,
                        nowMs = System.currentTimeMillis().toULong(),
                    )
                }
                if (result.rotated) {
                    pub.persist()
                    lastRotatedAtMs = System.currentTimeMillis()
                    afterRotate()
                }
            } catch (cause: kotlinx.coroutines.CancellationException) {
                throw cause
            } catch (e: Throwable) {
                Log.w(TAG, "scheduled rotation failed", e)
                error = "Could not rotate the keys. It will be retried."
            }
            delay(ROTATION_HEARTBEAT_MS)
        }
    }

    SectionCard(
        title = "Encryption keys",
        description = "Subscribers receive a new key automatically when it rotates.",
    ) {
        Text(
            rotationStatusLine(lastRotatedAtMs, System.currentTimeMillis(), zone, locale),
            style = MaterialTheme.typography.bodyMedium,
        )
        OutlinedButton(
            onClick = {
                val pub = publisher
                val pg = publisherGroup
                if (busy || pub == null || pg == null) return@OutlinedButton
                busy = true
                error = null
                scope.launch {
                    try {
                        // A genesis {0,0} state makes the policy fire against
                        // any real wall-clock now; runOnce rotates + reseals
                        // every subscriber atomically and returns the fresh
                        // after-rotation state adopted as the new baseline.
                        val result = pub.withIdentity(persist = false) { id ->
                            RotationCron(
                                publisherGroup = pg,
                                identity = id,
                                channelIdHex = channelId,
                                channelsClient = client,
                                saveState = { next -> adoptState(next) },
                            ).runOnce(
                                policy = policy,
                                state = ChannelRotationState(lastRotationMs = 0uL, postsSinceRotation = 0u),
                                nowMs = System.currentTimeMillis().toULong(),
                            )
                        }
                        adoptState(result.newState)
                        if (result.rotated) {
                            pub.persist()
                            lastRotatedAtMs = System.currentTimeMillis()
                            afterRotate()
                        }
                    } catch (e: Throwable) {
                        Log.w(TAG, "rotate now failed", e)
                        error = "Could not rotate the keys. Try again."
                    } finally {
                        busy = false
                    }
                }
            },
            enabled = !busy && publisherGroup != null,
        ) {
            Text(if (busy) "Rotating..." else "Rotate now")
        }
        error?.let { ErrorLine(it) }
    }
}

/** The scheduled rotation check cadence — the [RotationCron.start] default. */
private const val ROTATION_HEARTBEAT_MS: Long = 5L * 60L * 1_000L

private fun StoredRotationState.toFfi(): ChannelRotationState =
    ChannelRotationState(lastRotationMs = lastRotationMs, postsSinceRotation = postsSinceRotation)

private fun ChannelRotationState.toStored(): StoredRotationState =
    StoredRotationState(lastRotationMs = lastRotationMs, postsSinceRotation = postsSinceRotation)

// ============================================================================
// Join as co-publisher (ADR-0028 OQ3, joiner side)
// ============================================================================

/**
 * Paste a co-publisher link, ask to join, wait for the owner's approval, then
 * mint local publisher keys and open the channel.
 */
@Composable
public fun JoinCoPublisherScreen(
    state: ChannelsUiState,
    onBack: () -> Unit,
    onJoined: (channelId: String) -> Unit,
) {
    var link by remember { mutableStateOf("") }
    var localName by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    // ADR-0053 — "Try again" is offered only with the general failure.
    var offerRetry by remember { mutableStateOf(false) }
    val ds = remember { DsClient() }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    // ADR-0057 — marks the channel's journal as co-published, so this device
    // never claims it.
    val coPublished = remember(context) { CoPublishedJournals.onDevice(context) }

    Scaffold(topBar = { ChannelTopBar(title = "Join as co-publisher", onBack = onBack) }) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                "The channel owner shares an invite link. Once they approve your request, this device can publish to the channel.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            OutlinedTextField(
                value = link,
                onValueChange = { link = it },
                label = { Text("Invite link") },
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = localName,
                onValueChange = { localName = it },
                label = { Text("Name (optional)") },
                enabled = !busy,
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            // Sends the request; reached from `Request to join` and from
            // `Try again` after a general failure.
            fun submit() {
                val trimmed = link.trim()
                if (busy || trimmed.isEmpty()) return
                busy = true
                error = null
                offerRetry = false
                status = "Sending your request..."
                scope.launch {
                    try {
                        val publisher = state.publisher
                            ?: throw IllegalStateException("channel publisher identity not available")
                        // The request mints a KeyPackage whose private keys
                        // live in the shared identity's provider; the join
                        // below must run on the SAME identity, and the
                        // joined group is persisted with it.
                        val request = publisher.withIdentity { identity ->
                            CoPublisher.requestToCoPublish(ds, identity, trimmed, coPublished)
                        }
                        status = "Waiting for the channel owner to approve..."
                        var keys: ChannelPublisherKeys? = null
                        var attempts = 0
                        while (keys == null && attempts < 60) {
                            keys = publisher.withIdentity(persist = false) { identity ->
                                CoPublisher.pollForCoPublishWelcome(
                                    ds,
                                    identity,
                                    request.groupId,
                                    request.joinerKey,
                                    request.channelIdHex,
                                    coPublished,
                                    // G-29 — reuse the persisted signing seed on a re-poll.
                                    state.persistedPublisherSigningSeed(request.channelIdHex),
                                )
                            }
                            if (keys == null) {
                                delay(3_000L)
                                attempts += 1
                            } else {
                                // Joined: the PublisherGroup now lives in the
                                // provider at the post-approve epoch.
                                publisher.persist()
                            }
                        }
                        val resolved = keys
                        if (resolved == null) {
                            error = "No approval yet. Try again once the owner has approved."
                            return@launch
                        }
                        val name = localName.trim()
                        state.addChannel(
                            ChannelHandle(
                                id = request.channelIdHex,
                                localName = name.ifEmpty { "Shared channel" },
                                localDescription = "",
                                addedAtMs = System.currentTimeMillis().toULong(),
                            ),
                        )
                        state.setKeys(request.channelIdHex, resolved)
                        onJoined(request.channelIdHex)
                    } catch (e: kotlinx.coroutines.CancellationException) {
                        throw e
                    } catch (e: LegacyChannelException) {
                        error = e.message
                    } catch (e: Throwable) {
                        // The raw cause goes to the log; the person reads
                        // one of the join screen's fixed sentences (the
                        // "isn't taking requests yet" one when the owner
                        // has not claimed the journal; nothing was sent).
                        Log.w(TAG, "co-publish request failed", e)
                        val copy = me.lakko.app.chat.joinFailureCopy(e)
                        error = copy
                        offerRetry = me.lakko.app.chat.joinFailureOffersRetry(copy)
                    } finally {
                        busy = false
                        status = null
                    }
                }
            }
            Button(
                onClick = { submit() },
                enabled = !busy && link.trim().isNotEmpty(),
                modifier = Modifier.fillMaxWidth(),
            ) { Text(if (busy) "Requesting..." else "Request to join") }

            status?.let { Text(it, style = MaterialTheme.typography.bodyMedium, color = MaterialTheme.colorScheme.onSurfaceVariant) }
            error?.let { ErrorLine(it) }
            if (error != null && offerRetry) {
                Button(
                    onClick = { submit() },
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(me.lakko.app.chat.JOIN_TRY_AGAIN) }
            }
        }
    }
}
