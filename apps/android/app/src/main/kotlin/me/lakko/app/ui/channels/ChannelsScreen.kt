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
import me.lakko.app.channels.ChannelHandle
import me.lakko.app.channels.ChannelsApiException
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsComposer
import me.lakko.app.channels.ChannelsComposerException
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.PostView
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
    data class Detail(val channelId: String) : Route()
    data class Subscribe(val channelId: String) : Route()
}

@Composable
public fun ChannelsRootScreen(
    state: ChannelsUiState = remember { ChannelsUiState() },
    client: ChannelsClient = remember { ChannelsClient() },
) {
    var route by remember { mutableStateOf<Route>(Route.List) }
    when (val r = route) {
        Route.List -> ChannelsListScreen(
            state = state,
            onCreate = { route = Route.New },
            onOpen = { route = Route.Detail(it) },
        )
        Route.New -> NewChannelScreen(
            state = state,
            client = client,
            onClose = { route = Route.List },
            onCreated = { id -> route = Route.Detail(id) },
        )
        is Route.Detail -> ChannelDetailScreen(
            channelId = r.channelId,
            state = state,
            client = client,
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
) {
    val channels by state.channels.collectAsState()
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Channels", color = Theme.textPrimary) },
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
        if (channels.isEmpty()) {
            Column(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(inner)
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
                    "Tap + to create one.",
                    color = Theme.textSecondary,
                    fontSize = 14.sp,
                )
            }
        } else {
            LazyColumn(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(inner)
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

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun NewChannelScreen(
    state: ChannelsUiState,
    client: ChannelsClient,
    onClose: () -> Unit,
    onCreated: (String) -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var description by remember { mutableStateOf("") }
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
                    scope.launch {
                        try {
                            client.createChannel(id)
                            val keys = ChannelsComposer.freshPublisherKeys()
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
    val decodedByCursor = remember { mutableStateMapOf<ULong, String?>() }
    val scope = rememberCoroutineScope()

    suspend fun refresh() {
        loading = true
        try {
            val fetched = client.fetchPosts(channelId, 0u)
            val ep = client.fetchEpoch(channelId)
            state.setPosts(channelId, fetched.posts)
            state.setEpoch(channelId, ep)
            // Decode every post we now have keys for.
            decodedByCursor.clear()
            keys?.let { k ->
                for (p in fetched.posts) {
                    try {
                        val pt = ChannelsComposer.verifyAndDecrypt(
                            seed = k.seed,
                            channelIdHex = channelId,
                            postWireB64Url = p.payloadB64,
                        )
                        decodedByCursor[p.cursor] = pt.toString(Charsets.UTF_8)
                    } catch (_: Throwable) {
                        decodedByCursor[p.cursor] = null
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
                    PostRow(post, decoded = decodedByCursor[post.cursor])
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

@Composable
private fun PostRow(post: PostView, decoded: String?) {
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
            when {
                decoded == null -> Text(
                    "Decoding…",
                    color = Theme.textSecondary,
                    fontStyle = FontStyle.Italic,
                    fontSize = 13.sp,
                )
                decoded.isEmpty() -> Text(
                    "[unreadable — wrong seed, tampered ciphertext, or another publisher's signing key]",
                    color = Theme.textSecondary,
                    fontStyle = FontStyle.Italic,
                    fontSize = 13.sp,
                )
                else -> Text(
                    decoded,
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

    LaunchedEffect(channelId) { refresh() }

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
                                if (busyPk != null || keys == null) return@SubscriberRow
                                busyPk = pk
                                error = null
                                scope.launch {
                                    try {
                                        val parsed = hexToBytesOrNull(pk)
                                        if (parsed == null) {
                                            error = "Bad subscriber pubkey hex."
                                            return@launch
                                        }
                                        val wire =
                                            ChannelsComposer.sealSubscriberSeedEnvelope(
                                                subscriberPub = parsed,
                                                channelIdHex = channelId,
                                                epochId = epoch,
                                                seed = keys.seed,
                                            )
                                        client.deliverEnvelope(channelId, pk, wire)
                                        statusByPk[pk] = "delivered"
                                    } catch (e: ChannelsApiException) {
                                        error = "Deliver failed: ${e.code} — ${e.detail}"
                                        statusByPk[pk] = "failed"
                                    } catch (e: ChannelsComposerException) {
                                        error = "Seal failed: ${e.message}"
                                        statusByPk[pk] = "failed"
                                    } catch (e: Throwable) {
                                        error = "Error: ${e.message}"
                                        statusByPk[pk] = "failed"
                                    } finally {
                                        busyPk = null
                                    }
                                }
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
                Text(
                    s,
                    color = if (s == "delivered") Theme.accent else Theme.destructive,
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
    // subscribedSeedByChannel collected to trigger
    // recomposition on seed installation; the helpers below
    // read the latest value off `state` directly to keep the
    // read-modify-write under the StateFlow update barrier.
    state.subscribedSeedByChannel.collectAsState()
    val postsByChannel by state.postsByChannel.collectAsState()

    val keys = subscriberKeysByChannel[channelId]
    val posts = postsByChannel[channelId] ?: emptyList()

    var bootstrap by remember { mutableStateOf(SubscribeBootstrap.Idle) }
    var bootstrapError by remember { mutableStateOf<String?>(null) }
    val decodedByCursor = remember { mutableStateMapOf<ULong, String?>() }
    val scope = rememberCoroutineScope()
    var pollJob by remember { mutableStateOf<Job?>(null) }

    suspend fun openAndCacheSeed(envelopeB64: String) {
        val kp = state.subscriberKeysByChannel.value[channelId]
            ?: throw ChannelsComposerException.Ffi("missing subscriber keypair")
        val wire = ChannelsComposer.base64UrlDecodeOrNull(envelopeB64)
            ?: throw ChannelsComposerException.BadBase64(envelopeB64)
        val epoch = client.fetchEpoch(channelId)
        val recovered = ChannelsComposer.openSubscriberSeedEnvelope(
            keys = kp,
            channelIdHex = channelId,
            epochId = epoch,
            wire = wire,
        )
        state.setSubscribedSeed(channelId, recovered)
        state.setEpoch(channelId, epoch)
    }

    suspend fun loadAndDecodePosts() {
        val s = state.subscribedSeedByChannel.value[channelId] ?: return
        try {
            val page = client.fetchPosts(channelId, 0u)
            state.setPosts(channelId, page.posts)
            decodedByCursor.clear()
            for (p in page.posts) {
                try {
                    val pt = ChannelsComposer.verifyAndDecrypt(
                        seed = s,
                        channelIdHex = channelId,
                        postWireB64Url = p.payloadB64,
                    )
                    decodedByCursor[p.cursor] = pt.toString(Charsets.UTF_8)
                } catch (_: Throwable) {
                    decodedByCursor[p.cursor] = null
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

            if (state.subscribedSeedByChannel.value[channelId] != null) {
                bootstrap = SubscribeBootstrap.Active
                loadAndDecodePosts()
                return@LaunchedEffect
            }

            bootstrap = SubscribeBootstrap.Polling
            // First-attempt fetch — publisher may have
            // already fanned out before this screen opened.
            val firstTry = client.fetchEnvelope(channelId, pubHex)
            if (firstTry != null) {
                openAndCacheSeed(firstTry)
                bootstrap = SubscribeBootstrap.Active
                loadAndDecodePosts()
                return@LaunchedEffect
            }

            // 3-second poll loop on the envelope endpoint.
            pollJob = scope.launch {
                while (true) {
                    delay(3_000L)
                    try {
                        val env = client.fetchEnvelope(channelId, pubHex)
                        if (env != null) {
                            openAndCacheSeed(env)
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
                        PostRow(post, decoded = decodedByCursor[post.cursor])
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
