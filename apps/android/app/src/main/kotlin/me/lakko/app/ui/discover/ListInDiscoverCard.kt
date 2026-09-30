// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.discover

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.discover.DiscoverCopy
import me.lakko.app.discover.DiscoverListing
import me.lakko.app.discover.ListingNeedsHandle
import me.lakko.app.discover.appLanguage
import me.lakko.app.discover.languageLabel
import me.lakko.app.discover.languageOptions
import java.util.Locale

/**
 * "List in Discover" on the channel manage screen (docs/ux/feed.md 6.4,
 * ADR-0059). Mirror of web `ListInDiscover.tsx`. The switch is the OWNER's:
 * the listing is signed by a key only the account that created the channel
 * derives. A co-publisher sees the state and no switch. Whether this account
 * is the owner is asked of the server once (an owner-signed read it refuses to
 * a co-publisher).
 */
@Composable
public fun ListInDiscoverCard(channelId: String, state: ChannelsUiState, client: ChannelsClient) {
    val channels by state.channels.collectAsState()
    val channel = channels.firstOrNull { it.id == channelId }
    val publisher = state.publisher
    val scope = rememberCoroutineScope()
    var owner by remember(channelId) { mutableStateOf<Boolean?>(null) }
    var listed by remember(channelId) { mutableStateOf<Boolean?>(null) }
    val topics = remember {
        try {
            uniffi.lakko_ffi_uniffi.channelTopics().map { it.id.toInt() to it.name }
        } catch (_: Throwable) {
            emptyList()
        }
    }
    var topic by remember(channelId) { mutableStateOf(channel?.sharedTopic ?: 0) }
    var language by remember(channelId) {
        mutableStateOf(
            channel?.sharedLanguage?.takeIf { it.isNotEmpty() }
                ?: appLanguage(Locale.getDefault().toLanguageTag()),
        )
    }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf<String?>(null) }
    var confirming by remember { mutableStateOf(false) }
    val hasHandle = !channel?.handle.isNullOrEmpty()

    LaunchedEffect(channelId) {
        listed = try {
            DiscoverListing.listedStamp(client, channelId) != null
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
        owner = try {
            publisher?.let { DiscoverListing.probeOwner(client, it, channelId) }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }
    }

    suspend fun liveNow(): Boolean? =
        try {
            DiscoverListing.listedStamp(client, channelId) != null
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Throwable) {
            null
        }

    fun turnOn() {
        val pub = publisher ?: return
        busy = true
        listed = true
        scope.launch {
            try {
                DiscoverListing.listChannel(state, client, pub, channelId, topic, language)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (e: Throwable) {
                // The switch returns to off when the server refused the listing.
                val live = liveNow() == true
                listed = live
                message = when {
                    live -> "Listed. Subscribers will learn it with the next channel update."
                    e is ListingNeedsHandle -> DiscoverCopy.LISTING_NEEDS_HANDLE
                    else -> DiscoverCopy.LISTING_REFUSED
                }
            } finally {
                busy = false
            }
        }
    }

    fun turnOff() {
        val pub = publisher ?: return
        busy = true
        scope.launch {
            try {
                DiscoverListing.unlistChannel(state, client, pub, channelId)
                listed = false
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                val live = liveNow() == true
                listed = live
                message = if (live) "Couldn't remove the listing. Try again." else null
            } finally {
                busy = false
            }
        }
    }

    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        shape = MaterialTheme.shapes.medium,
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Text("List in Discover", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            val muted = MaterialTheme.colorScheme.onSurfaceVariant
            if (owner != true) {
                Text(
                    when (listed) {
                        true -> "This channel is listed in Discover. Only the channel's owner can change this."
                        false -> "This channel is not listed in Discover. Only the channel's owner can list it."
                        null -> "Checking whether this channel is listed..."
                    },
                    style = MaterialTheme.typography.bodySmall,
                    color = muted,
                )
            } else {
                Text(
                    "Anyone can find a listed channel's name, description and subscriber count. Posts stay encrypted.",
                    style = MaterialTheme.typography.bodySmall,
                    color = muted,
                )
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
                    Text(if (listed == true) "Listed" else "Not listed", modifier = Modifier.weight(1f))
                    Switch(
                        checked = listed == true,
                        enabled = !busy && listed != null,
                        onCheckedChange = { on ->
                            message = null
                            if (on) {
                                if (!hasHandle) {
                                    message = DiscoverCopy.LISTING_NEEDS_HANDLE
                                } else {
                                    confirming = true
                                }
                            } else {
                                turnOff()
                            }
                        },
                    )
                }
                if (listed != true) {
                    Picker(
                        label = "Topic",
                        value = topics.firstOrNull { it.first == topic }?.second ?: "Other",
                        options = topics.map { it.first to it.second },
                        enabled = !busy,
                        onPick = { topic = it },
                    )
                    Picker(
                        label = "Language",
                        value = languageLabel(language),
                        options = languageOptions(language).map { it to languageLabel(it) },
                        enabled = !busy,
                        onPick = { language = it },
                    )
                }
                if (!hasHandle) {
                    Text(DiscoverCopy.LISTING_NEEDS_HANDLE, style = MaterialTheme.typography.bodySmall, color = muted)
                }
            }
            message?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.primary) }
        }
    }

    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text("List this channel in Discover?") },
            text = { Text(DiscoverCopy.LISTING_CONFIRM) },
            confirmButton = {
                TextButton(onClick = {
                    confirming = false
                    turnOn()
                }) { Text("List") }
            },
            dismissButton = { TextButton(onClick = { confirming = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun <T> Picker(label: String, value: String, options: List<Pair<T, String>>, enabled: Boolean, onPick: (T) -> Unit) {
    var open by remember { mutableStateOf(false) }
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Text(label, modifier = Modifier.weight(1f))
        Box {
            TextButton(onClick = { open = true }, enabled = enabled) { Text(value) }
            DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                for ((key, text) in options) {
                    DropdownMenuItem(
                        text = { Text(text) },
                        onClick = {
                            open = false
                            onPick(key)
                        },
                    )
                }
            }
        }
    }
}
