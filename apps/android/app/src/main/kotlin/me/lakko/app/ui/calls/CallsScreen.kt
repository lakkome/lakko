// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.calls

import android.text.format.DateFormat
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
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.CallMade
import androidx.compose.material.icons.automirrored.filled.CallMissed
import androidx.compose.material.icons.automirrored.filled.CallReceived
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.Videocam
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.calls.CallDirectionGlyph
import me.lakko.app.calls.CallHistoryEntry
import me.lakko.app.calls.CallHistoryStore
import me.lakko.app.calls.callDirectionGlyph
import me.lakko.app.calls.callHistoryByDay
import me.lakko.app.calls.callHistoryIsUnanswered
import me.lakko.app.calls.callRowText
import me.lakko.app.calls.toTranscriptEntry
import me.lakko.app.chat.ChatRepository
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.chat.conversationTitle
import me.lakko.app.ui.chat.dateSeparatorLabel
import me.lakko.app.ui.chat.formatClockTime
import me.lakko.app.ui.components.LakkoAvatar
import java.time.ZoneId
import java.util.Locale

/**
 * Calls: recent calls across every conversation, newest first.
 *
 * `docs/ux/navigation.md` section 5. The destination was removed by the client
 * redesign on the recorded ground that it held no history — only a
 * create-link panel — and that was true of what existed. It was never an
 * argument against call history, and the design system's own information
 * architecture kept "Calls: recent calls" as destination 4 throughout.
 *
 * The call log has been written per conversation all along (a system row in the
 * encrypted message log); what was missing was the cut across conversations,
 * which is [me.lakko.app.calls.CallHistoryStore] and the pure rules beside it,
 * where the ordering, the day grouping and the codec are tested. This file
 * draws them.
 *
 * Two targets per row, and their hit areas must not overlap: the row opens the
 * conversation, the trailing button places the call — back in the same mode the
 * row records, so a video call re-dials video.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun CallsScreen(
    repository: ChatRepository,
    /** Open the conversation a call belongs to. */
    onOpenConversation: (groupIdHex: String) -> Unit,
    /** Call back, in the mode the row records. */
    onCallBack: (groupIdHex: String, video: Boolean) -> Unit,
    /** Open the "Create call link" form. */
    onCreateCallLink: () -> Unit,
    /** Join the call a pasted `/c/` link names. */
    onJoinByLink: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val directory = remember(repository) { ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val groupNameVersion by me.lakko.app.chat.GroupNameStore.version.collectAsStateWithLifecycle()
    val historyVersion by CallHistoryStore.version.collectAsStateWithLifecycle()
    val chatState by repository.state.collectAsStateWithLifecycle()
    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val use24h = remember(context) { DateFormat.is24HourFormat(context) }

    // The index, not a walk of every transcript — `docs/ux/navigation.md`
    // section 6.2. The thread COUNT is read so a conversation arriving after
    // this screen mounted re-titles the rows that name it; comparing the whole
    // state would compare every message in every thread on each recomposition.
    val threadCount = chatState.threads.size
    val days = remember(historyVersion, nameVersion, groupNameVersion, threadCount, zone) {
        val entries = CallHistoryStore.load(context, repository.currentAccountIdB64())
        callHistoryByDay(entries, zone)
    }

    Scaffold(
        modifier = modifier,
        topBar = { TopAppBar(title = { Text("Calls") }) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            if (days.isEmpty()) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f)
                        .padding(24.dp),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        text = "No calls yet",
                        style = MaterialTheme.typography.titleMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            } else {
                LazyColumn(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                    contentPadding = PaddingValues(vertical = 4.dp),
                ) {
                    for (day in days) {
                        item(key = "day-${day.day}") {
                            Text(
                                text = dateSeparatorLabel(
                                    day.rows.first().startedAtMs,
                                    System.currentTimeMillis(),
                                    zone,
                                    locale,
                                ),
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                            )
                        }
                        items(
                            count = day.rows.size,
                            key = { i ->
                                val row = day.rows[i]
                                "${row.conversationIdHex}-${row.startedAtMs}-$i"
                            },
                        ) { i ->
                            val row = day.rows[i]
                            CallHistoryRow(
                                entry = row,
                                title = titleOf(context, repository, directory, row),
                                time = formatClockTime(row.startedAtMs, zone, use24h, locale),
                                onOpen = { onOpenConversation(row.conversationIdHex) },
                                onCallBack = { onCallBack(row.conversationIdHex, row.video) },
                            )
                        }
                    }
                }
            }
            // Section 5.2 — the two actions the design system's information
            // architecture has always put here.
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = 16.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedButton(onClick = onCreateCallLink, modifier = Modifier.fillMaxWidth()) {
                    Icon(Icons.Filled.Videocam, contentDescription = null)
                    Spacer(Modifier.width(8.dp))
                    Text("Create call link")
                }
                OutlinedButton(onClick = onJoinByLink, modifier = Modifier.fillMaxWidth()) {
                    Icon(Icons.Filled.Link, contentDescription = null)
                    Spacer(Modifier.width(8.dp))
                    Text("Join by link")
                }
            }
        }
    }
}

/**
 * Who the call was with, named exactly as the conversation list names it.
 *
 * The shared title rule, not a second one: a chat called "Anna" in the list and
 * something else here is the drift this codebase keeps paying for. A call can
 * outlive the conversation it was placed from, so a conversation this device no
 * longer holds reads as "Unknown" rather than as a hex id.
 */
private fun titleOf(
    context: android.content.Context,
    repository: ChatRepository,
    directory: ProfileDirectory,
    row: CallHistoryEntry,
): String = try {
    if (repository.memberCount(row.conversationIdHex) == null) {
        "Unknown"
    } else {
        conversationTitle(context, repository, directory, row.conversationIdHex)
    }
} catch (cause: Throwable) {
    android.util.Log.w("CallsScreen", "resolving the call row title failed", cause)
    "Unknown"
}

@Composable
private fun CallHistoryRow(
    entry: CallHistoryEntry,
    title: String,
    time: String,
    onOpen: () -> Unit,
    onCallBack: () -> Unit,
) {
    val unanswered = callHistoryIsUnanswered(entry.outcome)
    val lineColor = if (unanswered) {
        MaterialTheme.colorScheme.error
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .weight(1f)
                .clickable(onClick = onOpen)
                .padding(start = 16.dp, top = 10.dp, bottom = 10.dp, end = 8.dp),
        ) {
            LakkoAvatar(
                name = title,
                seed = entry.conversationIdHex,
                size = 40.dp,
                group = entry.kind == me.lakko.app.chat.ConversationKind.Group,
            )
            Spacer(Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Text(
                    text = title,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                Spacer(Modifier.height(2.dp))
                Row(verticalAlignment = Alignment.CenterVertically) {
                    // Section 8 of docs/ux/calls.md — the glyph is never the
                    // only signal; the words next to it say the same thing.
                    Icon(
                        imageVector = when (callDirectionGlyph(entry.outcome)) {
                            CallDirectionGlyph.OUTGOING -> Icons.AutoMirrored.Filled.CallMade
                            CallDirectionGlyph.INCOMING -> Icons.AutoMirrored.Filled.CallReceived
                            CallDirectionGlyph.MISSED -> Icons.AutoMirrored.Filled.CallMissed
                        },
                        contentDescription = null,
                        tint = lineColor,
                        modifier = Modifier.size(14.dp),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = callRowText(entry.toTranscriptEntry()),
                        style = MaterialTheme.typography.bodyMedium,
                        color = lineColor,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = time,
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                    )
                }
            }
        }
        IconButton(
            onClick = onCallBack,
            modifier = Modifier.padding(end = 8.dp),
        ) {
            Icon(
                imageVector = if (entry.video) Icons.Filled.Videocam else Icons.Filled.Call,
                contentDescription = if (entry.video) "Video call" else "Call",
            )
        }
    }
}
