// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.text.format.DateFormat
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.combinedClickable
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
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Archive
import androidx.compose.material.icons.filled.Campaign
import androidx.compose.material.icons.filled.Clear
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Groups
import androidx.compose.material.icons.filled.NotificationsOff
import androidx.compose.material.icons.filled.PersonAdd
import androidx.compose.material.icons.filled.PushPin
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Unarchive
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import me.lakko.app.calls.quietOffer
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.ConversationRowKind
import me.lakko.app.chat.Direction
import me.lakko.app.chat.FoundByName
import me.lakko.app.chat.NameLookups
import me.lakko.app.chat.conversationRowMatchesQuery
import me.lakko.app.chat.findByName
import me.lakko.app.chat.findByNameMessage
import me.lakko.app.chat.findByNameRowLabel
import me.lakko.app.chat.orderConversationRows
import me.lakko.app.chat.toRowKind
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.FlagsState
import me.lakko.app.devices.sync.ReadCursorState
import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.channels.channelRowModel
import me.lakko.app.ui.components.LakkoAvatar
import java.time.ZoneId
import java.util.Locale

/**
 * Every conversation in one list: one-to-one chats, groups and channels.
 *
 * `docs/ux/navigation.md` section 2. It used to be one screen per container,
 * rendered twice with a `kind` prop and a third time as Channels, and the
 * newest thing that happened to you was in one of them and you had to remember
 * which. The prop is GONE rather than defaulted: a list sorted by WHEN cannot
 * be filtered by kind and still be that list.
 *
 * The order is the shared rule in `crates/lakko-ui-rules`, reached through
 * [orderConversationRows] — one copy behind three clients, because three
 * hand-written translations of one paragraph drift and the drift is only found
 * when someone notices the clients disagreeing.
 *
 * Receiving is automatic (the app-level long poll in `MainActivity`), so there
 * is no Refresh button; pull-to-refresh runs the same poll once. Row actions
 * live behind a swipe (pin to the right, archive to the left) and a long-press
 * menu, never as permanent buttons in the row.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ChatListScreen(
    repository: ChatRepository,
    onOpenThread: (groupIdHex: String) -> Unit,
    /** A channel row opens the channel, not a chat — the one thing the row's
     *  kind is carried for. */
    onOpenChannel: (channelId: String) -> Unit,
    /** The three items of the create menu, `docs/ux/navigation.md` section 3. */
    onNewChat: () -> Unit,
    onCreateGroup: () -> Unit,
    onCreateChannel: () -> Unit,
    /** A looked-up channel handle: its subscribe page. */
    onSubscribeToChannel: (channelId: String, handle: String) -> Unit,
    /** A looked-up username: Contacts, which owns the request flow. */
    onFoundPerson: (username: String) -> Unit,
    modifier: Modifier = Modifier,
    channels: ChannelsUiState? = null,
    deviceGroupSync: DeviceGroupSyncRepository? = null,
    /**
     * The process-wide 1x1 call host ([me.lakko.app.calls.AppCallHost.instance]),
     * or `null`.
     *
     * `docs/ux/calls.md` section 2.1.1 — a call in a MUTED conversation makes no
     * sound and never takes the screen over, so this list is one of the two
     * places it is offered instead: the ringing chat's own row carries Accept and
     * Decline. The host is the one source of what is ringing; this screen reads
     * its slot and adds no state of its own.
     */
    callHost: me.lakko.app.calls.OneToOneCallHost<*>? = null,
    /**
     * `docs/ux/who-can-find-me.md` section 6.1 — people in the search field:
     * the people this device knows as the person types, and the one **Search
     * people** row (username, name, contacts of contacts, and the channel
     * namespace). `null` keeps the plain lookup row (tests, previews).
     */
    peopleSearch: me.lakko.app.search.PeopleSearchService? = null,
    /** A contact row of the people results: open their chat. */
    onOpenContact: (keyB64: String) -> Unit = {},
) {
    val state by repository.state.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    // ADR-0048 UX §5 — a two-member chat is listed under the other person's
    // resolved name; a group under its local name or its members' names.
    val directory = remember(repository) { ProfileDirectory.get(repository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    // ADR-0051 — a shared group name accepted on a poll re-titles the row.
    val groupNameVersion by me.lakko.app.chat.GroupNameStore.version.collectAsStateWithLifecycle()
    val flagsState = deviceGroupSync?.flags?.collectAsStateWithLifecycle()
    val flagsMap: Map<String, FlagsState> = flagsState?.value ?: emptyMap()
    val readCursorState = deviceGroupSync?.readCursors?.collectAsStateWithLifecycle()
    val readCursorMap: Map<String, ReadCursorState> = readCursorState?.value ?: emptyMap()
    val channelHandles = channels?.channels?.collectAsStateWithLifecycle()?.value ?: emptyList()
    val channelPosts = channels?.postsByChannel?.collectAsStateWithLifecycle()?.value ?: emptyMap()
    val channelKeys = channels?.keysByChannel?.collectAsStateWithLifecycle()?.value ?: emptyMap()
    var refreshing by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var createMenuOpen by remember { mutableStateOf(false) }

    // docs/ux/calls.md section 2.1.1 — the muted conversation whose call is
    // ringing right now, or `null`. Read straight from the one call slot: this
    // screen keeps no call state, starts no timer and counts nothing down. The
    // offer disappears on its own when the window runs out, because the host
    // ends the call it belongs to and clears the slot.
    // Both fallbacks are hoisted rather than written inline behind the `?:`, so
    // the `remember`s are reached on every composition: a slot that only exists
    // while nothing is ringing changes the composition's shape the moment a call
    // arrives.
    val noCall = remember { MutableStateFlow<me.lakko.app.calls.OneToOneCall<*>?>(null) }
    val noCallState = remember {
        MutableStateFlow(me.lakko.app.calls.CallSession.WebCallState.Done)
    }
    val ringingCall: me.lakko.app.calls.OneToOneCall<*>? by (callHost?.active ?: noCall)
        .collectAsStateWithLifecycle()
    val ringingCallState by (ringingCall?.session?.state ?: noCallState)
        .collectAsStateWithLifecycle()
    val quietCallOffer = remember(ringingCall, ringingCallState) {
        ringingCall?.quietOffer(ringingCallState)
    }

    LaunchedEffect(Unit) {
        // One immediate poll so the list is fresh on arrival; the continuous
        // long poll across every group runs app-wide from `MainActivity`.
        repository.pollAllGroups()
    }

    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val use24h = remember(context) { DateFormat.is24HourFormat(context) }

    // Chats and groups. They were already one store split by a filter, so they
    // merge by dropping it.
    val threadRows = remember(state, nameVersion, groupNameVersion, flagsMap, readCursorMap) {
        threadConversationRows(
            context = context,
            repository = repository,
            directory = directory,
            threads = state.threads,
            flagsMap = flagsMap,
            readCursorMap = readCursorMap,
            nowMs = System.currentTimeMillis(),
            zone = zone,
            use24h = use24h,
            locale = locale,
        )
    }

    // Channels are their own store, so they have to be mapped in. Their last
    // POST time becomes the same field as a chat's last MESSAGE time, or the
    // one ordering has nothing to order by.
    val channelRows = remember(channelHandles, channelPosts, channelKeys) {
        val now = System.currentTimeMillis()
        channelHandles.map { channel ->
            val model = channelRowModel(
                channel = channel,
                posts = channelPosts[channel.id],
                subscriberCount = null,
                isOwner = channelKeys.containsKey(channel.id),
                nowMs = now,
                zone = zone,
                use24h = use24h,
                locale = locale,
            )
            ConversationListRow(
                id = channel.id,
                kind = ConversationRowKind.CHANNEL,
                title = model.title,
                preview = model.subtitle,
                time = model.time,
                lastActivityMs = model.sortMs,
                // Unread, pinned, muted and archived are keyed on a
                // conversation id a channel does not carry, so a channel row
                // shows none of them (section 6.1).
            )
        }
    }

    val rows = remember(threadRows, channelRows) {
        orderConversationRows(threadRows + channelRows)
    }
    val visible = remember(rows, query) {
        rows.filter { conversationRowMatchesQuery(query, it.title, it.preview) }
    }
    val synchronizing = remember(state) { repository.isSynchronizing() }

    // `docs/ux/navigation.md` section 4.3 — ONE network lookup row, offered
    // when nothing local matched and the text could be a name. It asks both
    // namespaces because the person typing does not know which one holds the
    // name they were given.
    var lookupBusy by remember { mutableStateOf(false) }
    var lookupNote by remember { mutableStateOf<String?>(null) }
    val lookupOffered = remember(visible, query) {
        visible.isEmpty() && query.isNotBlank() && looksLikeANameSafely(query)
    }

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text("Chats") },
                actions = {
                    Box {
                        IconButton(onClick = { createMenuOpen = true }) {
                            Icon(Icons.Filled.Edit, contentDescription = "New")
                        }
                        CreateMenu(
                            expanded = createMenuOpen,
                            onDismiss = { createMenuOpen = false },
                            onNewChat = onNewChat,
                            onCreateGroup = onCreateGroup,
                            onCreateChannel = onCreateChannel,
                        )
                    }
                },
            )
        },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = refreshing,
            onRefresh = {
                scope.launch {
                    refreshing = true
                    try {
                        repository.pollAllGroups()
                    } catch (_: Throwable) {
                        // The next automatic pass reports through the row state.
                    } finally {
                        refreshing = false
                    }
                }
            },
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            Column(modifier = Modifier.fillMaxSize()) {
                SearchField(
                    query = query,
                    onQueryChange = {
                        query = it
                        lookupNote = null
                    },
                )
                // ADR-0050 / docs/ux/second-device.md section 2: one label while
                // this device is still being admitted to the account's chats.
                if (synchronizing) {
                    Text(
                        "Synchronizing...",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                when {
                    // Cold start: the MLS restore that populates the list runs
                    // off the main thread after the first frame, so an empty
                    // list before it finishes is "loading", not "no chats".
                    rows.isEmpty() && !state.bootstrapped -> Box(
                        modifier = Modifier.fillMaxSize(),
                        contentAlignment = Alignment.Center,
                    ) {
                        CircularProgressIndicator()
                    }
                    // The empty state's button opens the SAME menu the compose
                    // button does, not a different set of choices in a
                    // different place.
                    rows.isEmpty() -> EmptyPanel(onStart = { createMenuOpen = true })
                    else -> Column(modifier = Modifier.fillMaxSize()) {
                        if (visible.isEmpty() && peopleSearch != null && query.isNotBlank()) {
                            Column(
                                modifier = Modifier
                                    .fillMaxSize()
                                    .verticalScroll(rememberScrollState()),
                            ) {
                                PeopleSearchSection(
                                    query = query,
                                    service = peopleSearch,
                                    conversationMatches = 0,
                                    onOpenChannel = { id, handle ->
                                        query = ""
                                        onSubscribeToChannel(id, handle)
                                    },
                                    onOpenContact = { key ->
                                        query = ""
                                        onOpenContact(key)
                                    },
                                )
                            }
                        } else if (visible.isEmpty()) {
                            NoMatchPanel(
                                offerLookup = lookupOffered,
                                label = findByNameRowLabelSafely(query),
                                busy = lookupBusy,
                                note = lookupNote,
                                onLookUp = {
                                    if (!lookupBusy) {
                                        lookupBusy = true
                                        lookupNote = null
                                        val typed = query
                                        scope.launch {
                                            val found = lookUpName(typed)
                                            lookupBusy = false
                                            when (found) {
                                                is FoundByName.Channel -> {
                                                    query = ""
                                                    onSubscribeToChannel(
                                                        found.channelIdHex,
                                                        found.handle,
                                                    )
                                                }
                                                is FoundByName.Person -> {
                                                    query = ""
                                                    onFoundPerson(found.username)
                                                }
                                                else -> lookupNote =
                                                    findByNameMessageSafely(found, typed)
                                            }
                                        }
                                    }
                                },
                            )
                        } else {
                            LazyColumn(
                                modifier = Modifier.fillMaxSize(),
                                contentPadding = PaddingValues(vertical = 4.dp),
                            ) {
                                items(items = visible, key = { "${it.kind}:${it.id}" }) { row ->
                                    if (row.kind == ConversationRowKind.CHANNEL) {
                                        // Same avatar, same title, same second
                                        // line as every other row. What it does
                                        // NOT have is the badge, the glyphs and
                                        // the row menu — all keyed on a
                                        // conversation id a channel does not
                                        // carry.
                                        ConversationRowBody(
                                            row = row,
                                            modifier = Modifier.clickable {
                                                onOpenChannel(row.id)
                                            },
                                        )
                                    } else {
                                        val toggle: (FlagField) -> Unit = { field ->
                                            toggleFlag(
                                                sync = deviceGroupSync,
                                                groupIdHex = row.id,
                                                current = flagsMap[row.id],
                                                field = field,
                                                scope = scope,
                                            )
                                        }
                                        SwipeableThreadRow(
                                            row = row,
                                            onClick = { onOpenThread(row.id) },
                                            onToggleFlag = toggle,
                                            // Section 2.1.1 — the row carries
                                            // Accept and Decline for exactly
                                            // the conversation the muted call
                                            // is ringing in, and for no other.
                                            quietCallOffer = quietCallOffer?.takeIf { it.isFor(row.id) },
                                            // The same two entry points the
                                            // full-screen ring screen drives.
                                            onAcceptCall = { withVideo ->
                                                callHost?.accept(withVideo = withVideo)
                                            },
                                            onDeclineCall = { callHost?.decline() },
                                        )
                                    }
                                }
                                // UX who-can-find-me 6.1 — people under the
                                // matching conversations.
                                if (peopleSearch != null && query.isNotBlank()) {
                                    item(key = "people-search") {
                                        PeopleSearchSection(
                                            query = query,
                                            service = peopleSearch,
                                            conversationMatches = visible.size,
                                            onOpenChannel = { id, handle ->
                                                query = ""
                                                onSubscribeToChannel(id, handle)
                                            },
                                            onOpenContact = { key ->
                                                query = ""
                                                onOpenContact(key)
                                            },
                                        )
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/**
 * The three ways to start something, `docs/ux/navigation.md` section 3.
 *
 * A row with a leading icon and a label and nothing else: an item that needs a
 * sentence to explain it has the wrong label. Reached from the compose button
 * and from the empty state, so a person never meets two different sets of
 * choices for the same intent.
 */
@Composable
private fun CreateMenu(
    expanded: Boolean,
    onDismiss: () -> Unit,
    onNewChat: () -> Unit,
    onCreateGroup: () -> Unit,
    onCreateChannel: () -> Unit,
) {
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        DropdownMenuItem(
            text = { Text("New chat") },
            leadingIcon = { Icon(Icons.Filled.PersonAdd, contentDescription = null) },
            onClick = { onDismiss(); onNewChat() },
        )
        DropdownMenuItem(
            text = { Text("Create group") },
            leadingIcon = { Icon(Icons.Filled.Groups, contentDescription = null) },
            onClick = { onDismiss(); onCreateGroup() },
        )
        DropdownMenuItem(
            text = { Text("Create channel") },
            leadingIcon = { Icon(Icons.Filled.Campaign, contentDescription = null) },
            onClick = { onDismiss(); onCreateChannel() },
        )
    }
}

internal enum class FlagField { Pin, Mute, Archive }

private fun toggleFlag(
    sync: DeviceGroupSyncRepository?,
    groupIdHex: String,
    current: FlagsState?,
    field: FlagField,
    scope: kotlinx.coroutines.CoroutineScope,
) {
    val groupId = hexToBytes(groupIdHex)
    if (sync == null || groupId == null) return
    var pinned = current?.pinned ?: false
    var muted = current?.muted ?: false
    var archived = current?.archived ?: false
    val unreadOverride = current?.unreadOverride ?: false
    when (field) {
        FlagField.Pin -> pinned = !pinned
        FlagField.Mute -> muted = !muted
        FlagField.Archive -> archived = !archived
    }
    scope.launch {
        try {
            sync.setFlags(
                groupId = groupId,
                pinned = pinned,
                muted = muted,
                archived = archived,
                unreadOverride = unreadOverride,
            )
        } catch (cause: Throwable) {
            android.util.Log.w("ChatListScreen", "setFlags failed", cause)
        }
    }
}

/**
 * Ask both discovery namespaces for one typed name.
 *
 * The client is built and closed per lookup because a lookup is a rare,
 * user-initiated act, not a hot path. The decision itself is
 * [me.lakko.app.chat.findByName], which is pure and tested; this only supplies
 * the two network calls it takes as an argument.
 */
private suspend fun lookUpName(typed: String): FoundByName = try {
    DiscoveryClient().use { client ->
        findByName(
            typed = typed,
            lookups = object : NameLookups {
                override suspend fun channel(handle: String): String? =
                    client.lookupChannel(handle)

                override suspend fun person(username: String): String? {
                    val found = client.lookup(username)
                    return if (found.account == null) null else found.normalisedUsername
                }
            },
        )
    }
} catch (cancelled: kotlinx.coroutines.CancellationException) {
    throw cancelled
} catch (cause: Throwable) {
    android.util.Log.w("ChatListScreen", "the name lookup could not be started", cause)
    FoundByName.Unreachable
}

/**
 * The shared name gate, degrading to "not a name" when the native library is
 * not there.
 *
 * Failing shut is the honest direction: the gate exists so a name the blinder
 * would refuse never costs a lookup token, and offering a lookup we cannot
 * even check the shape of would spend one to be told no.
 */
private fun looksLikeANameSafely(typed: String): Boolean = try {
    uniffi.lakko_ffi_uniffi.looksLikeAName(typed)
} catch (cause: Throwable) {
    android.util.Log.w("ChatListScreen", "the name rule could not be read", cause)
    false
}

private fun findByNameRowLabelSafely(typed: String): String = try {
    findByNameRowLabel(typed)
} catch (_: Throwable) {
    "Look up this name"
}

private fun findByNameMessageSafely(found: FoundByName, typed: String): String? = try {
    findByNameMessage(found, typed)
} catch (_: Throwable) {
    "Could not look that up. Check your connection and try again."
}

/**
 * The chat-and-group rows of the merged list, one per thread, as Compose draws
 * them. Shared with the "Forward to" picker (ADR-0052 decision 2 / UX 5.1),
 * which lists "the same merged list the Chats tab shows" and must not build a
 * second one.
 */
internal fun threadConversationRows(
    context: android.content.Context,
    repository: ChatRepository,
    directory: ProfileDirectory,
    threads: List<ChatThread>,
    flagsMap: Map<String, FlagsState>,
    readCursorMap: Map<String, ReadCursorState>,
    nowMs: Long,
    zone: ZoneId,
    use24h: Boolean,
    locale: Locale,
): List<ConversationListRow> = threads.map { thread ->
    val kind = conversationKind(context, repository, thread.groupIdHex)
    // A call row IS the conversation's last event and belongs in the
    // preview; the receive loop's bookkeeping rows are not and never
    // did. Found live: a group whose only rows were commits showed the
    // literal "[mls commit]" as its preview, which is a protocol name
    // in a person's face and the one thing the copy rules forbid
    // outright. A call row is a system row carrying its parts, so the
    // two are told apart by what the row HOLDS, not by its text.
    val last = thread.messages.lastOrNull { it.direction != Direction.System || it.call != null }
        ?: thread.messages.lastOrNull { it.call != null }
    val lastMs = last?.let { parseMessageTimeMs(it.createdAt) }
    val senderName = last?.takeIf { it.direction == Direction.Incoming }
        ?.authorKey?.let { directory.resolve(it).primary }
    val flags = flagsMap[thread.groupIdHex]
    ConversationListRow(
        id = thread.groupIdHex,
        kind = kind.toRowKind(),
        title = conversationTitle(context, repository, directory, thread.groupIdHex),
        preview = rowPreview(
            thread,
            last,
            isGroup = kind == ConversationKind.Group,
            senderName = senderName,
        ),
        time = lastMs?.let { formatListTime(it, nowMs, zone, use24h, locale) },
        // A conversation nothing has happened in yet has no activity
        // at all, and the shared rule puts "never" at the bottom and
        // breaks the tie on the title — so an unrelated refresh never
        // reshuffles the empty ones.
        lastActivityMs = lastMs,
        // ADR-0045 UX §6.2.4 / §6.2.11 — no badge once the membership
        // has ended; the count would be frozen for good.
        unread = if (thread.membershipEnded) {
            0
        } else {
            unreadCount(thread = thread, cursor = readCursorMap[thread.groupIdHex])
        },
        pinned = flags?.pinned == true,
        muted = flags?.muted == true,
        archived = flags?.archived == true,
    )
}

/**
 * The second line of a row: the membership-ended line when this device's
 * membership is over (ADR-0045 UX §6.2.4), else the last message's preview
 * with its sender prefix, else "No messages yet".
 */
internal fun rowPreview(
    thread: ChatThread,
    last: ChatMessage?,
    isGroup: Boolean,
    senderName: String?,
): String {
    val endedLine = me.lakko.app.chat.chatListLine(
        me.lakko.app.chat.membershipState(
            isActive = !thread.membershipEnded,
            leaveRequestedAtMs = null,
        ),
    )
    if (endedLine != null) return endedLine
    if (last == null) return "No messages yet"
    if (last.direction == Direction.System) {
        // Only a row this build can render: an unreadable bookkeeping row is
        // never shown raw.
        return last.call?.let { me.lakko.app.calls.callRowText(it) } ?: "No messages yet"
    }
    val line = previewLine(
        text = last.text,
        attachments = last.attachments.map { PreviewAttachment(it.kind, it.mime) },
        deleted = last.deleted,
        prefix = previewPrefix(last.direction, isGroup, senderName),
    )
    return line.ifEmpty { "No messages yet" }
}

/** Compute unread count for a thread.
 *
 *  Two sources, folded by [me.lakko.app.chat.combinedUnread]:
 *   - the cross-device read cursor (Phase 2a3e2c-followup): every incoming /
 *     system row whose `messageId` is strictly greater than the last broadcast
 *     cursor. Outgoing rows never count — the sender already "read" them by
 *     composing. `null` when the chat has no cursor.
 *   - the local count of incoming messages this device's polls applied while
 *     the chat was not on screen ([ChatThread.localUnread]). Without it a chat
 *     with no cursor never showed a badge, whatever arrived. */
internal fun unreadCount(thread: ChatThread, cursor: ReadCursorState?): Int {
    val cursorCount = cursor?.let { c ->
        val cursorId = decodeBeI64(c.upToMessageId)
        thread.messages.count { m -> m.direction != Direction.Outgoing && m.messageId > cursorId }
    }
    return me.lakko.app.chat.combinedUnread(
        cursorCount = cursorCount,
        local = me.lakko.app.chat.LocalUnread(
            count = thread.localUnread,
            viewedThisSession = thread.unreadViewedThisSession,
        ),
    )
}

private fun decodeBeI64(bytes: ByteArray): Long {
    if (bytes.size < 8) return Long.MAX_VALUE
    var v: Long = 0L
    for (i in 0 until 8) {
        v = (v shl 8) or (bytes[i].toLong() and 0xff)
    }
    return v
}

private fun hexToBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    var i = 0
    while (i < hex.length) {
        val hi = Character.digit(hex[i], 16)
        val lo = Character.digit(hex[i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i / 2] = ((hi shl 4) or lo).toByte()
        i += 2
    }
    return out
}

/**
 * The search field under the app bar.
 *
 * "Search" rather than "Search chats": the list holds every kind of
 * conversation now, and naming one of them would say the other two are not
 * here.
 */
@Composable
internal fun SearchField(query: String, onQueryChange: (String) -> Unit) {
    OutlinedTextField(
        value = query,
        onValueChange = onQueryChange,
        placeholder = { Text("Search") },
        singleLine = true,
        leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
        trailingIcon = if (query.isNotEmpty()) {
            {
                IconButton(onClick = { onQueryChange("") }) {
                    Icon(Icons.Filled.Clear, contentDescription = "Clear search")
                }
            }
        } else {
            null
        },
        shape = RoundedCornerShape(12.dp),
        colors = OutlinedTextFieldDefaults.colors(
            focusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
            unfocusedContainerColor = MaterialTheme.colorScheme.surfaceVariant,
            focusedBorderColor = Color.Transparent,
            unfocusedBorderColor = Color.Transparent,
        ),
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
    )
}

/**
 * A row with its swipe actions: right (start to end) pins or unpins, left (end
 * to start) archives or unarchives. The row always snaps back — the swipe is
 * a command, not a dismissal — so `confirmValueChange` refuses every settle.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeableThreadRow(
    row: ConversationListRow,
    onClick: () -> Unit,
    onToggleFlag: (FlagField) -> Unit,
    quietCallOffer: me.lakko.app.calls.QuietCallOffer? = null,
    onAcceptCall: (withVideo: Boolean) -> Unit = {},
    onDeclineCall: () -> Unit = {},
) {
    val dismissState = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            when (value) {
                SwipeToDismissBoxValue.StartToEnd -> onToggleFlag(FlagField.Pin)
                SwipeToDismissBoxValue.EndToStart -> onToggleFlag(FlagField.Archive)
                SwipeToDismissBoxValue.Settled -> Unit
            }
            false
        },
        positionalThreshold = { total -> total * 0.35f },
    )
    SwipeToDismissBox(
        state = dismissState,
        backgroundContent = {
            val towardsEnd = dismissState.dismissDirection == SwipeToDismissBoxValue.StartToEnd
            val icon = if (towardsEnd) {
                Icons.Filled.PushPin
            } else if (row.archived) {
                Icons.Filled.Unarchive
            } else {
                Icons.Filled.Archive
            }
            val label = if (towardsEnd) {
                if (row.pinned) "Unpin" else "Pin"
            } else {
                if (row.archived) "Unarchive" else "Archive"
            }
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .background(MaterialTheme.colorScheme.primaryContainer)
                    .padding(horizontal = 24.dp),
                contentAlignment = if (towardsEnd) Alignment.CenterStart else Alignment.CenterEnd,
            ) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        icon,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                    Spacer(Modifier.width(8.dp))
                    Text(
                        label,
                        style = MaterialTheme.typography.labelLarge,
                        color = MaterialTheme.colorScheme.onPrimaryContainer,
                    )
                }
            }
        },
    ) {
        Box(modifier = Modifier.background(MaterialTheme.colorScheme.background)) {
            ThreadRow(
                row = row,
                onClick = onClick,
                onToggleFlag = onToggleFlag,
                quietCallOffer = quietCallOffer,
                onAcceptCall = onAcceptCall,
                onDeclineCall = onDeclineCall,
            )
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ThreadRow(
    row: ConversationListRow,
    onClick: () -> Unit,
    onToggleFlag: (FlagField) -> Unit,
    quietCallOffer: me.lakko.app.calls.QuietCallOffer? = null,
    onAcceptCall: (withVideo: Boolean) -> Unit = {},
    onDeclineCall: () -> Unit = {},
) {
    var menuOpen by remember { mutableStateOf(false) }
    Box {
        Column {
            ConversationRowBody(
                row = row,
                modifier = Modifier.combinedClickable(
                    onClick = onClick,
                    onLongClick = { menuOpen = true },
                ),
            )
            // docs/ux/calls.md section 2.1.1 — the muted conversation's call is
            // answerable from the row, which is where the person is already
            // looking and needs no navigation. Inside the row's own column, so
            // the rows below move down rather than being covered.
            quietCallOffer?.let { offer ->
                me.lakko.app.ui.call.QuietIncomingCallRowActions(
                    offer = offer,
                    // The row's own title: the conversation is named once.
                    callerName = row.title,
                    onAccept = onAcceptCall,
                    onDecline = onDeclineCall,
                )
            }
        }
        DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
            DropdownMenuItem(
                text = { Text(if (row.pinned) "Unpin" else "Pin") },
                leadingIcon = { Icon(Icons.Filled.PushPin, contentDescription = null) },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Pin) },
            )
            DropdownMenuItem(
                text = { Text(if (row.muted) "Unmute" else "Mute") },
                leadingIcon = { Icon(Icons.Filled.NotificationsOff, contentDescription = null) },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Mute) },
            )
            DropdownMenuItem(
                text = { Text(if (row.archived) "Unarchive" else "Archive") },
                leadingIcon = {
                    Icon(
                        if (row.archived) Icons.Filled.Unarchive else Icons.Filled.Archive,
                        contentDescription = null,
                    )
                },
                onClick = { menuOpen = false; onToggleFlag(FlagField.Archive) },
            )
        }
    }
}

/**
 * The one row shape, drawn identically for all three kinds
 * (`docs/ux/navigation.md` section 2.1): same avatar treatment, same title,
 * same second line, same trailing column. Nothing here says which kind a row
 * is — a channel simply has no flags and no badge to draw.
 */
@Composable
internal fun ConversationRowBody(row: ConversationListRow, modifier: Modifier = Modifier) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = modifier
            .fillMaxWidth()
            .alpha(if (row.archived) 0.6f else 1f)
            .padding(horizontal = 16.dp, vertical = 10.dp),
    ) {
        LakkoAvatar(
            name = row.title,
            seed = row.id,
            size = 40.dp,
            group = row.kind != ConversationRowKind.CHAT,
        )
        Spacer(Modifier.width(12.dp))
        Column(modifier = Modifier.weight(1f)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = row.title,
                    style = MaterialTheme.typography.titleMedium,
                    fontWeight = if (row.unread > 0) FontWeight.Bold else FontWeight.Medium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f, fill = false),
                )
                if (row.time != null) {
                    Spacer(Modifier.width(8.dp))
                    Text(
                        text = row.time,
                        style = MaterialTheme.typography.labelSmall,
                        color = if (row.unread > 0) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                        maxLines = 1,
                    )
                }
            }
            Spacer(Modifier.height(2.dp))
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = row.preview,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                if (row.muted) {
                    Spacer(Modifier.width(6.dp))
                    Icon(
                        Icons.Filled.NotificationsOff,
                        contentDescription = "Muted",
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(16.dp),
                    )
                }
                if (row.pinned) {
                    Spacer(Modifier.width(6.dp))
                    Icon(
                        Icons.Filled.PushPin,
                        contentDescription = "Pinned",
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(16.dp),
                    )
                }
                if (row.unread > 0) {
                    Spacer(Modifier.width(6.dp))
                    UnreadBadge(count = row.unread, muted = row.muted)
                }
            }
        }
    }
}

@Composable
private fun UnreadBadge(count: Int, muted: Boolean) {
    val label = if (count > 99) "99+" else count.toString()
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .clip(RoundedCornerShape(percent = 50))
            .background(
                if (muted) {
                    MaterialTheme.colorScheme.surfaceVariant
                } else {
                    MaterialTheme.colorScheme.primary
                },
            )
            .padding(horizontal = 7.dp, vertical = 2.dp),
    ) {
        Text(
            text = label,
            style = MaterialTheme.typography.labelSmall,
            color = if (muted) {
                MaterialTheme.colorScheme.onSurfaceVariant
            } else {
                MaterialTheme.colorScheme.onPrimary
            },
        )
    }
}

@Composable
private fun EmptyPanel(onStart: () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = "No conversations yet",
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(12.dp))
            Button(onClick = onStart) { Text("Start one") }
        }
    }
}

/**
 * Nothing on this device matched, plus the ONE network lookup row when the text
 * could be a name.
 *
 * One row for the whole name, never a list of partial matches: discovery
 * evaluates a blinded name and answers only "does this exact name exist", so a
 * prefix search would be the server learning the prefix.
 */
@Composable
private fun NoMatchPanel(
    offerLookup: Boolean,
    label: String,
    busy: Boolean,
    note: String?,
    onLookUp: () -> Unit,
) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                text = "No results",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            if (offerLookup) {
                Button(onClick = onLookUp, enabled = !busy) {
                    Text(if (busy) "Looking..." else label)
                }
            }
            if (note != null) {
                Text(
                    text = note,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}
