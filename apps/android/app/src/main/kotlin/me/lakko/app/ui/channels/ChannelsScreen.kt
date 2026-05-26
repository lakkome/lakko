// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
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
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
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
            } else {
                SubscriberPlaceholderSection()
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
private fun SubscriberPlaceholderSection() {
    Card(
        colors = CardDefaults.cardColors(containerColor = Theme.surface),
        shape = RoundedCornerShape(8.dp),
    ) {
        Text(
            "No local publisher keys for this channel. Subscriber-side view + HPKE seed-envelope reception arrive in a follow-up sub-sprint.",
            color = Theme.textSecondary,
            fontSize = 13.sp,
            modifier = Modifier.padding(12.dp),
        )
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
