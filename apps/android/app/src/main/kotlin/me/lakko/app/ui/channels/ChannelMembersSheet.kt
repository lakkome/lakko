// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import me.lakko.app.channels.ChannelMemberCards
import me.lakko.app.channels.ChannelMembersResult
import me.lakko.app.channels.ChannelsClient
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.channels.loadChannelMembers
import me.lakko.app.l10n.t
import me.lakko.app.reach.ReachManager
import me.lakko.app.reach.reachHex
import me.lakko.app.ui.components.LakkoAvatar

/**
 * A channel's member list (`docs/ux/who-can-find-me.md` section 6.3), opened
 * from the `Members` row of channel info. The Android twin of web
 * `MembersPanel.tsx`.
 *
 * Each row is a member card this device could open: the member's name, and
 * their `@username` only when they let this channel's members find them
 * (section 6.2). Cards sealed under an epoch this device holds no seed for
 * are counted, not shown.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ChannelMembersSheet(
    channelId: String,
    state: ChannelsUiState,
    client: ChannelsClient,
    onDismiss: () -> Unit,
) {
    val context = LocalContext.current
    var result by remember(channelId) { mutableStateOf<ChannelMembersResult?>(null) }
    var failed by remember(channelId) { mutableStateOf(false) }
    var attempt by remember(channelId) { mutableIntStateOf(0) }
    LaunchedEffect(channelId, attempt) {
        failed = false
        result = null
        result = try {
            val reach = ReachManager.get(context)
            loadChannelMembers(
                channelIdHex = channelId,
                seedForEpoch = { epoch -> ChannelMemberCards.seedForEpoch(state, channelId, epoch) },
                ownSubscriberPkHex = state.subscriberKeysByChannel.value[channelId]?.publicKey?.let(::reachHex),
                ports = ChannelMemberCards.ports(reach, client),
            )
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            failed = true
            null
        }
    }
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = 16.dp),
        ) {
            val loaded = result
            Text(
                if (loaded != null) {
                    t("channels.members.heading", mapOf("count" to loaded.rows.size.toString()))
                } else {
                    t("channels.members.title")
                },
                style = MaterialTheme.typography.titleLarge,
            )
            Spacer(Modifier.height(12.dp))
            when {
                failed -> {
                    Text(
                        t("channels.members.error.load"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.error,
                    )
                    TextButton(onClick = { attempt += 1 }) { Text(t("common.try_again")) }
                }
                loaded == null -> Text(
                    t("channels.members.loading"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                else -> {
                    if (loaded.rows.isEmpty()) {
                        Text(
                            t("channels.members.empty"),
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    for (row in loaded.rows) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                            horizontalArrangement = Arrangement.Start,
                        ) {
                            LakkoAvatar(name = row.name, seed = row.key, size = 40.dp, avatar = row.avatar)
                            Spacer(Modifier.width(12.dp))
                            Column {
                                Text(row.name, style = MaterialTheme.typography.bodyLarge)
                                row.username?.let {
                                    Text(
                                        "@$it",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                }
                            }
                        }
                    }
                    if (loaded.unreadable > 0) {
                        Spacer(Modifier.height(8.dp))
                        Text(
                            t("channels.members.unreadable", emptyMap(), mapOf("count" to loaded.unreadable.toULong())),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                    if (loaded.truncated) {
                        Text(
                            t("channels.members.truncated"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
            Spacer(Modifier.height(24.dp))
        }
    }
}
