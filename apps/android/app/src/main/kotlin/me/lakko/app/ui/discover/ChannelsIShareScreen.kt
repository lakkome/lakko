// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.discover

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.chat.ChatRepository
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.chat.conversationTitle

/**
 * Settings -> Security -> Channels I share (docs/ux/feed.md 6.3): every group
 * the "Share my channels" switch is on in, so it can be turned off in one
 * place. Turning it off sends the group an empty list (ADR-0059 section 6).
 * Mirror of web `ChannelsIShare.tsx`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChannelsIShareScreen(chatRepository: ChatRepository, onBack: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val shared = chatRepository.sharedChannels
    LaunchedEffect(Unit) { runCatching { shared.open() } }
    val row by shared.row.collectAsStateWithLifecycle()
    val chatState by chatRepository.state.collectAsStateWithLifecycle()
    val directory = remember(chatRepository) { ProfileDirectory.get(chatRepository.context) }
    var busy by remember { mutableStateOf<String?>(null) }
    var error by remember { mutableStateOf<String?>(null) }
    val held = chatState.threads.map { it.groupIdHex }.toSet()
    val groups = row.shareOn.filter { it in held }.sorted()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Channels I share") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                "Groups where people can see which channels you follow. Lakko's server cannot.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (groups.isEmpty()) {
                Text(
                    "You don't share your channels with any group.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            for (hex in groups) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    Text(
                        conversationTitle(context, chatRepository, directory, hex),
                        style = MaterialTheme.typography.bodyLarge,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(
                        enabled = busy == null,
                        onClick = {
                            busy = hex
                            error = null
                            scope.launch {
                                try {
                                    shared.setSharing(hex, false)
                                } catch (e: kotlinx.coroutines.CancellationException) {
                                    throw e
                                } catch (_: Throwable) {
                                    error = "Couldn't turn sharing off. Try again."
                                } finally {
                                    busy = null
                                }
                            }
                        },
                    ) { Text("Stop sharing") }
                }
            }
            error?.let { Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall) }
        }
    }
}
