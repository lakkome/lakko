// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.calls

import androidx.compose.foundation.clickable
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
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.semantics
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
import me.lakko.app.calls.callHistoryRowShape
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
import me.lakko.app.R
import me.lakko.app.ui.components.LkIcon
import me.lakko.app.ui.components.LkIconSize

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
 *
 * A call with no conversation (`New call`, or a call joined from a link;
 * section 5.1.1) has neither: its row carries the group-call glyph, the title
 * `Group call`, no call button, and is not a tap target.
 *
 * ## One action
 *
 * `docs/ux/call-links.md` section 2. The destination used to offer
 * "Create call link" and "Join by link" — two buttons of equal weight, neither
 * of which placed a call, so the place named after the act did not perform it.
 * There is now one action, [onNewCall], and it starts a group video call: no
 * media choice, no participants, no name, no form. Someone who wants to call
 * one particular contact has a faster route from that conversation, which is
 * why this button does not try to be it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun CallsScreen(
    repository: ChatRepository,
    /** Open the conversation a call belongs to. */
    onOpenConversation: (groupIdHex: String) -> Unit,
    /** Call back, in the mode the row records. */
    onCallBack: (groupIdHex: String, video: Boolean) -> Unit,
    /**
     * Start a group video call and show the call screen
     * (`docs/ux/call-links.md` section 2). Nothing is asked first.
     */
    onNewCall: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val context = LocalContext.current
    val directory = remember(repository) { ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val groupNameVersion by me.lakko.app.chat.GroupNameStore.version.collectAsStateWithLifecycle()
    val historyVersion by CallHistoryStore.version.collectAsStateWithLifecycle()
    val chatState by repository.state.collectAsStateWithLifecycle()
    val zone = remember { ZoneId.systemDefault() }
    val strings = me.lakko.app.l10n.LocalStrings.current
    val locale = remember(strings.language) { Locale.forLanguageTag(strings.language) }
    val use24h = remember(context) { me.lakko.app.l10n.DeviceFormats.is24Hour(context) }

    // Calls placed before the index existed live only in their conversations.
    // Filled once per account, so this screen never opens empty while the
    // conversation list next to it is visibly showing calls.
    LaunchedEffect(repository) { repository.backfillCallHistory() }

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
        topBar = {
            TopAppBar(
                title = { Text(me.lakko.app.l10n.t("nav.calls")) },
            )
        },
        floatingActionButton = {
            ExtendedFloatingActionButton(
                onClick = onNewCall,
                icon = { LkIcon(R.drawable.ic_lk_video, contentDescription = null) },
                text = { Text(me.lakko.app.l10n.t("call.history.new_call")) },
            )
        },
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
                    Column(horizontalAlignment = Alignment.CenterHorizontally) {
                        LkIcon(
                            R.drawable.ic_lk_phone,
                            contentDescription = null,
                            tint = MaterialTheme.colorScheme.onSurfaceVariant,
                            size = LkIconSize.Empty,
                        )
                        Spacer(Modifier.height(12.dp))
                        Text(
                            text = me.lakko.app.l10n.t("call.history.empty"),
                            style = MaterialTheme.typography.titleMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            } else {
                LazyColumn(
                    modifier = Modifier
                        .fillMaxWidth()
                        .weight(1f),
                    // Room under the last row for the floating `New call`
                    // button, which would otherwise sit on top of it.
                    contentPadding = PaddingValues(top = 4.dp, bottom = 88.dp),
                ) {
                    for (day in days) {
                        item(key = "day-${day.day}") {
                            Text(
                                text = dateSeparatorLabel(
                                    day.rows.first().startedAtMs,
                                    System.currentTimeMillis(),
                                    zone,
                                    locale,
                                    strings,
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
                                "${row.conversationIdHex ?: "call:" + row.callIdHex}-${row.startedAtMs}-$i"
                            },
                        ) { i ->
                            val row = day.rows[i]
                            val conversation = row.conversationIdHex
                            val time = formatClockTime(row.startedAtMs, zone, use24h, locale)
                            if (conversation == null) {
                                // docs/ux/navigation.md section 5.1.1: nothing
                                // to open and nothing to call back.
                                CallHistoryRow(
                                    entry = row,
                                    // A call has no name of its own.
                                    title = strings["call.label.group"],
                                    time = time,
                                    onOpen = null,
                                    onCallBack = null,
                                )
                            } else {
                                CallHistoryRow(
                                    entry = row,
                                    title = titleOf(context, repository, directory, conversation, strings),
                                    personKey = if (row.kind == me.lakko.app.chat.ConversationKind.Chat) {
                                        runCatching { repository.oneToOnePeerKey(conversation) }.getOrNull()
                                    } else {
                                        null
                                    },
                                    time = time,
                                    onOpen = { onOpenConversation(conversation) },
                                    onCallBack = { onCallBack(conversation, row.video) },
                                )
                            }
                        }
                    }
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
    conversationIdHex: String,
    strings: me.lakko.app.l10n.Strings,
): String = try {
    if (repository.memberCount(conversationIdHex) == null) {
        strings["call.history.unknown_conversation"]
    } else {
        conversationTitle(context, repository, directory, conversationIdHex)
    }
} catch (cause: Throwable) {
    android.util.Log.w("CallsScreen", "resolving the call row title failed", cause)
    strings["call.history.unknown_conversation"]
}

@Composable
private fun CallHistoryRow(
    entry: CallHistoryEntry,
    title: String,
    time: String,
    /** Opens the conversation; `null` when the row is not a tap target
     *  (a call with no conversation, section 5.1.1). */
    onOpen: (() -> Unit)?,
    /** Calls back; `null` when the row carries no call button. */
    onCallBack: (() -> Unit)?,
    personKey: ByteArray? = null,
) {
    val shape = callHistoryRowShape(entry)
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
                .then(
                    if (onOpen != null && shape.opensConversation) {
                        Modifier.clickable(onClick = onOpen)
                    } else {
                        // Read as one item by TalkBack, with no click action
                        // and no pressed state (section 5.1.1).
                        Modifier.semantics(mergeDescendants = true) {}
                    },
                )
                .padding(
                    start = 16.dp,
                    top = 10.dp,
                    bottom = 10.dp,
                    end = if (shape.callBack) 8.dp else 16.dp,
                ),
        ) {
            LakkoAvatar(
                name = title,
                seed = entry.conversationIdHex ?: entry.callIdHex.orEmpty(),
                size = 40.dp,
                group = shape.groupCallGlyph || entry.kind == me.lakko.app.chat.ConversationKind.Group,
                personKey = personKey,
                groupIdHex = entry.conversationIdHex.takeIf { entry.kind == me.lakko.app.chat.ConversationKind.Group },
                groupGlyph = shape.groupCallGlyph,
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
                    LkIcon(
                        icon = when (callDirectionGlyph(entry.outcome)) {
                            CallDirectionGlyph.OUTGOING -> R.drawable.ic_lk_phone_outgoing
                            CallDirectionGlyph.INCOMING -> R.drawable.ic_lk_phone_incoming
                            CallDirectionGlyph.MISSED -> R.drawable.ic_lk_phone_down
                        },
                        contentDescription = null,
                        tint = lineColor,
                        size = LkIconSize.Mark,
                    )
                    Spacer(Modifier.width(6.dp))
                    Text(
                        text = callRowText(entry.toTranscriptEntry(), me.lakko.app.l10n.LocalStrings.current),
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
        if (onCallBack != null && shape.callBack) {
            IconButton(
                onClick = onCallBack,
                modifier = Modifier.padding(end = 8.dp),
            ) {
                LkIcon(
                    icon = if (entry.video) R.drawable.ic_lk_video else R.drawable.ic_lk_phone,
                    contentDescription = me.lakko.app.l10n.t(if (entry.video) "chat.call.video" else "chat.call.voice"),
                )
            }
        }
    }
}
