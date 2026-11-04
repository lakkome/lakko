// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
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
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.chat.FoundConversation
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoAvatar

/*
 * group-and-channel-privacy.md sections 5, 5.1 and 6.1 — public groups and
 * channels in search under "Groups and channels", the card, and asking to
 * join a public group. Subscribing to a channel stays as it is today
 * (phase B): a channel's card opens the existing subscribe page.
 */

/** What the search section needs from the app. */
public interface ConversationSearchPort {
    /** Section 5 — the public groups and channels matching [query]. */
    public suspend fun search(query: String): List<FoundConversation>

    /** Section 6.1 — send the request to join a public group. */
    public suspend fun requestToJoin(found: FoundConversation)

    /** Whether this device is already in the group (hex id): `Open`. */
    public fun isMember(idHex: String): Boolean

    /** Open a group this device is in, or a channel's subscribe page. */
    public fun open(found: FoundConversation)

    /** Section 5.1 — the card's Subscribe: subscribe to the channel at once
     *  and open it, under the name the card read. */
    public fun subscribe(found: FoundConversation) {
        open(found)
    }
}

private fun FoundConversation.idHex(): String = id.joinToString("") { "%02x".format(it) }

/** The band word of a size band (ADR-0059): `<10`, `10+`, ... */
private fun bandText(band: Int): String = when (band) {
    5 -> "100k+"
    4 -> "10k+"
    3 -> "1k+"
    2 -> "100+"
    1 -> "10+"
    else -> "<10"
}

/**
 * Section 5 — the public groups and channels matching [query], asked 500 ms
 * after the last keystroke from 3 characters; empty below that or on a
 * failure. Hoisted so the field knows whether anything matched at all.
 */
/** The public groups and channels a search found, and whether it is still
 *  asking. */
internal class FoundConversations(val rows: List<FoundConversation>, val pending: Boolean)

@Composable
internal fun rememberFoundConversations(query: String, port: ConversationSearchPort?): FoundConversations {
    var found by remember { mutableStateOf<List<FoundConversation>>(emptyList()) }
    var pending by remember { mutableStateOf(false) }
    LaunchedEffect(query, port != null) {
        found = emptyList()
        if (port == null || query.trim().length < 3) {
            pending = false
            return@LaunchedEffect
        }
        pending = true
        try {
            delay(500)
            found = try {
                port.search(query)
            } catch (cause: Throwable) {
                me.lakko.app.rethrowIfCancelled(cause)
                emptyList()
            }
        } finally {
            pending = false
        }
    }
    return FoundConversations(found, pending)
}

/** Section 5 — the public groups and channels in the one result list
 *  (who-can-find-me.md section 6.1, "One list"): no heading; each row's
 *  second line says what it is. */
@Composable
internal fun GroupsAndChannelsSection(found: List<FoundConversation>, port: ConversationSearchPort) {
    var opened by remember { mutableStateOf<FoundConversation?>(null) }
    if (found.isEmpty()) return
    val strings = LocalStrings.current
    for (f in found) {
        val member = port.isMember(f.idHex())
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier
                .fillMaxWidth()
                .clickable { if (member) port.open(f) else opened = f }
                .padding(horizontal = 16.dp, vertical = 10.dp),
        ) {
            LakkoAvatar(name = f.name, seed = f.idHex(), size = 40.dp, group = true, avatar = f.avatar)
            Spacer(Modifier.width(12.dp))
            Column(Modifier.weight(1f)) {
                Text(f.name, style = MaterialTheme.typography.titleMedium)
                Text(
                    "@" + f.handle + "  " + kindLine(f, strings),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            if (member) TextButton(onClick = { port.open(f) }) { Text(t("search.conversation.open")) }
        }
    }
    // The card reads membership as it draws: a join that lands while it is
    // open turns its button into Open.
    opened?.let { f -> ConversationCard(f, port, onClose = { opened = null }) }
}

private fun kindLine(f: FoundConversation, strings: me.lakko.app.l10n.Strings): String =
    if (f.kind == uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.GROUP) {
        strings.format("search.conversation.group_line", mapOf("band" to bandText(f.sizeBand)))
    } else {
        strings.format("search.conversation.channel_line", mapOf("band" to bandText(f.sizeBand)))
    }

/** Section 5.1 — the card: avatar, name, @name, the kind line, the
 *  description, and `Request to join` (a channel: `Subscribe`). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ConversationCard(f: FoundConversation, port: ConversationSearchPort, onClose: () -> Unit) {
    val scope = rememberCoroutineScope()
    val strings = LocalStrings.current
    var sending by remember { mutableStateOf(false) }
    var sent by remember {
        mutableStateOf(!me.lakko.app.chat.mayAskAgain(me.lakko.app.chat.RequestsSentStore.of(f.idHex()), System.currentTimeMillis()))
    }
    var line by remember { mutableStateOf<UiText?>(if (sent) UiText("search.conversation.request_sent") else null) }
    val isGroup = f.kind == uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.GROUP
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        BackHandler(onBack = onClose)
        Scaffold(
            topBar = {
                TopAppBar(
                    title = {},
                    navigationIcon = {
                        IconButton(onClick = onClose) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                        }
                    },
                )
            },
        ) { padding ->
            Column(
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(8.dp),
                modifier = Modifier.fillMaxSize().padding(padding).padding(16.dp),
            ) {
                LakkoAvatar(name = f.name, seed = f.idHex(), size = 96.dp, group = true, avatar = f.avatar, large = true)
                Text(f.name, style = MaterialTheme.typography.titleLarge)
                Text("@" + f.handle, style = MaterialTheme.typography.bodyMedium)
                Text(kindLine(f, strings), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
                if (f.description.isNotBlank()) Text(f.description, style = MaterialTheme.typography.bodyMedium)
                Spacer(Modifier.height(8.dp))
                when {
                    port.isMember(f.idHex()) -> Button(onClick = { port.open(f) }) { Text(t("search.conversation.open")) }
                    !isGroup -> Button(onClick = { port.subscribe(f) }) { Text(t("search.conversation.subscribe")) }
                    else -> Button(
                        enabled = !sending && !sent,
                        onClick = {
                            scope.launch {
                                sending = true
                                line = try {
                                    port.requestToJoin(f)
                                    sent = true
                                    UiText("search.conversation.request_sent")
                                } catch (cause: Throwable) {
                                    me.lakko.app.rethrowIfCancelled(cause)
                                    knockFailure(cause)
                                } finally {
                                    sending = false
                                }
                            }
                        },
                    ) {
                        Text(t(if (sent) "search.conversation.request_sent_button" else "search.conversation.request"))
                    }
                }
                line?.let { Text(it.text(), style = MaterialTheme.typography.bodySmall) }
            }
        }
    }
}

/** Section 6.1 — the copy of a failed request. */
internal fun knockFailure(cause: Throwable): UiText {
    var c: Throwable? = cause
    val seen = HashSet<Throwable>()
    while (c != null && seen.add(c)) {
        if (c is me.lakko.app.ds.DsHttpError && c.status == 429) return UiText("search.conversation.queue_full")
        if (c is me.lakko.app.anon.AnonTokenQuotaExhausted) return UiText("search.conversation.out_of_requests")
        if (c is me.lakko.app.ds.DsNetworkError) return UiText("group_handle.error.offline")
        c = c.cause
    }
    return UiText("search.people.request_failed")
}

/**
 * Section 6.2 — the `Requests sent` row at the top of Chats, with a count,
 * opening the list: avatar, name, `@name`, and "Waiting for an admin." or,
 * after 7 days, "No answer yet." with `Ask again`. No Cancel.
 */
@Composable
internal fun RequestsSentRow(
    requests: List<me.lakko.app.chat.SentRequest>,
    onAskAgain: suspend (me.lakko.app.chat.SentRequest) -> Unit,
) {
    if (requests.isEmpty()) return
    var open by remember { mutableStateOf(false) }
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable { open = true }
            .padding(horizontal = 16.dp, vertical = 12.dp),
    ) {
        Text(t("requests_sent.title"), style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
        Text(requests.size.toString(), color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
    if (open) RequestsSentScreen(requests, onAskAgain, onClose = { open = false })
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun RequestsSentScreen(
    requests: List<me.lakko.app.chat.SentRequest>,
    onAskAgain: suspend (me.lakko.app.chat.SentRequest) -> Unit,
    onClose: () -> Unit,
) {
    val scope = rememberCoroutineScope()
    var error by remember { mutableStateOf<UiText?>(null) }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        BackHandler(onBack = onClose)
        Scaffold(
            topBar = {
                TopAppBar(
                    title = { Text(t("requests_sent.title")) },
                    navigationIcon = {
                        IconButton(onClick = onClose) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                        }
                    },
                )
            },
        ) { padding ->
            Column(Modifier.fillMaxSize().padding(padding)) {
                error?.let {
                    Text(it.text(), color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(16.dp))
                }
                val now = System.currentTimeMillis()
                for (r in requests) {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp),
                    ) {
                        LakkoAvatar(name = r.name, seed = r.idHex, size = 40.dp, group = true, avatar = r.avatar)
                        Spacer(Modifier.width(12.dp))
                        Column(Modifier.weight(1f)) {
                            Text(r.name, style = MaterialTheme.typography.titleMedium)
                            Text("@" + r.handle, style = MaterialTheme.typography.bodySmall)
                            Text(
                                t(if (me.lakko.app.chat.sentRequestWaiting(r, now)) "requests_sent.waiting" else "requests_sent.no_answer"),
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        if (!me.lakko.app.chat.sentRequestWaiting(r, now)) {
                            TextButton(onClick = {
                                scope.launch {
                                    error = try {
                                        onAskAgain(r)
                                        null
                                    } catch (cause: Throwable) {
                                        me.lakko.app.rethrowIfCancelled(cause)
                                        knockFailure(cause)
                                    }
                                }
                            }) { Text(t("requests_sent.ask_again")) }
                        }
                    }
                }
            }
        }
    }
}
