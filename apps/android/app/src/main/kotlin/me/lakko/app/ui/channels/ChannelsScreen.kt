// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.platform.LocalContext
import me.lakko.app.channels.ChannelKeyStore
import me.lakko.app.channels.ChannelPublisherKeyStore
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import androidx.compose.foundation.text.selection.SelectionContainer
import me.lakko.app.channels.ChannelDiscovery
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelPublisherKeys
import me.lakko.app.channels.ChannelsApiException
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.ChannelsComposerException
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.CoPublisher
import me.lakko.app.channels.PostView
import me.lakko.app.channels.RotationCron
import me.lakko.app.discovery.DiscoveryHttpError
import me.lakko.app.ds.DsClient
import uniffi.lakko_ffi_uniffi.ChannelRotationState
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup
import uniffi.lakko_ffi_uniffi.channelRotationPolicyDefault
import uniffi.lakko_ffi_uniffi.channelRotationStateAfterRotation
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone

/**
 * Channels Compose UI — Phase 4 G4-2 Android port of
 * `apps/web/src/routes/Channels.tsx` and
 * `apps/ios/Lakko/UI/Channels/ChannelsScreen.swift`.
 *
 * Three nested screens — top-level entry takes a `route`
 * sealed class so a containing NavHost (or the simple
 * mutable-state version below) can render the right one.
 * Same palette + flow as the iOS / web sides.
 */

private object Theme {
    val background = Color(0xFF0F0F0F)
    val surface = Color(0xFF1A1A1A)
    val border = Color(0xFF2A2A2A)
    val textPrimary = Color(0xFFFAFAFA)
    val textSecondary = Color(0xFFA1A1AA)
    val accent = Color(0xFF3B82F6)
    val destructive = Color(0xFFDC2626)
}

private sealed class Route {
    object List : Route()
    object New : Route()
    object JoinCoPublisher : Route()
    data class Detail(val channelId: String) : Route()
    data class Subscribe(val channelId: String) : Route()
}

/**
 * Build the process-scoped [ChannelsUiState], persisting the subscriber
 * keypairs AND the publisher signing seeds into the hardware-backed Keystore
 * (G-29) rather than plaintext `SharedPreferences`. Reads `LocalContext`
 * (composable scope) before the non-composable `remember` block builds the
 * encrypted prefs.
 */
@Composable
private fun rememberChannelsUiState(): ChannelsUiState {
    val context = LocalContext.current
    return remember {
        ChannelsUiState(
            prefs = ChannelKeyStore.encryptedPrefs(context),
            publisherPrefs = ChannelPublisherKeyStore.encryptedPrefs(context),
        )
    }
}

@Composable
public fun ChannelsRootScreen(
    /**
     * 64-byte AccountIdentity (BIP39 phrase) seed for the active
     * session (ADR-0017). [NewChannelScreen] rebuilds a transient
     * `Identity` from it (`Identity.fromSeed(accountIdentitySeed)`)
     * to drive the MLS PublisherGroup that produces the channel
     * content-key seed (G-27). Per ADR-0017 the canonical
     * AccountIdentity is seeded from the stable phrase seed, NOT the
     * password-derived OPAQUE export key, so it survives a recover /
     * password change.
     *
     * The host (`MainActivity` / a future Channels tab) supplies this
     * from `AuthRepository`'s in-memory `IdentitySecrets` — same shape
     * `ChatRepository` already uses.
     */
    accountIdentitySeed: ByteArray,
    /** Phase 1.5b bearer token for the active session
     *  (`IdentitySecrets.sessionToken`). Threaded into
     *  [NewChannelScreen] so the optional `@handle` registration
     *  (G4-5, ADR-0010) can drive the session-token-gated
     *  `POST /discovery/channel/register`. */
    sessionToken: String,
    /** Back to the host (the app Home surface). Wired into the list
     *  screen's top-bar back arrow, matching the Contacts / Devices
     *  navigation idiom so the Channels entry is not a dead-end. */
    onBack: () -> Unit,
    state: ChannelsUiState = rememberChannelsUiState(),
    client: ChannelsClient = remember { ChannelsClient() },
) {
    var route by remember { mutableStateOf<Route>(Route.List) }
    when (val r = route) {
        Route.List -> ChannelsListScreen(
            state = state,
            onCreate = { route = Route.New },
            onOpen = { route = Route.Detail(it) },
            onJoin = { route = Route.JoinCoPublisher },
            // G4-5: a resolved `@handle` OR pasted hex id lands on the
            // subscriber route for that channel id (mirrors the web
            // `ChannelsList` find-and-navigate).
            onFindSubscribe = { hex -> route = Route.Subscribe(hex) },
            onBack = onBack,
        )
        Route.New -> NewChannelScreen(
            state = state,
            client = client,
            accountIdentitySeed = accountIdentitySeed,
            sessionToken = sessionToken,
            onClose = { route = Route.List },
            onCreated = { id -> route = Route.Detail(id) },
        )
        Route.JoinCoPublisher -> JoinCoPublisherScreen(
            state = state,
            accountIdentitySeed = accountIdentitySeed,
            onClose = { route = Route.List },
            onJoined = { id -> route = Route.Detail(id) },
        )
        is Route.Detail -> ChannelDetailScreen(
            channelId = r.channelId,
            state = state,
            client = client,
            accountIdentitySeed = accountIdentitySeed,
            onBack = { route = Route.List },
            onSubscribe = { route = Route.Subscribe(r.channelId) },
        )
        is Route.Subscribe -> ChannelSubscribeScreen(
            channelId = r.channelId,
            state = state,
            client = client,
            onBack = { route = Route.List },
        )
    }
}

// ============================================================================
// ChannelsListScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelsListScreen(
    state: ChannelsUiState,
    onCreate: () -> Unit,
    onOpen: (String) -> Unit,
    onJoin: () -> Unit,
    /** G4-5: navigate to the subscriber route for a resolved 32-char
     *  hex channel id (from a typed `@handle` or a pasted raw id). */
    onFindSubscribe: (String) -> Unit,
    onBack: () -> Unit,
) {
    val channels by state.channels.collectAsState()
    // G4-5: subscribe entry point. Accepts EITHER a typed `@handle`
    // (OPRF-resolved to a channel id) OR a pasted 32-char hex channel
    // id (used verbatim). Both land on the subscriber route.
    var findInput by remember { mutableStateOf("") }
    var findBusy by remember { mutableStateOf(false) }
    var findError by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    fun runFind() {
        if (findBusy) return
        val raw = findInput.trim()
        if (raw.isEmpty()) return
        findBusy = true
        findError = null
        scope.launch {
            try {
                val idHex = ChannelDiscovery.resolveChannelIdOrHandle(raw)
                if (idHex == null) {
                    val shown = if (raw.startsWith("@")) raw else "@$raw"
                    findError = "No channel found for $shown."
                    findBusy = false
                    return@launch
                }
                findBusy = false
                onFindSubscribe(idHex)
            } catch (e: Throwable) {
                findError = "Lookup failed: ${e.message}"
                findBusy = false
            }
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Channels", color = Theme.textPrimary) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, "Back", tint = Theme.textPrimary)
                    }
                },
                actions = {
                    TextButton(onClick = onJoin) {
                        Text("Join", color = Theme.accent)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Theme.background,
                ),
            )
        },
        floatingActionButton = {
            FloatingActionButton(
                onClick = onCreate,
                containerColor = Theme.accent,
            ) {
                Icon(Icons.Default.Add, "New channel", tint = Color.White)
            }
        },
        containerColor = Theme.background,
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner),
        ) {
            // G4-5 subscribe entry: @handle OR pasted hex channel id.
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 12.dp)
                    .padding(top = 8.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedTextField(
                    value = findInput,
                    onValueChange = {
                        findInput = it
                        findError = null
                    },
                    label = { Text("Find a channel — @handle or id") },
                    enabled = !findBusy,
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                findError?.let { msg ->
                    Text(msg, color = Theme.destructive, fontSize = 13.sp)
                }
                Button(
                    onClick = { runFind() },
                    enabled = !findBusy && findInput.trim().isNotEmpty(),
                    colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    Text(if (findBusy) "Finding…" else "Find & subscribe")
                }
            }
            Spacer(Modifier.height(8.dp))
            if (channels.isEmpty()) {
                Column(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth()
                        .padding(24.dp),
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center,
                ) {
                    Text(
                        "No channels yet.",
                        color = Theme.textSecondary,
                        fontSize = 16.sp,
                    )
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "Tap + to create one, or find one above.",
                        color = Theme.textSecondary,
                        fontSize = 14.sp,
                    )
                }
            } else {
                LazyColumn(
                    modifier = Modifier
                        .weight(1f)
                        .fillMaxWidth()
                        .padding(horizontal = 12.dp),
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    items(channels, key = { it.id }) { channel ->
                        ChannelRow(channel, onClick = { onOpen(channel.id) })
                    }
                }
            }
        }
    }
}

@Composable
private fun ChannelRow(channel: ChannelHandle, onClick: () -> Unit) {
    Card(
        modifier = Modifier
            .fillMaxWidth()
            .padding(vertical = 4.dp),
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        onClick = onClick,
    ) {
        Column(Modifier.padding(14.dp)) {
            Text(
                channel.localName.ifEmpty { "(unnamed)" },
                color = Theme.textPrimary,
                fontWeight = FontWeight.SemiBold,
            )
            if (channel.localDescription.isNotEmpty()) {
                Spacer(Modifier.height(4.dp))
                Text(
                    channel.localDescription,
                    color = Theme.textSecondary,
                    fontSize = 13.sp,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            Spacer(Modifier.height(6.dp))
            Text(
                "${channel.id.take(8)}…${channel.id.takeLast(8)}",
                color = Theme.textSecondary,
                fontSize = 12.sp,
                fontFamily = FontFamily.Monospace,
            )
        }
    }
}

// ============================================================================
// NewChannelScreen
// ============================================================================

/** G-27 / ADR-0017: needs the AccountIdentity (phrase) seed so
 *  [NewChannelScreen.create] can rebuild the PublisherGroup
 *  founding-member identity. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NewChannelScreen(
    state: ChannelsUiState,
    client: ChannelsClient,
    accountIdentitySeed: ByteArray,
    sessionToken: String,
    onClose: () -> Unit,
    onCreated: (String) -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var description by remember { mutableStateOf("") }
    // G4-5: optional public `@handle` (OPRF-discoverable). Empty →
    // discovery is skipped; the raw hex channel id still works.
    var handle by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("New channel", color = Theme.textPrimary) },
                navigationIcon = {
                    IconButton(onClick = onClose) {
                        Icon(Icons.Default.ArrowBack, "Back", tint = Theme.textPrimary)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Theme.background,
                ),
            )
        },
        containerColor = Theme.background,
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text("Name (local only)") },
                enabled = !busy,
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = description,
                onValueChange = { description = it },
                label = { Text("Description (optional, local only)") },
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
                keyboardOptions = KeyboardOptions.Default,
            )
            OutlinedTextField(
                value = handle,
                onValueChange = { handle = it },
                label = { Text("@handle (optional, public)") },
                enabled = !busy,
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                "A public handle lets others find this channel by @name. " +
                    "Leave blank to share the channel id only.",
                color = Theme.textSecondary,
                fontSize = 12.sp,
            )
            error?.let { msg ->
                Card(
                    colors = CardDefaults.cardColors(containerColor = Theme.destructive),
                ) {
                    Text(
                        msg,
                        color = Color.White,
                        fontSize = 13.sp,
                        modifier = Modifier.padding(12.dp),
                    )
                }
            }
            Button(
                onClick = {
                    if (busy || name.trim().isEmpty()) return@Button
                    busy = true
                    error = null
                    val id = ChannelsComposer.freshChannelIdHex()
                    val wantedHandle = handle.trim()
                    scope.launch {
                        try {
                            // G4-5: claim the optional `@handle` FIRST (before
                            // the channel exists on the channels service) so a
                            // first-come-first-served collision (409) aborts
                            // cleanly without leaving a half-created channel. An
                            // empty handle skips discovery entirely — the raw hex
                            // channel id still works. The mapping is `@handle ->
                            // channel_id`; the OPRF hash is all the server sees.
                            if (wantedHandle.isNotEmpty()) {
                                try {
                                    ChannelDiscovery.registerChannelHandle(
                                        handle = wantedHandle,
                                        channelIdHex = id,
                                        sessionToken = sessionToken,
                                    )
                                } catch (e: DiscoveryHttpError) {
                                    error = if (e.status == 409) {
                                        "The handle @${wantedHandle.removePrefix("@")} is " +
                                            "already taken — pick another (or leave it blank)."
                                    } else {
                                        "Could not register the handle: ${e.message}"
                                    }
                                    busy = false
                                    return@launch
                                }
                            }
                            client.createChannel(id)
                            // G-27 / ADR-0017: build a transient
                            // `Identity` from the canonical
                            // AccountIdentity (phrase) seed, hand it
                            // to the composer so the channel
                            // content-key seed comes from a real MLS
                            // PublisherGroup exporter (instead of
                            // `newChannelSeedForTest`).
                            val identity =
                                uniffi.lakko_ffi_uniffi.Identity.fromSeed(accountIdentitySeed)
                            val keys = try {
                                ChannelsComposer.freshPublisherKeys(
                                    identity = identity,
                                    channelIdHex = id,
                                    // G-29 — reuse the persisted per-channel
                                    // signing seed if this device published to
                                    // `id` before (null for a fresh id).
                                    existingSigningSeed =
                                        state.persistedPublisherSigningSeed(id),
                                )
                            } finally {
                                identity.close()
                            }
                            state.addChannel(
                                ChannelHandle(
                                    id = id,
                                    localName = name.trim(),
                                    localDescription = description.trim(),
                                    addedAtMs = System.currentTimeMillis().toULong(),
                                ),
                            )
                            state.setKeys(id, keys)
                            onCreated(id)
                        } catch (e: ChannelsApiException) {
                            error = "Server rejected: ${e.code} — ${e.detail}"
                        } catch (e: ChannelsComposerException) {
                            error = "Composer: ${e.message}"
                        } catch (e: Throwable) {
                            error = "Network error: ${e.message}"
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy && name.trim().isNotEmpty(),
                colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text(if (busy) "Creating…" else "Create")
            }
        }
    }
}

// ============================================================================
// ChannelDetailScreen
// ============================================================================

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelDetailScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    accountIdentitySeed: ByteArray,
    onBack: () -> Unit,
    onSubscribe: () -> Unit,
) {
    val channels by state.channels.collectAsState()
    val postsByChannel by state.postsByChannel.collectAsState()
    val epochByChannel by state.epochByChannel.collectAsState()
    val keysByChannel by state.keysByChannel.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    val posts = postsByChannel[channelId] ?: emptyList()
    val epoch = epochByChannel[channelId] ?: 0u
    val keys = keysByChannel[channelId]

    var composeText by remember { mutableStateOf("") }
    var publishBusy by remember { mutableStateOf(false) }
    var publishError by remember { mutableStateOf<String?>(null) }
    var loading by remember { mutableStateOf(true) }
    val decodedByCursor = remember { mutableStateMapOf<ULong, PostRender>() }
    val scope = rememberCoroutineScope()

    suspend fun refresh() {
        loading = true
        try {
            val fetched = client.fetchPosts(channelId, 0u)
            val ep = client.fetchEpoch(channelId)
            state.setPosts(channelId, fetched.posts)
            state.setEpoch(channelId, ep)
            // Decode every post we now have keys for. The
            // publisher holds the current-epoch content seed
            // (`keys.seed` from the PublisherGroup exporter), so
            // a post it published under an earlier epoch shows
            // as unreadable here — the subscriber-side
            // epoch->seed map (ADR-0031) is what recovers older
            // windows; the publisher does not retain old seeds.
            decodedByCursor.clear()
            keys?.let { k ->
                for (p in fetched.posts) {
                    decodedByCursor[p.cursor] = try {
                        PostRender.Plain(
                            ChannelsComposer.verifyAndDecrypt(
                                seed = k.seed,
                                channelIdHex = channelId,
                                postWireB64Url = p.payloadB64,
                            ).toString(Charsets.UTF_8)
                        )
                    } catch (_: Throwable) {
                        PostRender.Unreadable
                    }
                }
            }
        } catch (e: ChannelsApiException) {
            publishError = "Fetch failed: ${e.code} — ${e.detail}"
        } catch (e: Throwable) {
            publishError = "Network error: ${e.message}"
        } finally {
            loading = false
        }
    }

    LaunchedEffect(channelId) { refresh() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(
                        channel?.localName?.ifEmpty { "(channel)" } ?: "(channel)",
                        color = Theme.textPrimary,
                    )
                },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, "Back", tint = Theme.textPrimary)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Theme.background,
                ),
            )
        },
        containerColor = Theme.background,
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text(
                channelId,
                color = Theme.textSecondary,
                fontFamily = FontFamily.Monospace,
                fontSize = 12.sp,
            )
            channel?.localDescription?.takeIf { it.isNotEmpty() }?.let {
                Text(it, color = Theme.textSecondary, fontSize = 14.sp)
            }
            Row {
                Text("Server epoch: ", color = Theme.textSecondary, fontSize = 13.sp)
                Text(
                    "$epoch",
                    color = Theme.textPrimary,
                    fontFamily = FontFamily.Monospace,
                    fontSize = 13.sp,
                )
            }

            if (keys != null) {
                // Shared publisher-group handle for this channel. Rotation
                // AND co-publisher management both mutate the SAME MLS group
                // — `rotate()` advances the epoch DURABLY (persisted), so two
                // live handles would diverge after a rotation. The Rust handle
                // is Mutex-guarded, so the rotation heartbeat and the
                // co-publisher poll loop share it safely (serialised). Opened
                // once per channel; left to the UniFFI cleaner on dispose, the
                // same posture the rest of this screen uses for MLS handles.
                var pubIdentity by remember(channelId) { mutableStateOf<Identity?>(null) }
                var pubGroup by remember(channelId) { mutableStateOf<PublisherGroup?>(null) }
                var pubOpenError by remember(channelId) { mutableStateOf<String?>(null) }
                LaunchedEffect(channelId) {
                    try {
                        val id = Identity.fromSeed(accountIdentitySeed)
                        val pg = CoPublisher.openPublisherGroup(id, channelId)
                        pubIdentity = id
                        pubGroup = pg
                    } catch (e: Throwable) {
                        pubOpenError = "Could not open the channel group: ${e.message}"
                    }
                }

                ComposerSection(
                    composeText = composeText,
                    onComposeChange = { composeText = it },
                    busy = publishBusy,
                    error = publishError,
                    onPublish = {
                        val text = composeText.trim()
                        if (publishBusy || text.isEmpty()) return@ComposerSection
                        publishBusy = true
                        publishError = null
                        scope.launch {
                            try {
                                val counter = state.nextCounter(channelId)
                                val wire = ChannelsComposer.compose(
                                    keys = keys,
                                    channelIdHex = channelId,
                                    epochId = epoch,
                                    messageCounter = counter,
                                    plaintext = text.toByteArray(Charsets.UTF_8),
                                )
                                client.publishPost(channelId, wire)
                                composeText = ""
                                refresh()
                            } catch (e: ChannelsApiException) {
                                publishError = "Server rejected: ${e.code} — ${e.detail}"
                            } catch (e: ChannelsComposerException) {
                                publishError = "Compose error: ${e.message}"
                            } catch (e: Throwable) {
                                publishError = "Error: ${e.message}"
                            } finally {
                                publishBusy = false
                            }
                        }
                    },
                )
                SubscribersPanel(
                    channelId = channelId,
                    state = state,
                    client = client,
                )
                RotationSection(
                    channelIdHex = channelId,
                    client = client,
                    identity = pubIdentity,
                    publisherGroup = pubGroup,
                    openError = pubOpenError,
                )
                CoPublishersPanel(
                    channelId = channelId,
                    client = client,
                    identity = pubIdentity,
                    publisherGroup = pubGroup,
                    openError = pubOpenError,
                )
            } else {
                SubscriberPlaceholderSection(onOpenSubscribe = onSubscribe)
            }

            Text(
                "Posts",
                color = Theme.textPrimary,
                fontWeight = FontWeight.SemiBold,
                fontSize = 16.sp,
            )
            if (posts.isEmpty()) {
                Text(
                    if (loading) "Loading…" else "No posts yet. Publish the first one above.",
                    color = Theme.textSecondary,
                )
            } else {
                for (post in posts) {
                    PostRow(post, render = decodedByCursor[post.cursor])
                }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable
private fun ComposerSection(
    composeText: String,
    onComposeChange: (String) -> Unit,
    busy: Boolean,
    error: String?,
    onPublish: () -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        OutlinedTextField(
            value = composeText,
            onValueChange = onComposeChange,
            label = { Text("Write a post…") },
            enabled = !busy,
            modifier = Modifier.fillMaxWidth(),
        )
        Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
            Button(
                onClick = onPublish,
                enabled = !busy && composeText.trim().isNotEmpty(),
                colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
            ) {
                Text(if (busy) "Publishing…" else "Publish")
            }
        }
        error?.let { msg ->
            Card(colors = CardDefaults.cardColors(containerColor = Theme.destructive)) {
                Text(
                    msg,
                    color = Color.White,
                    fontSize = 13.sp,
                    modifier = Modifier.padding(12.dp),
                )
            }
        }
    }
}

@Composable
private fun SubscriberPlaceholderSection(onOpenSubscribe: () -> Unit) {
    Card(
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(Modifier.padding(12.dp)) {
            Text(
                "No local publisher keys for this channel — you cannot publish to it from this device.",
                color = Theme.textSecondary,
                fontSize = 13.sp,
            )
            Spacer(Modifier.height(8.dp))
            TextButton(
                onClick = onOpenSubscribe,
                contentPadding = PaddingValues(0.dp),
            ) {
                Text(
                    "Read as a subscriber →",
                    color = Theme.accent,
                    fontSize = 13.sp,
                )
            }
        }
    }
}

/**
 * Per-post render state (ADR-0031). `null` means "still
 * decoding"; the three variants distinguish a decoded post
 * from the two non-readable cases the UI must show
 * differently.
 */
private sealed interface PostRender {
    /** Verified + decrypted text. */
    data class Plain(val value: String) : PostRender
    /** Seed present but verify / AEAD-open failed. */
    object Unreadable : PostRender
    /** ADR-0031 — the post's epoch seed is not in the
     *  retained window (older than N rotations, or a gap),
     *  so it can no longer be read. Never force-decrypted
     *  with a wrong-epoch seed (invariant #4). */
    object OlderKeyWindow : PostRender
}

@Composable
private fun PostRow(post: PostView, render: PostRender?) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(Modifier.padding(12.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(
                    "cursor ${post.cursor}",
                    color = Theme.textSecondary,
                    fontSize = 12.sp,
                    fontFamily = FontFamily.Monospace,
                )
                Text(
                    isoTimestamp(post.serverTsMs.toLong()),
                    color = Theme.textSecondary,
                    fontSize = 12.sp,
                    fontFamily = FontFamily.Monospace,
                )
            }
            Spacer(Modifier.height(6.dp))
            when (render) {
                null -> Text(
                    "Decoding…",
                    color = Theme.textSecondary,
                    fontStyle = FontStyle.Italic,
                    fontSize = 13.sp,
                )
                PostRender.Unreadable -> Text(
                    "[unreadable — wrong seed, tampered ciphertext, or another publisher's signing key]",
                    color = Theme.textSecondary,
                    fontStyle = FontStyle.Italic,
                    fontSize = 13.sp,
                )
                PostRender.OlderKeyWindow -> Text(
                    "[message from an older key window — no longer available]",
                    color = Theme.textSecondary,
                    fontStyle = FontStyle.Italic,
                    fontSize = 13.sp,
                )
                is PostRender.Plain -> Text(
                    render.value,
                    color = Theme.textPrimary,
                    fontSize = 14.sp,
                )
            }
        }
    }
}

private fun isoTimestamp(ms: Long): String {
    val fmt = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss'Z'", Locale.US)
    fmt.timeZone = TimeZone.getTimeZone("UTC")
    return fmt.format(Date(ms))
}

// ============================================================================
// SubscribersPanel — publisher-side fan-out widget
// (embedded inside ChannelDetailScreen when the user holds
//  publisher keys for this channel)
// ============================================================================

/** Auto-fanout poll cadence — every 5 s the publisher
 *  panel re-fetches the subscriber list, diffs against
 *  the already-delivered set, and auto-seals an envelope
 *  for each new pubkey. Same cadence the subscribe-screen
 *  poll uses on the receive side. */
private const val SUBSCRIBERS_POLL_INTERVAL_MS: Long = 5_000L

@Composable
private fun SubscribersPanel(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
) {
    val keysByChannel by state.keysByChannel.collectAsState()
    val epochByChannel by state.epochByChannel.collectAsState()
    val keys = keysByChannel[channelId]
    val epoch = epochByChannel[channelId] ?: 0u

    var subscribers by remember { mutableStateOf<List<String>?>(null) }
    var busyPk by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    // statusByPk values: "delivered" (manual), "auto"
    // (polling loop), "failed". UI badges differentiate
    // delivered vs. auto-delivered visually.
    val statusByPk = remember { mutableStateMapOf<String, String>() }
    val scope = rememberCoroutineScope()

    suspend fun refresh() {
        try {
            subscribers = client.listSubscribers(channelId)
        } catch (e: ChannelsApiException) {
            error = "List failed: ${e.code} — ${e.detail}"
        } catch (e: Throwable) {
            error = "List failed: ${e.message}"
        }
    }

    suspend fun deliverTo(pubkeyHex: String, trigger: String) {
        // Manual delivery is one-at-a-time; the auto
        // path ignores busyPk so a slow manual click does
        // not stall progress on other subscribers.
        if (trigger == "manual" && busyPk != null) return
        if (keys == null) {
            if (trigger == "manual") {
                error = "No publisher keys on this device."
            }
            return
        }
        if (trigger == "manual") {
            busyPk = pubkeyHex
            error = null
        }
        try {
            val parsed = hexToBytesOrNull(pubkeyHex)
            if (parsed == null) {
                if (trigger == "manual") error = "Bad subscriber pubkey hex."
                statusByPk[pubkeyHex] = "failed"
                return
            }
            val wire = ChannelsComposer.sealSubscriberSeedEnvelope(
                subscriberPub = parsed,
                channelIdHex = channelId,
                epochId = epoch,
                seed = keys.seed,
            )
            // ADR-0031 — file the envelope at the exact epoch we
            // sealed it under, so the server retains it under
            // that epoch and the subscriber opens it with the
            // matching seed. `epoch` is the current server epoch
            // (never a future one), so the server accepts it.
            client.deliverEnvelope(channelId, pubkeyHex, wire, epoch = epoch.toInt())
            statusByPk[pubkeyHex] = if (trigger == "auto") "auto" else "delivered"
        } catch (e: ChannelsApiException) {
            if (trigger == "manual") error = "Deliver failed: ${e.code} — ${e.detail}"
            statusByPk[pubkeyHex] = "failed"
        } catch (e: ChannelsComposerException) {
            if (trigger == "manual") error = "Seal failed: ${e.message}"
            statusByPk[pubkeyHex] = "failed"
        } catch (e: Throwable) {
            if (trigger == "manual") error = "Error: ${e.message}"
            statusByPk[pubkeyHex] = "failed"
        } finally {
            if (trigger == "manual") busyPk = null
        }
    }

    LaunchedEffect(channelId) { refresh() }

    // Auto-fanout polling loop. Cancelled when the
    // composable leaves the composition. Sequences the
    // per-subscriber seals — the UniFFI ChannelSigningKey
    // and ChannelSubscriberKeypair calls hit the Rust
    // runtime serially anyway, and serial avoids a burst
    // of N parallel seals on a fresh subscriber storm.
    LaunchedEffect(channelId) {
        while (true) {
            delay(SUBSCRIBERS_POLL_INTERVAL_MS)
            refresh()
            val list = subscribers ?: continue
            for (pk in list) {
                val st = statusByPk[pk]
                if (st == null || st == "failed") {
                    deliverTo(pk, "auto")
                }
            }
        }
    }

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(Modifier.padding(12.dp)) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "Subscribers",
                    color = Theme.textPrimary,
                    fontWeight = FontWeight.SemiBold,
                    fontSize = 15.sp,
                )
                Spacer(Modifier.width(6.dp))
                Text(
                    "(${subscribers?.size?.toString() ?: "…"})",
                    color = Theme.textSecondary,
                    fontSize = 14.sp,
                )
                Spacer(Modifier.fillMaxWidth().weight(1f))
                TextButton(
                    onClick = { scope.launch { refresh() } },
                    contentPadding = PaddingValues(horizontal = 8.dp, vertical = 0.dp),
                ) {
                    Text(
                        "Refresh",
                        color = Theme.textSecondary,
                        fontSize = 12.sp,
                    )
                }
            }
            error?.let { msg ->
                Spacer(Modifier.height(6.dp))
                Card(colors = CardDefaults.cardColors(containerColor = Theme.destructive)) {
                    Text(
                        msg,
                        color = Color.White,
                        fontSize = 12.sp,
                        modifier = Modifier.padding(8.dp),
                    )
                }
            }
            val subs = subscribers
            when {
                subs == null -> {
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "Loading subscribers…",
                        color = Theme.textSecondary,
                        fontSize = 12.sp,
                    )
                }
                subs.isEmpty() -> {
                    Spacer(Modifier.height(6.dp))
                    Text(
                        "No subscribers yet. Anyone with the channel id can open the subscribe screen and register their X25519 public key.",
                        color = Theme.textSecondary,
                        fontSize = 12.sp,
                    )
                }
                else -> {
                    Spacer(Modifier.height(6.dp))
                    for (pk in subs) {
                        SubscriberRow(
                            pubkeyHex = pk,
                            busy = busyPk == pk,
                            status = statusByPk[pk],
                            onDeliver = {
                                scope.launch { deliverTo(pk, "manual") }
                            },
                        )
                        Spacer(Modifier.height(6.dp))
                    }
                }
            }
        }
    }
}

@Composable
private fun SubscriberRow(
    pubkeyHex: String,
    busy: Boolean,
    status: String?,
    onDeliver: () -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = Theme.background),
        shape = RoundedCornerShape(6.dp),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 8.dp, vertical = 6.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                pubkeyHex,
                color = Theme.textSecondary,
                fontFamily = FontFamily.Monospace,
                fontSize = 11.sp,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
                modifier = Modifier.weight(1f),
            )
            Spacer(Modifier.width(6.dp))
            status?.let { s ->
                val (label, color) = when (s) {
                    "delivered" -> "delivered" to Theme.accent
                    "auto"      -> "auto-delivered" to Theme.accent
                    "failed"    -> "failed" to Theme.destructive
                    else        -> s to Theme.textSecondary
                }
                Text(
                    label,
                    color = color,
                    fontSize = 11.sp,
                )
                Spacer(Modifier.width(6.dp))
            }
            Button(
                onClick = onDeliver,
                enabled = !busy,
                colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
                contentPadding = PaddingValues(horizontal = 10.dp, vertical = 4.dp),
            ) {
                Text(
                    if (busy) "Sealing…" else "Deliver",
                    color = Color.White,
                    fontSize = 11.sp,
                )
            }
        }
    }
}

// ============================================================================
// ChannelSubscribeScreen — subscriber-side route
//   Generates (or restores) an X25519 keypair, registers
//   on the channel, polls every 3 s for a seed envelope,
//   opens it, then renders decoded posts using the
//   recovered seed.
// ============================================================================

private enum class SubscribeBootstrap {
    Idle, Registering, Polling, Active, ErrorState,
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ChannelSubscribeScreen(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onBack: () -> Unit,
) {
    val subscriberKeysByChannel by state.subscriberKeysByChannel.collectAsState()
    // subscribedSeedsByChannel collected to trigger
    // recomposition when a seed lands for a new epoch; the
    // helpers below read the latest value off `state` directly
    // to keep the read-modify-write under the StateFlow update
    // barrier.
    state.subscribedSeedsByChannel.collectAsState()
    val postsByChannel by state.postsByChannel.collectAsState()

    val keys = subscriberKeysByChannel[channelId]
    val posts = postsByChannel[channelId] ?: emptyList()

    var bootstrap by remember { mutableStateOf(SubscribeBootstrap.Idle) }
    var bootstrapError by remember { mutableStateOf<String?>(null) }
    val decodedByCursor = remember { mutableStateMapOf<ULong, PostRender>() }
    val scope = rememberCoroutineScope()
    var pollJob by remember { mutableStateOf<Job?>(null) }

    // ADR-0031 — open one epoch's seed envelope into the
    // per-channel epoch->seed map. `epoch` MUST be the epoch
    // the publisher sealed the envelope under (the plural
    // endpoint reports it per entry); a wrong epoch makes the
    // HPKE open fail (AAD mismatch) and is skipped by the
    // caller — never a wrong seed.
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

    // ADR-0031 catch-up + re-sync. Fetch the server's retained
    // set (up to N=4 epochs) and open each into the epoch->seed
    // map; fall back to the singular latest-envelope endpoint to
    // guarantee the current seed is present; and if we still
    // lack a seed for the current epoch (offline past the
    // retention window), re-subscribe (idempotent) so the
    // publisher reseals us on its next fanout. Returns true once
    // at least one epoch seed is available. Best-effort per
    // step — a transient failure on one endpoint never aborts
    // the others.
    suspend fun catchUp(pubHex: String): Boolean {
        // 1. The retained set (plural) — the authoritative
        //    (epoch, envelope) pairs.
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
            // 2. Singular fallback — guarantees the current
            //    seed is present even if the plural endpoint is
            //    unavailable. The latest envelope is the
            //    highest retained epoch (== current after a
            //    reseal); a wrong-epoch guess just fails to open
            //    and is skipped (fail-closed).
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
            // 3. Re-sync — still no seed for the current epoch
            //    (offline past retention). Re-subscribe so the
            //    publisher reseals us next fanout; older posts
            //    stay unreadable (the "older key window"
            //    placeholder).
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
                // ADR-0031 — decrypt each post with the seed
                // matching ITS epoch; a post whose epoch is not
                // in the retained map renders as an "older key
                // window" placeholder, never force-decrypted.
                decodedByCursor[p.cursor] = try {
                    when (
                        val r = ChannelsComposer.verifyAndDecryptForEpochs(
                            seedByEpoch = seeds,
                            channelIdHex = channelId,
                            postWireB64Url = p.payloadB64,
                        )
                    ) {
                        is ChannelsComposer.SubscriberPostResult.Decoded ->
                            PostRender.Plain(r.plaintext.toString(Charsets.UTF_8))
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
            // Status section already shows the bootstrap
            // state; swallow transient post-list errors.
        }
    }

    // Bootstrap on first composition.
    LaunchedEffect(channelId) {
        bootstrap = SubscribeBootstrap.Registering
        bootstrapError = null
        try {
            val kp = state.subscriberKeysByChannel.value[channelId]
                ?: ChannelsComposer.freshSubscriberKeys().also {
                    state.setSubscriberKeys(channelId, it)
                }
            val pubHex = ChannelsComposer.bytesToHex(kp.publicKey)
            client.subscribe(channelId, pubHex)

            // Cached seeds from a previous open? Show posts
            // immediately, then still catch up in case a
            // rotation added a newer epoch since.
            if (state.subscribedSeeds(channelId).isNotEmpty()) {
                bootstrap = SubscribeBootstrap.Active
                loadAndDecodePosts()
            }

            bootstrap = if (state.subscribedSeeds(channelId).isNotEmpty()) {
                SubscribeBootstrap.Active
            } else {
                SubscribeBootstrap.Polling
            }

            // ADR-0031 — first catch-up: open the retained set
            // (plural), the singular latest, and re-sync if
            // needed. Covers the "publisher already fanned out
            // before this screen opened" case.
            if (catchUp(pubHex)) {
                bootstrap = SubscribeBootstrap.Active
                loadAndDecodePosts()
                return@LaunchedEffect
            }

            // 3-second poll loop — retry catch-up until a seed
            // arrives.
            pollJob = scope.launch {
                while (true) {
                    delay(3_000L)
                    try {
                        if (catchUp(pubHex)) {
                            bootstrap = SubscribeBootstrap.Active
                            loadAndDecodePosts()
                            break
                        }
                    } catch (_: Throwable) {
                        // Transient — keep polling.
                    }
                }
            }
        } catch (e: ChannelsApiException) {
            bootstrapError = "${e.code} — ${e.detail}"
            bootstrap = SubscribeBootstrap.ErrorState
        } catch (e: Throwable) {
            bootstrapError = e.message ?: "unknown error"
            bootstrap = SubscribeBootstrap.ErrorState
        }
    }

    DisposableEffect(channelId) {
        onDispose { pollJob?.cancel() }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Subscribe", color = Theme.textPrimary) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, "Back", tint = Theme.textPrimary)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Theme.background,
                ),
            )
        },
        containerColor = Theme.background,
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .padding(horizontal = 16.dp)
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(14.dp),
        ) {
            Text(
                channelId,
                color = Theme.textSecondary,
                fontFamily = FontFamily.Monospace,
                fontSize = 12.sp,
            )

            keys?.let { kp ->
                Card(
                    modifier = Modifier.fillMaxWidth(),
                    colors = CardDefaults.cardColors(containerColor = Theme.surface),
                    shape = RoundedCornerShape(8.dp),
                ) {
                    Column(Modifier.padding(12.dp)) {
                        Text(
                            "Your subscriber public key (registered on this channel):",
                            color = Theme.textSecondary,
                            fontSize = 12.sp,
                        )
                        Spacer(Modifier.height(4.dp))
                        Text(
                            ChannelsComposer.bytesToHex(kp.publicKey),
                            color = Theme.textPrimary,
                            fontFamily = FontFamily.Monospace,
                            fontSize = 11.sp,
                            maxLines = 2,
                            overflow = TextOverflow.Ellipsis,
                        )
                    }
                }
            }

            val (statusText, statusColor) = when (bootstrap) {
                SubscribeBootstrap.Idle, SubscribeBootstrap.Registering ->
                    "Registering your key on the channel…" to Theme.textSecondary
                SubscribeBootstrap.Polling ->
                    "Registered. Waiting for the publisher to fan out a seed envelope… (auto-refreshes every 3 s)" to Theme.textSecondary
                SubscribeBootstrap.Active ->
                    "Seed envelope opened. You can read posts on this channel." to Theme.accent
                SubscribeBootstrap.ErrorState ->
                    (bootstrapError ?: "Unknown error.") to Theme.destructive
            }
            Card(
                modifier = Modifier.fillMaxWidth(),
                colors = CardDefaults.cardColors(containerColor = Theme.surface),
                shape = RoundedCornerShape(8.dp),
            ) {
                Text(
                    statusText,
                    color = statusColor,
                    fontSize = 13.sp,
                    modifier = Modifier.padding(12.dp),
                )
            }

            Text(
                "Posts",
                color = Theme.textPrimary,
                fontWeight = FontWeight.SemiBold,
                fontSize = 16.sp,
            )
            when {
                bootstrap == SubscribeBootstrap.Active && posts.isNotEmpty() -> {
                    for (post in posts) {
                        PostRow(post, render = decodedByCursor[post.cursor])
                    }
                }
                bootstrap == SubscribeBootstrap.Active -> Text(
                    "No posts yet on this channel.",
                    color = Theme.textSecondary,
                )
                bootstrap == SubscribeBootstrap.Polling -> Text(
                    "Waiting for the publisher to deliver your seed.",
                    color = Theme.textSecondary,
                )
                else -> Text(
                    "Bootstrapping the subscribe flow…",
                    color = Theme.textSecondary,
                )
            }
            Spacer(Modifier.height(16.dp))
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
// RotationSection — publisher-side content-key rotation (ADR-0010 / C-54)
//   Activates the scheduled RotationCron heartbeat for a channel the local
//   user publishes, plus a manual "Rotate now" control. Both drive the
//   audited rotate + reseal-subscribers fanout (POST /v1/channels/:id/rotate).
// ============================================================================

/**
 * Publisher-side rotation control. Started only when the shared publisher
 * handle is open (invariant #4 — no rotation without a valid publisher
 * handle + keys); cancelled when the section leaves the composition.
 *
 * The Rust core (`crates/lakko-channels/src/rotation_policy.rs`) owns the
 * *when* via the pure `channelShouldRotate`; [RotationCron] owns the *how*
 * (rotate the MLS epoch → reseal every subscriber → atomic `/rotate` fanout).
 *
 * **Genesis-state seeding is load-bearing.** The default policy is 24 h /
 * 1000 posts, and `channelShouldRotate(default, {0,0}, now)` fires
 * IMMEDIATELY against any real wall-clock now (the Rust doc mandates seeding
 * with `after_rotation(now)` right after creating the group). We therefore
 * seed the heartbeat state as "rotated just now", so an opened screen never
 * triggers a spurious rotation + subscriber-reseal storm. Android has no
 * persistent rotation-state store yet, so within a single session the
 * scheduled trigger stays silent and "Rotate now" is the immediate control;
 * a future persistence layer feeds a real historical last-rotation into
 * `loadState` to drive true scheduled rotation.
 */
@Composable
private fun RotationSection(
    channelIdHex: String,
    client: ChannelsClient,
    identity: Identity?,
    publisherGroup: PublisherGroup?,
    openError: String?,
) {
    // Single cross-platform source of truth for the cadence.
    val policy = remember { channelRotationPolicyDefault() }
    var state by remember(channelIdHex) {
        mutableStateOf(channelRotationStateAfterRotation(System.currentTimeMillis().toULong()))
    }
    var busy by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()

    // Periodic rotation heartbeat — armed once the shared publisher handle is
    // open, cancelled when this section leaves the composition. A no-op tick
    // (policy says no) costs zero network round-trips.
    DisposableEffect(identity, publisherGroup) {
        val id = identity
        val pg = publisherGroup
        val job: Job? = if (id != null && pg != null) {
            RotationCron(
                publisherGroup = pg,
                identity = id,
                channelIdHex = channelIdHex,
                channelsClient = client,
                saveState = { next -> state = next },
                onRotated = { epoch -> status = "Scheduled rotation — new server epoch $epoch." },
            ).start(
                scope = scope,
                policy = policy,
                loadState = { state },
                onError = { e -> error = "Scheduled rotation failed: ${e.message}" },
            )
        } else {
            null
        }
        onDispose { job?.cancel() }
    }

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                "Content-key rotation",
                color = Theme.textPrimary,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                "Rotate the channel content key now — advances the MLS epoch and " +
                    "reseals the new seed to every subscriber (backward secrecy).",
                color = Theme.textSecondary,
                fontSize = 12.sp,
            )
            Button(
                onClick = {
                    val id = identity
                    val pg = publisherGroup
                    if (busy || id == null || pg == null) return@Button
                    busy = true
                    error = null
                    status = "Rotating…"
                    scope.launch {
                        try {
                            val cron = RotationCron(
                                publisherGroup = pg,
                                identity = id,
                                channelIdHex = channelIdHex,
                                channelsClient = client,
                                saveState = { next -> state = next },
                            )
                            // Force via the run-once path: a genesis {0,0} state
                            // makes channelShouldRotate fire against any real
                            // wall-clock now. runOnce rotates + reseals every
                            // subscriber atomically, then returns a fresh
                            // after_rotation(now) state we adopt as the new
                            // heartbeat baseline.
                            val result = cron.runOnce(
                                policy = policy,
                                state = ChannelRotationState(
                                    lastRotationMs = 0uL,
                                    postsSinceRotation = 0u,
                                ),
                                nowMs = System.currentTimeMillis().toULong(),
                            )
                            state = result.newState
                            status = if (result.rotated) {
                                "Rotated — new server epoch ${result.newEpoch}."
                            } else {
                                "No rotation needed."
                            }
                        } catch (e: Throwable) {
                            error = "Rotate failed: ${e.message}"
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy && publisherGroup != null,
                colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
            ) {
                Text(if (busy) "Rotating…" else "Rotate now", color = Color.White)
            }
            status?.let { Text(it, color = Theme.textSecondary, fontSize = 12.sp) }
            (openError ?: error)?.let {
                Text(it, color = Theme.destructive, fontSize = 12.sp)
            }
        }
    }
}

// ============================================================================
// CoPublishersPanel — owner-side co-publisher management (ADR-0028 OQ3)
// ============================================================================

/**
 * Owner-side co-publisher management panel. Holds a long-lived PublisherGroup +
 * Identity (the approve path needs the same handle the poll loop populated the
 * pending set on). Opens both on appear, polls the journal every 5 s, and
 * surfaces create-link + approve/reject. Kotlin mirror of the iOS
 * `CoPublishersPanelView`.
 */
@Composable
private fun CoPublishersPanel(
    channelId: String,
    client: ChannelsClient,
    identity: Identity?,
    publisherGroup: PublisherGroup?,
    openError: String?,
) {
    var link by remember { mutableStateOf<String?>(null) }
    var pending by remember { mutableStateOf<List<ByteArray>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    val ds = remember { DsClient() }
    val scope = rememberCoroutineScope()

    // Poll the channel's PublisherGroup journal for incoming co-publisher
    // external-join requests using the SHARED handle the parent owns (see
    // ChannelDetailScreen). Runs once the handle is open; re-armed if the
    // handle re-opens.
    LaunchedEffect(publisherGroup) {
        val id = identity ?: return@LaunchedEffect
        val pg = publisherGroup ?: return@LaunchedEffect
        while (true) {
            try {
                CoPublisher.pollJournal(ds, id, pg)
                pending = CoPublisher.pending(pg)
            } catch (_: Throwable) {
                // best-effort — keep the last good pending list
            }
            delay(5_000L)
        }
    }

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        shape = RoundedCornerShape(8.dp),
    ) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text(
                "Co-publishers",
                color = Theme.textPrimary,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                "Invite another device or person to publish to this channel.",
                color = Theme.textSecondary,
                fontSize = 12.sp,
            )
            Button(
                onClick = {
                    val id = identity
                    val pg = publisherGroup
                    if (busy || id == null || pg == null) return@Button
                    busy = true
                    error = null
                    scope.launch {
                        try {
                            link = CoPublisher.createCoPublisherLink(id, pg)
                        } catch (e: Throwable) {
                            error = "Could not create the link: ${e.message}"
                        } finally {
                            busy = false
                        }
                    }
                },
                enabled = !busy && publisherGroup != null,
                colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
            ) { Text("Create co-publisher link", color = Color.White) }

            link?.let {
                SelectionContainer {
                    Text(
                        it,
                        color = Theme.textSecondary,
                        fontFamily = FontFamily.Monospace,
                        fontSize = 12.sp,
                    )
                }
            }

            if (pending.isNotEmpty()) {
                Text(
                    "Requests (${pending.size})",
                    color = Theme.textPrimary,
                    fontWeight = FontWeight.SemiBold,
                )
                for (ref in pending) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            shortHex(ref),
                            color = Theme.textSecondary,
                            fontFamily = FontFamily.Monospace,
                            fontSize = 12.sp,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(
                            onClick = {
                                val id = identity
                                val pg = publisherGroup
                                if (busy || id == null || pg == null) return@TextButton
                                busy = true
                                error = null
                                scope.launch {
                                    try {
                                        CoPublisher.approve(ds, client, id, pg, ref, channelId)
                                        pending = CoPublisher.pending(pg)
                                    } catch (e: Throwable) {
                                        error = "Approve failed: ${e.message}"
                                    } finally {
                                        busy = false
                                    }
                                }
                            },
                            enabled = !busy,
                        ) { Text("Approve", color = Theme.accent) }
                        TextButton(
                            onClick = {
                                val pg = publisherGroup ?: return@TextButton
                                if (busy) return@TextButton
                                error = null
                                try {
                                    CoPublisher.reject(pg, ref)
                                    pending = CoPublisher.pending(pg)
                                } catch (e: Throwable) {
                                    error = "Reject failed: ${e.message}"
                                }
                            },
                            enabled = !busy,
                        ) { Text("Reject", color = Theme.destructive) }
                    }
                }
            }

            (openError ?: error)?.let {
                Text(it, color = Theme.destructive, fontSize = 12.sp)
            }
        }
    }
}

private fun shortHex(data: ByteArray): String {
    val sb = StringBuilder()
    for (i in 0 until minOf(8, data.size)) {
        sb.append(String.format("%02x", data[i].toInt() and 0xFF))
    }
    sb.append("…")
    return sb.toString()
}

// ============================================================================
// JoinCoPublisherScreen — joiner-side route (ADR-0028 OQ3)
// ============================================================================

/**
 * Joiner side: paste a `/g/` co-publisher link, request to join, poll until the
 * owner approves, then mint local publisher keys + open the channel. Kotlin
 * mirror of the web `onJoinAsCoPublisher` flow + the iOS `JoinCoPublisherScreen`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun JoinCoPublisherScreen(
    state: ChannelsUiState,
    accountIdentitySeed: ByteArray,
    onClose: () -> Unit,
    onJoined: (String) -> Unit,
) {
    var link by remember { mutableStateOf("") }
    var localName by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var status by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    val ds = remember { DsClient() }
    val scope = rememberCoroutineScope()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Join as co-publisher", color = Theme.textPrimary) },
                navigationIcon = {
                    IconButton(onClick = onClose) {
                        Icon(Icons.Default.ArrowBack, "Back", tint = Theme.textPrimary)
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = Theme.background,
                ),
            )
        },
        containerColor = Theme.background,
    ) { inner ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(inner)
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedTextField(
                value = link,
                onValueChange = { link = it },
                label = { Text("Paste a /g/ co-publisher link") },
                enabled = !busy,
                modifier = Modifier.fillMaxWidth(),
            )
            OutlinedTextField(
                value = localName,
                onValueChange = { localName = it },
                label = { Text("Local name (optional)") },
                enabled = !busy,
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )
            Text(
                "The channel owner shares a co-publisher link. After they approve " +
                    "your request, this device can publish to the channel.",
                color = Theme.textSecondary,
                fontSize = 13.sp,
            )
            Button(
                onClick = {
                    val trimmed = link.trim()
                    if (busy || trimmed.isEmpty()) return@Button
                    busy = true
                    error = null
                    status = "Requesting…"
                    scope.launch {
                        try {
                            val identity =
                                uniffi.lakko_ffi_uniffi.Identity.fromSeed(accountIdentitySeed)
                            val request =
                                CoPublisher.requestToCoPublish(ds, identity, trimmed)
                            status = "Waiting for the channel owner to approve…"
                            var keys: ChannelPublisherKeys? = null
                            var attempts = 0
                            while (keys == null && attempts < 60) {
                                keys = CoPublisher.pollForCoPublishWelcome(
                                    ds,
                                    identity,
                                    request.groupId,
                                    request.joinerKey,
                                    request.channelIdHex,
                                    // G-29 — reuse the persisted signing seed
                                    // on a re-poll.
                                    state.persistedPublisherSigningSeed(
                                        request.channelIdHex),
                                )
                                if (keys == null) {
                                    delay(3_000L)
                                    attempts += 1
                                }
                            }
                            val resolved = keys
                            if (resolved == null) {
                                error = "Timed out waiting for approval — try again " +
                                    "once the owner has approved."
                                return@launch
                            }
                            val name = localName.trim()
                            state.addChannel(
                                ChannelHandle(
                                    id = request.channelIdHex,
                                    localName = name.ifEmpty { "Co-published channel" },
                                    localDescription = "",
                                    addedAtMs = System.currentTimeMillis().toULong(),
                                ),
                            )
                            state.setKeys(request.channelIdHex, resolved)
                            onJoined(request.channelIdHex)
                        } catch (e: Throwable) {
                            error = "Co-publish failed: ${e.message}"
                        } finally {
                            busy = false
                            status = null
                        }
                    }
                },
                enabled = !busy && link.trim().isNotEmpty(),
                colors = ButtonDefaults.buttonColors(containerColor = Theme.accent),
            ) { Text(if (busy) "Requesting…" else "Request", color = Color.White) }

            status?.let { Text(it, color = Theme.textSecondary, fontSize = 13.sp) }
            error?.let { msg ->
                Card(colors = CardDefaults.cardColors(containerColor = Theme.destructive)) {
                    Text(
                        msg,
                        color = Color.White,
                        fontSize = 13.sp,
                        modifier = Modifier.padding(12.dp),
                    )
                }
            }
        }
    }
}
