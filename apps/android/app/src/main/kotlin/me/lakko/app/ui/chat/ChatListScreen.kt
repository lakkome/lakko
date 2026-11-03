// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.material.icons.filled.Bookmark
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
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Archive
import androidx.compose.material.icons.filled.Campaign
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.CreateNewFolder
import androidx.compose.material.icons.filled.Folder
import androidx.compose.material.icons.filled.FolderOff
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
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
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
import me.lakko.app.chat.ChatFolder
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.FolderActions
import me.lakko.app.chat.FolderLimits
import me.lakko.app.chat.folderRows
import me.lakko.app.chat.membershipToggle
import me.lakko.app.chat.membershipToggleBlocked
import me.lakko.app.chat.rowsMatching
import me.lakko.app.chat.searchFoundNoRow
import me.lakko.app.chat.selectedTabIndex
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.ChatThread
import me.lakko.app.chat.ConversationListRow
import me.lakko.app.chat.ConversationRowKind
import me.lakko.app.chat.Direction
import me.lakko.app.chat.conversationRowMatchesQuery
import me.lakko.app.chat.orderConversationRows
import me.lakko.app.chat.showsEmptyConversationState
import me.lakko.app.chat.toRowKind
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.devices.sync.FlagsState
import me.lakko.app.devices.sync.ReadCursorState
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.channels.channelRowModel
import me.lakko.app.ui.components.LakkoAvatar
import uniffi.lakko_ffi_uniffi.FolderView
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
    /** `docs/ux/chat-folders.md` 2.2 — the folder editor for one folder. */
    onEditFolder: (folderIdHex: String) -> Unit = {},
    /** UX spec 5 — "New folder..." from a row: the editor with that chat
     *  already added (`null` for an empty editor). */
    onNewFolder: (firstRow: ConversationListRow?) -> Unit = {},
    /** UX spec 2.2 — Settings > Chat folders ("Edit folders", "Reorder folders"). */
    onOpenFolderSettings: () -> Unit = {},
    /** docs/ux/saved-messages-and-file-lifetime.md section 3 — Saved, from
     *  this screen's menu. `null` hides the entry (tests, previews). */
    onOpenSaved: (() -> Unit)? = null,
    /**
     * `docs/ux/who-can-find-me.md` section 6.1 — people in the search field:
     * the people this device knows as the person types, and what the network
     * finds on its own from 3 characters (username, name, contacts of
     * contacts, and the channel namespace). `null` keeps the plain lookup row
     * (tests, previews).
     */
    peopleSearch: me.lakko.app.search.PeopleSearchService? = null,
    /** A contact row of the people results: open their chat. */
    onOpenContact: (keyB64: String) -> Unit = {},
    /** ADR-0079 section 4 — the people added through their invite link whose
     *  app has not answered yet, listed in All chats at once. */
    invited: List<me.lakko.app.contacts.PendingContact> = emptyList(),
    onOpenPendingChat: (accountIdHex: String) -> Unit = {},
    /** `docs/ux/identity-recovery.md` 7.2 — what the list says after a restore. */
    restoredChats: me.lakko.app.backup.RestoredChatsHint = me.lakko.app.backup.RestoredChatsHint.NONE,
    onOpenContacts: () -> Unit = {},
) {
    val state by repository.state.collectAsStateWithLifecycle()
    // ADR-0087 section 5 — public groups and channels in search.
    val conversationSearch = remember(repository) {
        object : ConversationSearchPort {
            override suspend fun search(query: String) = repository.searchPublicConversations(query)
            override suspend fun requestToJoin(found: me.lakko.app.chat.FoundConversation) =
                repository.requestToJoinPublicGroup(found)
            override fun isMember(idHex: String): Boolean = repository.holdsGroup(idHex)
            override fun open(found: me.lakko.app.chat.FoundConversation) {
                val hex = found.id.joinToString("") { "%02x".format(it) }
                if (found.kind == uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.GROUP) {
                    onOpenThread(hex)
                } else {
                    onSubscribeToChannel(hex, found.handle)
                }
            }
        }
    }
    val scope = rememberCoroutineScope()
    val context = LocalContext.current
    val knownChannels = channels?.channels?.collectAsStateWithLifecycle()?.value.orEmpty()
    val knownChannelName: (String) -> String? = { id ->
        knownChannels.firstOrNull { it.id.equals(id, ignoreCase = true) }
            ?.let { it.sharedName.ifEmpty { it.localName } }
    }
    val flagsState = deviceGroupSync?.flags?.collectAsStateWithLifecycle()
    val flagsMap: Map<String, FlagsState> = flagsState?.value ?: emptyMap()
    var refreshing by remember { mutableStateOf(false) }
    var query by rememberSaveable { mutableStateOf("") }
    var createMenuOpen by remember { mutableStateOf(false) }
    // docs/ux/who-can-find-me.md section 6.1 — the field asks the network on
    // its own (3 characters, 500 ms after the last keystroke).
    val peopleSession = remember(peopleSearch, scope) { peopleSearch?.session(scope) }
    LaunchedEffect(query, peopleSession) { peopleSession?.onQuery(query) }

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

    // Every conversation, unordered: the one source for All chats, every
    // folder, the folder editor and the picker.
    val allRows = rememberConversationRows(repository, channels, deviceGroupSync)

    val rows = remember(allRows) { orderConversationRows(allRows) }
    val visible = remember(rows, query) {
        rows.filter { conversationRowMatchesQuery(query, it.title, it.preview) }
    }
    // The people whose one-to-one is among the matching conversations.
    val visibleChatPeers = remember(visible) {
        visible.mapNotNullTo(HashSet()) { row ->
            row.peerKeyB64.takeIf { row.kind == ConversationRowKind.CHAT }
        }
    }
    // The pending contacts the text matches: rows of the list, so the people
    // half leaves them out (`docs/ux/who-can-find-me.md` section 6.1).
    val visibleInvited = remember(invited, query) { me.lakko.app.contacts.PendingContacts.matching(invited, query) }
    val visibleInvitedKeys = remember(visibleInvited) { visibleInvited.mapTo(HashSet()) { it.signaturePubkeyB64 } }
    val foundNoRow = searchFoundNoRow(visible.size, visibleInvited.size)
    val synchronizing = remember(state) { repository.isSynchronizing() }

    // docs/ux/chat-folders.md — the person's folders, in their order.
    val noFolders = remember { MutableStateFlow<List<FolderView>>(emptyList()) }
    val folderViews by (deviceGroupSync?.folders ?: noFolders).collectAsStateWithLifecycle()
    val folders = remember(folderViews) { folderViews.map(ChatFolder::of) }
    val actions = remember(deviceGroupSync, repository) {
        deviceGroupSync?.let { FolderActions(it, repository) }
    }
    val searching = query.isNotBlank()
    // docs/ux/navigation.md section 4 / chat-folders.md — no folder bar over an
    // empty list (the web client never showed one), and none while searching.
    val showBar = folders.isNotEmpty() && !searching && rows.isNotEmpty()

    // Section 2.5 — the tab last selected on THIS device, not synced; a folder
    // deleted since (on any device) opens All chats.
    var selectedFolderId by rememberSaveable { mutableStateOf(FolderTabPrefs.load(context)) }
    val pagerState = rememberPagerState(
        initialPage = selectedTabIndex(selectedFolderId, folders),
    ) { 1 + folders.size }
    LaunchedEffect(folders) {
        val target = selectedTabIndex(selectedFolderId, folders)
        if (target == 0 && selectedFolderId != null) {
            selectedFolderId = null
            FolderTabPrefs.save(context, null)
        }
        if (pagerState.currentPage != target) pagerState.scrollToPage(target)
    }
    LaunchedEffect(pagerState, folders) {
        snapshotFlow { pagerState.settledPage }.collect { page ->
            val id = folders.getOrNull(page - 1)?.idHex
            if (id != selectedFolderId) {
                selectedFolderId = id
                FolderTabPrefs.save(context, id)
            }
        }
    }

    var deletingFolder by remember { mutableStateOf<ChatFolder?>(null) }

    // One set of row handlers for every tab; [folder] is the tab the row sits
    // in (`null` = All chats), which is the pin set the row menu's Pin acts on.
    val rowHandlers = RowHandlers(
        folders = folders,
        foldersEnabled = actions != null,
        inFolder = { folder, row -> rowsMatching(folder.rule, listOf(row)).isNotEmpty() },
        onToggleFlag = { row, field ->
            toggleFlag(
                sync = deviceGroupSync,
                groupIdHex = row.id,
                current = flagsMap[row.id],
                field = field,
                scope = scope,
            )
        },
        onToggleFolderPin = { folder, row ->
            scope.launch { actions?.togglePin(folder, row) }
        },
        onToggleMembership = { folder, row, inFolder ->
            scope.launch { actions?.toggleMembership(folder, row, inFolder) }
        },
        onNewFolderWith = { row -> onNewFolder(row) },
        onOpenThread = onOpenThread,
        onOpenChannel = onOpenChannel,
        quietCallOffer = quietCallOffer,
        onAcceptCall = { withVideo -> callHost?.accept(withVideo = withVideo) },
        onDeclineCall = { callHost?.decline() },
    )

    Scaffold(
        modifier = modifier,
        topBar = {
            TopAppBar(
                title = { Text(t("nav.chats")) },
                actions = {
                    Box {
                        IconButton(onClick = { createMenuOpen = true }) {
                            Icon(Icons.Filled.Edit, contentDescription = t("chats.compose"))
                        }
                        CreateMenu(
                            expanded = createMenuOpen,
                            onDismiss = { createMenuOpen = false },
                            onNewChat = onNewChat,
                            onCreateGroup = onCreateGroup,
                            onCreateChannel = onCreateChannel,
                            onOpenSaved = onOpenSaved,
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
                    onQueryChange = { query = it },
                )
                // ADR-0087 UX 9.1 — "You are no longer in {name}.", once per
                // device, one at a time. It waits here across a restart.
                state.endNotices.firstOrNull()?.let { notice ->
                    MembershipEndBanner(
                        notice = notice,
                        onClose = { repository.dismissEndNotice(notice.idHex) },
                    )
                }
                // ADR-0050 / docs/ux/second-device.md section 2: one label while
                // this device is still being admitted to the account's chats.
                if (synchronizing) {
                    Text(
                        t("chats.synchronizing"),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                if (restoredChats == me.lakko.app.backup.RestoredChatsHint.BANNER) {
                    Text(
                        t("chats.restored.banner_other_devices"),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                // group-and-channel-privacy section 6.2 — `Requests sent`,
                // above the list while one is open.
                val requestsSentFlow = remember(repository) { repository.requestsSent() }
                val requestsSent by requestsSentFlow.collectAsStateWithLifecycle()
                val openRequests = remember(requestsSent, state) {
                    requestsSent.filter { !repository.holdsGroup(it.idHex) }
                }
                if (!searching) {
                    RequestsSentRow(requests = openRequests, onAskAgain = { repository.askAgain(it) })
                }
                // docs/ux/chat-folders.md 2.1 — the bar exists once one folder
                // does; a search hides it and searches everything (2.4).
                if (showBar) {
                    FolderBar(
                        folders = folders,
                        rows = allRows,
                        selectedIndex = pagerState.currentPage,
                        onSelect = { index -> scope.launch { pagerState.animateScrollToPage(index) } },
                        onEditFolder = { onEditFolder(it.idHex) },
                        onMarkAllRead = { tabRows -> scope.launch { actions?.markAllRead(tabRows) } },
                        onReorder = onOpenFolderSettings,
                        onDelete = { deletingFolder = it },
                        onEditFolders = onOpenFolderSettings,
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
                    // different place. Only while nothing is typed: with an
                    // empty list a search still reaches the people half below
                    // (docs/ux/navigation.md section 4, "Also with an empty
                    // list").
                    showsEmptyConversationState(rows.isEmpty() && invited.isEmpty(), query) && !showBar ->
                        if (restoredChats == me.lakko.app.backup.RestoredChatsHint.CARD) {
                            RestoredChatsPanel(onOpenContacts = onOpenContacts)
                        } else {
                            EmptyPanel(onStart = { createMenuOpen = true })
                        }
                    // UX who-can-find-me 6.1 — the people half of the search
                    // field replaces the plain lookup row when it is wired.
                    searching && foundNoRow && peopleSearch != null && peopleSession != null -> Column(
                        modifier = Modifier
                            .fillMaxSize()
                            .verticalScroll(rememberScrollState()),
                    ) {
                        PeopleSearchSection(
                            query = query,
                            service = peopleSearch,
                            session = peopleSession,
                            conversationMatches = 0,
                            channelName = knownChannelName,
                            onOpenChannel = { id, handle ->
                                query = ""
                                onSubscribeToChannel(id, handle)
                            },
                            onOpenContact = { key ->
                                query = ""
                                onOpenContact(key)
                            },
                            conversations = conversationSearch,
                        )
                    }
                    searching && foundNoRow -> NoMatchPanel()
                    searching || !showBar -> ConversationList(
                        rows = visible,
                        folder = null,
                        handlers = rowHandlers,
                        invited = visibleInvited,
                        onOpenPendingChat = onOpenPendingChat,
                        // UX who-can-find-me 6.1 — people under the matching
                        // conversations.
                        footer = if (searching && peopleSearch != null && peopleSession != null) {
                            {
                                PeopleSearchSection(
                                    query = query,
                                    service = peopleSearch,
                                    session = peopleSession,
                                    conversationMatches = visible.size + visibleInvited.size,
                                    channelName = knownChannelName,
                                    onOpenChannel = { id, handle ->
                                        query = ""
                                        onSubscribeToChannel(id, handle)
                                    },
                                    onOpenContact = { key ->
                                        query = ""
                                        onOpenContact(key)
                                    },
                                    excludeKeysB64 = visibleInvitedKeys,
                                    chatPeersB64 = visibleChatPeers,
                                    conversations = conversationSearch,
                                )
                            }
                        } else {
                            null
                        },
                    )
                    // Android: the list pages sideways in step with the tabs.
                    else -> HorizontalPager(
                        state = pagerState,
                        modifier = Modifier.fillMaxSize(),
                        key = { page -> if (page == 0) "all" else folders.getOrNull(page - 1)?.idHex ?: "gone-$page" },
                    ) { page ->
                        val folder = folders.getOrNull(page - 1)
                        if (folder == null) {
                            if (rows.isEmpty() && invited.isEmpty()) {
                                if (restoredChats == me.lakko.app.backup.RestoredChatsHint.CARD) {
                                    RestoredChatsPanel(onOpenContacts = onOpenContacts)
                                } else {
                                    EmptyPanel(onStart = { createMenuOpen = true })
                                }
                            } else {
                                ConversationList(
                                    rows = rows,
                                    folder = null,
                                    handlers = rowHandlers,
                                    invited = invited,
                                    onOpenPendingChat = onOpenPendingChat,
                                )
                            }
                        } else {
                            val inFolder = remember(folder, allRows) { folderRows(folder, allRows) }
                            if (inFolder.isEmpty()) {
                                EmptyFolderPanel(onEdit = { onEditFolder(folder.idHex) })
                            } else {
                                ConversationList(rows = inFolder, folder = folder, handlers = rowHandlers)
                            }
                        }
                    }
                }
            }
        }
    }

    deletingFolder?.let { folder ->
        DeleteFolderDialog(
            folder = folder,
            onDismiss = { deletingFolder = null },
            onConfirm = {
                deletingFolder = null
                scope.launch { actions?.delete(folder) }
            },
        )
    }
}

/**
 * Every conversation this device holds - chats, groups and channels - as the
 * rows Compose draws, NOT yet ordered. One place builds them, for the merged
 * list, every folder, the folder editor's live count and the chat picker, so
 * a chat cannot be in a folder on one screen and missing from it on another.
 */
@Composable
internal fun rememberConversationRows(
    repository: ChatRepository,
    channels: ChannelsUiState?,
    deviceGroupSync: DeviceGroupSyncRepository?,
): List<ConversationListRow> {
    val state by repository.state.collectAsStateWithLifecycle()
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

    val zone = remember { ZoneId.systemDefault() }
    // ADR-0061: dates, times and every row line in the interface language.
    val strings = LocalStrings.current
    val locale = remember(strings.language) { Locale.forLanguageTag(strings.language) }
    val use24h = remember(context) { me.lakko.app.l10n.DeviceFormats.is24Hour(context) }

    // Chats and groups. They were already one store split by a filter, so they
    // merge by dropping it.
    val threadRows = remember(state, nameVersion, groupNameVersion, flagsMap, readCursorMap, strings) {
        threadConversationRows(
            context = context,
            repository = repository,
            directory = directory,
            // ADR-0073 R.5 — one row per conversation.
            threads = state.conversations,
            flagsMap = flagsMap,
            readCursorMap = readCursorMap,
            nowMs = System.currentTimeMillis(),
            zone = zone,
            use24h = use24h,
            locale = locale,
            strings = strings,
        )
    }

    // Channels are their own store, so they have to be mapped in. Their last
    // POST time becomes the same field as a chat's last MESSAGE time, or the
    // one ordering has nothing to order by.
    // `docs/ux/feed.md` section 5, "Channel posts" — a channel's count of new
    // posts: those newer than the newest one read, here, in the channel or in
    // the feed. Counted over the channel-post log the feed keeps.
    val listAccount = repository.currentAccountIdB64()
    val channelPostsVersion by me.lakko.app.feed.FeedSealedStore.channelPostsChanged.collectAsStateWithLifecycle()
    val channelPostCursors by androidx.compose.runtime.produceState(
        initialValue = emptyMap<String, List<Long>>(),
        listAccount,
        channelPostsVersion,
    ) {
        val acct = listAccount
        value = if (acct == null) {
            emptyMap()
        } else {
            kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.IO) {
                me.lakko.app.feed.FeedSealedStore.loadChannelPostLog(context, acct).channels
                    .mapValues { (_, slot) -> slot.posts.map { it.cursor } }
            }
        }
    }
    val readingFlow = remember(listAccount) {
        listAccount?.let { me.lakko.app.feed.FeedReadingStore.flow(context, it) }
            ?: MutableStateFlow(me.lakko.app.feed.FeedReading())
    }
    val reading by readingFlow.collectAsStateWithLifecycle()
    // ADR-0073 phase 2 / UX 16.6 — the channels a key reset orphaned on this
    // device carry their own secondary line.
    val orphanedChannels = channels?.orphaned?.collectAsStateWithLifecycle()?.value ?: emptySet()
    val channelRows = remember(channelHandles, channelPosts, channelKeys, channelPostCursors, reading.channelRead, strings, orphanedChannels) {
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
                strings = strings,
            )
            ConversationListRow(
                id = channel.id,
                kind = ConversationRowKind.CHANNEL,
                title = model.title,
                preview = if (me.lakko.app.channels.channelIsOrphaned(channel.id, orphanedChannels)) {
                    strings["channels.keys_reset.row_sub"]
                } else {
                    model.subtitle
                },
                time = model.time,
                lastActivityMs = model.sortMs,
                // Pinned, muted and archived are keyed on a conversation id a
                // channel does not carry, so a channel row shows none of them
                // (section 6.1). Its count is the feed's channel read state.
                unread = me.lakko.app.feed.channelUnreadCount(
                    postCursors = channelPostCursors[channel.id].orEmpty(),
                    readThrough = reading.channelRead[channel.id],
                    own = channelKeys.containsKey(channel.id),
                ),
            )
        }
    }
    return remember(threadRows, channelRows) { threadRows + channelRows }
}

/** What a row can do, shared by every tab. */
internal class RowHandlers(
    val folders: List<ChatFolder>,
    /** `false` while the sync runtime is not there: no folder items. */
    val foldersEnabled: Boolean,
    val inFolder: (ChatFolder, ConversationListRow) -> Boolean,
    val onToggleFlag: (ConversationListRow, FlagField) -> Unit,
    val onToggleFolderPin: (ChatFolder, ConversationListRow) -> Unit,
    val onToggleMembership: (ChatFolder, ConversationListRow, Boolean) -> Unit,
    val onNewFolderWith: (ConversationListRow) -> Unit,
    val onOpenThread: (String) -> Unit,
    val onOpenChannel: (String) -> Unit,
    val quietCallOffer: me.lakko.app.calls.QuietCallOffer?,
    val onAcceptCall: (Boolean) -> Unit,
    val onDeclineCall: () -> Unit,
)

/** One tab's rows. [folder] is the tab (`null` = All chats). */
@Composable
private fun MembershipEndBanner(
    notice: me.lakko.app.chat.MembershipEndNotice,
    onClose: () -> Unit,
) {
    val fallback = t("chat.group.fallback_name")
    androidx.compose.material3.Surface(
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(start = 16.dp, end = 4.dp, top = 4.dp, bottom = 4.dp),
        ) {
            Text(
                text = me.lakko.app.chat.endNoticeLine(notice.name, fallback).text(),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.weight(1f),
            )
            IconButton(onClick = onClose) {
                Icon(
                    Icons.Filled.Close,
                    contentDescription = t("common.close"),
                )
            }
        }
    }
}

@Composable
private fun ConversationList(
    rows: List<ConversationListRow>,
    folder: ChatFolder?,
    handlers: RowHandlers,
    /** Drawn after the last row (the people search while searching). */
    footer: (@Composable () -> Unit)? = null,
    /** ADR-0079 — the pending contacts, drawn first like any row. */
    invited: List<me.lakko.app.contacts.PendingContact> = emptyList(),
    onOpenPendingChat: (String) -> Unit = {},
) {
    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(vertical = 4.dp),
    ) {
        items(items = invited, key = { "invited:${it.accountIdHex}" }) { contact ->
            InvitedChatRow(contact = contact, onClick = { onOpenPendingChat(contact.accountIdHex) })
        }
        items(items = rows, key = { "${it.kind}:${it.id}" }) { row ->
            val menu = RowMenuModel(row = row, folder = folder, handlers = handlers)
            if (row.kind == ConversationRowKind.CHANNEL) {
                // Same avatar, same title, same second line as every other
                // row. What it does NOT have is the badge, the glyphs and the
                // flag items — all keyed on a conversation id a channel does
                // not carry. Its menu holds only the folder items.
                ChannelRow(row = row, menu = menu, onClick = { handlers.onOpenChannel(row.id) })
            } else {
                SwipeableThreadRow(
                    row = row,
                    menu = menu,
                    onClick = { handlers.onOpenThread(row.id) },
                    // Section 2.1.1 — the row carries Accept and Decline for
                    // exactly the conversation the muted call is ringing in,
                    // and for no other.
                    quietCallOffer = handlers.quietCallOffer?.takeIf { it.isFor(row.headId) },
                    // The same two entry points the full-screen ring screen
                    // drives.
                    onAcceptCall = handlers.onAcceptCall,
                    onDeclineCall = handlers.onDeclineCall,
                )
            }
        }
        if (footer != null) {
            item(key = "people-search") { footer() }
        }
    }
}

/**
 * ADR-0079 section 4 — the chat with a person added through their invite link
 * before their app answered: drawn like any row (no "pending" badge).
 */
@Composable
private fun InvitedChatRow(contact: me.lakko.app.contacts.PendingContact, onClick: () -> Unit) {
    val title = me.lakko.app.contacts.PendingContacts.name(contact) ?: t("invite.contact.page.unnamed")
    androidx.compose.foundation.layout.Row(
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 8.dp),
    ) {
        me.lakko.app.ui.components.LakkoAvatar(
            name = title,
            seed = contact.accountIdHex,
            size = 48.dp,
            personKey = me.lakko.app.ui.components.personKeyBytes(contact.signaturePubkeyB64),
        )
        androidx.compose.foundation.layout.Spacer(Modifier.width(12.dp))
        androidx.compose.foundation.layout.Column {
            androidx.compose.material3.Text(
                text = title,
                style = androidx.compose.material3.MaterialTheme.typography.bodyLarge,
                maxLines = 1,
            )
            androidx.compose.material3.Text(
                text = t("chat.pending.waiting", mapOf("name" to title)),
                style = androidx.compose.material3.MaterialTheme.typography.bodyMedium,
                color = androidx.compose.material3.MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
            )
        }
    }
}

/** A channel row: tap opens it, long press opens the folder items. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ChannelRow(row: ConversationListRow, menu: RowMenuModel, onClick: () -> Unit) {
    var menuOpen by remember { mutableStateOf(false) }
    Box {
        ConversationRowBody(
            row = row,
            modifier = Modifier.combinedClickable(
                onClick = onClick,
                onLongClick = { if (menu.hasItems) menuOpen = true },
            ),
        )
        RowMenu(expanded = menuOpen, onDismiss = { menuOpen = false }, model = menu)
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
    onOpenSaved: (() -> Unit)? = null,
) {
    DropdownMenu(expanded = expanded, onDismissRequest = onDismiss) {
        DropdownMenuItem(
            text = { Text(t("chats.create.chat")) },
            leadingIcon = { Icon(Icons.Filled.PersonAdd, contentDescription = null) },
            onClick = { onDismiss(); onNewChat() },
        )
        DropdownMenuItem(
            text = { Text(t("chats.create.group")) },
            leadingIcon = { Icon(Icons.Filled.Groups, contentDescription = null) },
            onClick = { onDismiss(); onCreateGroup() },
        )
        DropdownMenuItem(
            text = { Text(t("chats.create.channel")) },
            leadingIcon = { Icon(Icons.Filled.Campaign, contentDescription = null) },
            onClick = { onDismiss(); onCreateChannel() },
        )
        if (onOpenSaved != null) {
            DropdownMenuItem(
                text = { Text(t("saved.title")) },
                leadingIcon = { Icon(Icons.Filled.Bookmark, contentDescription = null) },
                onClick = { onDismiss(); onOpenSaved() },
            )
        }
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
            me.lakko.app.rethrowIfCancelled(cause)
            android.util.Log.w("ChatListScreen", "setFlags failed", cause)
        }
    }
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
    strings: Strings = Localization.strings.value,
): List<ConversationListRow> = threads.map { thread ->
    val kind = conversationKind(repository, thread.groupIdHex)
    val last = previewSourceRow(thread.messages)
    val lastMs = last?.let { parseMessageTimeMs(it.createdAt) }
    // Section 10 of the group privacy spec — a conversation with no message
    // sorts by the moment this device created or joined it.
    val activityMs = me.lakko.app.chat.conversationActivityMs(lastMs, thread.joinedAtMs)
    val senderName = last?.takeIf { it.direction == Direction.Incoming }
        ?.authorKey?.let { directory.resolve(it).primary }
    val flags = flagsMap[thread.conversationHex]
    val title = conversationTitle(context, repository, directory, thread.groupIdHex, strings)
    ConversationListRow(
        // ADR-0073 R.5 — the conversation id: pins, mutes, the archive,
        // folders and the order are keyed on it and stay with the
        // conversation when it continues in a successor group.
        id = thread.conversationHex,
        headId = thread.groupIdHex,
        kind = kind.toRowKind(),
        title = title,
        preview = rowPreview(
            thread,
            last,
            isGroup = kind == ConversationKind.Group,
            senderName = senderName,
            strings = strings,
        ),
        time = activityMs?.let { formatListTime(it, nowMs, zone, use24h, locale) },
        lastActivityMs = activityMs,
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
        unreadOverride = flags?.unreadOverride == true && !thread.membershipEnded,
        membershipEnded = thread.membershipEnded,
        peerKeyB64 = if (kind == ConversationKind.Chat) {
            repository.oneToOnePeerKey(thread.groupIdHex)?.let { java.util.Base64.getEncoder().encodeToString(it) }
        } else {
            null
        },
    )
}

/**
 * The row a conversation's preview is taken from: the last message or call
 * row. A call row IS the conversation's last event and belongs in the
 * preview; the receive loop's bookkeeping rows (a commit, an external-join
 * request) are not and never did, in a group or a one-to-one alike. Found
 * live: a group whose only rows were commits showed the literal
 * "[mls commit]" as its preview, which is a protocol name in a person's face
 * and the one thing the copy rules forbid outright; and in a one-to-one,
 * whose people cannot change (ADR-0062), a commit row is never a "Members
 * changed" line either (2026-10-29). A call row is a system row carrying its
 * parts, so the two are told apart by what the row HOLDS, not by its text.
 */
internal fun previewSourceRow(messages: List<ChatMessage>): ChatMessage? =
    messages.lastOrNull { it.direction != Direction.System || it.call != null }

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
    strings: Strings = Localization.strings.value,
): String {
    val endedLine = me.lakko.app.chat.chatListLine(
        me.lakko.app.chat.membershipState(
            isActive = !thread.membershipEnded,
            leaveRequestedAtMs = null,
        ),
    )
    if (endedLine != null) return endedLine.renderWith(strings)
    // ADR-0087 UX 9.4 — the row stays until the leave lands, and says so.
    if (thread.leaveWaiting) return strings["chat.members.pending.waiting_leave"]
    val noMessages = strings["chats.row.no_messages"]
    if (last == null) return noMessages
    if (last.direction == Direction.System) {
        // Only a row this build can render: an unreadable bookkeeping row is
        // never shown raw.
        return last.call?.let { me.lakko.app.calls.callRowText(it, strings) } ?: noMessages
    }
    val line = previewLine(
        text = last.text,
        attachments = last.attachments.map { PreviewAttachment(it.kind, it.mime) },
        deleted = last.deleted,
        author = previewPrefix(last.direction, isGroup, senderName),
        strings = strings,
        unsupported = last.unsupportedVersion != null,
    )
    return line.ifEmpty { noMessages }
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
 *     with no cursor never showed a badge, whatever arrived.
 *
 *  A row the pairing history brought is never unread here, with a cursor or
 *  without one ([me.lakko.app.chat.rowInPairingHistory]; the local count leaves
 *  those rows out as they arrive). */
internal fun unreadCount(thread: ChatThread, cursor: ReadCursorState?): Int {
    // ADR-0063 section 5 — a message the feed showed is read, whether or not
    // the chat was opened: its id sits in `feedSeen` or under the feed's own
    // watermark.
    val feedThrough = thread.feedSeenThrough
    // ADR-0073 R.5 — the cursor is a position in the conversation's
    // TIMELINE (segment, message id): a row of an older segment the position
    // has not passed is unread, and a position that only reached the old
    // group reads nothing of its successor, whose ids restart.
    val cursorCount = cursor?.let { c ->
        // An undecodable cursor counts nothing, as before.
        val position = me.lakko.app.chat.decodeReadPosition(c.upToMessageId) ?: return@let 0
        thread.messages.count { m ->
            m.direction != Direction.Outgoing &&
                !me.lakko.app.chat.rowInPairingHistory(thread, m) &&
                me.lakko.app.chat.rowPastReadPosition(thread, m, position) &&
                // The feed's own marks are the head group's ids.
                (m.isHistory || (m.messageId !in thread.feedSeen && (feedThrough == null || m.messageId > feedThrough)))
        }
    }
    return me.lakko.app.chat.combinedUnread(
        cursorCount = cursorCount,
        local = me.lakko.app.chat.LocalUnread(
            count = thread.localUnread,
            viewedThisSession = thread.unreadViewedThisSession,
        ),
    )
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
        placeholder = { Text(t("chats.search.placeholder")) },
        singleLine = true,
        leadingIcon = { Icon(Icons.Filled.Search, contentDescription = null) },
        trailingIcon = if (query.isNotEmpty()) {
            {
                IconButton(onClick = { onQueryChange("") }) {
                    Icon(Icons.Filled.Clear, contentDescription = t("chats.search.clear"))
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
 * Inside a folder the pin swipe acts on the folder's own pins, like the menu.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeableThreadRow(
    row: ConversationListRow,
    menu: RowMenuModel,
    onClick: () -> Unit,
    quietCallOffer: me.lakko.app.calls.QuietCallOffer? = null,
    onAcceptCall: (withVideo: Boolean) -> Unit = {},
    onDeclineCall: () -> Unit = {},
) {
    val dismissState = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            when (value) {
                SwipeToDismissBoxValue.StartToEnd -> if (menu.pinEnabled) menu.onPin()
                SwipeToDismissBoxValue.EndToStart -> menu.onFlag(FlagField.Archive)
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
                t(if (row.pinned) "chats.action.unpin" else "chats.action.pin")
            } else {
                t(if (row.archived) "chats.action.unarchive" else "chats.action.archive")
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
                menu = menu,
                onClick = onClick,
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
    menu: RowMenuModel,
    onClick: () -> Unit,
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
        RowMenu(expanded = menuOpen, onDismiss = { menuOpen = false }, model = menu)
    }
}

/**
 * The row menu's state for one row in one tab: which pin set Pin acts on,
 * which folders the row is in, and what each item does.
 */
internal class RowMenuModel(
    val row: ConversationListRow,
    /** The tab the row sits in; `null` = All chats. */
    val folder: ChatFolder?,
    private val handlers: RowHandlers,
) {
    private val isChannel = row.kind == ConversationRowKind.CHANNEL

    /** Pin / Unpin shows for a chat or group everywhere, and for a channel
     *  inside a folder (a folder's pins name any conversation). */
    val showPin: Boolean = folder != null || !isChannel

    /** Pinned in the pin set this tab uses. */
    val pinned: Boolean = row.pinned

    /** UX spec 3.4 — ten pins per folder. */
    val pinEnabled: Boolean = folder == null || pinned || folder.pinned.size < FolderLimits.MAX_PINS

    val showFlags: Boolean = !isChannel
    val showFolderItems: Boolean = handlers.foldersEnabled
    val folders: List<ChatFolder> = handlers.folders
    val canCreateFolder: Boolean = handlers.folders.size < FolderLimits.MAX_FOLDERS

    val hasItems: Boolean get() = showPin || showFlags || showFolderItems

    fun onPin() {
        if (folder != null) handlers.onToggleFolderPin(folder, row) else handlers.onToggleFlag(row, FlagField.Pin)
    }

    fun onFlag(field: FlagField) {
        if (!isChannel) handlers.onToggleFlag(row, field)
    }

    fun isIn(f: ChatFolder): Boolean = handlers.inFolder(f, row)

    /** Whether tapping [f] in the list would pass a hand-list limit. */
    fun blocked(f: ChatFolder): Boolean {
        val state = membershipToggle(f, row, isIn(f)) ?: return false
        return membershipToggleBlocked(f, row, state)
    }

    fun toggle(f: ChatFolder) = handlers.onToggleMembership(f, row, isIn(f))

    fun newFolder() = handlers.onNewFolderWith(row)
}

/**
 * The long-press menu. "Add to folder" is a second step of the same dropdown
 * on Android (`docs/ux/chat-folders.md` 5): the folders with a checkmark where
 * the chat is in them, then "New folder...".
 */
@Composable
private fun RowMenu(expanded: Boolean, onDismiss: () -> Unit, model: RowMenuModel) {
    var folderStep by remember { mutableStateOf(false) }
    val close = {
        folderStep = false
        onDismiss()
    }
    DropdownMenu(expanded = expanded, onDismissRequest = close) {
        if (!folderStep) {
            if (model.showPin) {
                DropdownMenuItem(
                    text = { Text(t(if (model.pinned) "chats.action.unpin" else "chats.action.pin")) },
                    leadingIcon = { Icon(Icons.Filled.PushPin, contentDescription = null) },
                    enabled = model.pinEnabled,
                    onClick = { close(); model.onPin() },
                )
                if (!model.pinEnabled) {
                    MenuHelperLine(t("folders.pin_limit", emptyMap(), mapOf("count" to FolderLimits.MAX_PINS.toULong())))
                }
            }
            if (model.showFlags) {
                DropdownMenuItem(
                    text = { Text(t(if (model.row.muted) "chats.action.unmute" else "chats.action.mute")) },
                    leadingIcon = { Icon(Icons.Filled.NotificationsOff, contentDescription = null) },
                    onClick = { close(); model.onFlag(FlagField.Mute) },
                )
                DropdownMenuItem(
                    text = { Text(t(if (model.row.archived) "chats.action.unarchive" else "chats.action.archive")) },
                    leadingIcon = {
                        Icon(
                            if (model.row.archived) Icons.Filled.Unarchive else Icons.Filled.Archive,
                            contentDescription = null,
                        )
                    },
                    onClick = { close(); model.onFlag(FlagField.Archive) },
                )
            }
            if (model.showFolderItems) {
                DropdownMenuItem(
                    text = { Text(t("folders.add_to_folder")) },
                    leadingIcon = { Icon(Icons.Filled.Folder, contentDescription = null) },
                    // With no folders yet it goes straight to the editor, as
                    // "New folder..." would.
                    enabled = model.folders.isNotEmpty() || model.canCreateFolder,
                    onClick = {
                        if (model.folders.isEmpty()) {
                            close()
                            model.newFolder()
                        } else {
                            folderStep = true
                        }
                    },
                )
                val tab = model.folder
                if (tab != null) {
                    DropdownMenuItem(
                        text = { Text(t("folders.remove_from_folder")) },
                        leadingIcon = { Icon(Icons.Filled.FolderOff, contentDescription = null) },
                        onClick = { close(); model.toggle(tab) },
                    )
                }
            }
        } else {
            for (f in model.folders) {
                val checked = model.isIn(f)
                val blocked = model.blocked(f)
                DropdownMenuItem(
                    text = { Text(f.name) },
                    leadingIcon = {
                        if (checked) {
                            Icon(Icons.Filled.Check, contentDescription = t("folders.in_folder_a11y"))
                        } else {
                            Spacer(Modifier.size(24.dp))
                        }
                    },
                    enabled = !blocked,
                    onClick = { close(); model.toggle(f) },
                )
            }
            DropdownMenuItem(
                text = { Text(t("folders.new_ellipsis")) },
                leadingIcon = { Icon(Icons.Filled.CreateNewFolder, contentDescription = null) },
                enabled = model.canCreateFolder,
                onClick = { close(); model.newFolder() },
            )
            if (!model.canCreateFolder) {
                MenuHelperLine(t("folders.limit", emptyMap(), mapOf("count" to FolderLimits.MAX_FOLDERS.toULong())))
            }
        }
    }
}

@Composable
private fun MenuHelperLine(text: String) {
    Text(
        text = text,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp).width(220.dp),
    )
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
            personKey = me.lakko.app.ui.components.personKeyBytes(row.peerKeyB64.takeIf { row.kind == ConversationRowKind.CHAT }),
            groupIdHex = row.id.takeIf { row.kind != ConversationRowKind.CHAT },
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
                        contentDescription = t("chats.row.muted"),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(16.dp),
                    )
                }
                if (row.pinned) {
                    Spacer(Modifier.width(6.dp))
                    Icon(
                        Icons.Filled.PushPin,
                        contentDescription = t("chats.row.pinned"),
                        tint = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.size(16.dp),
                    )
                }
                // reactions-and-emoji section 12 — a small heart beside the
                // badge while others' reactions to own messages are unseen.
                val unseenVersion by me.lakko.app.chat.UnseenReactions.version.collectAsStateWithLifecycle()
                val unseen = remember(unseenVersion, row.id) {
                    row.kind != ConversationRowKind.CHANNEL && me.lakko.app.chat.UnseenReactions.any(row.id)
                }
                if (unseen) {
                    Spacer(Modifier.width(6.dp))
                    val heartLabel = t("chats.row.unseen_reactions")
                    Text(
                        text = "\u2764\uFE0F",
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.semanticsLabel(heartLabel),
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
internal fun UnreadBadge(count: Int, muted: Boolean) {
    val label = if (count > 99) t("chats.row.unread_overflow") else count.toString()
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
                text = t("chats.empty.title"),
                style = MaterialTheme.typography.titleMedium,
            )
            Spacer(Modifier.height(12.dp))
            Button(onClick = onStart) { Text(t("chats.empty.action")) }
        }
    }
}

/** The empty list of a device restored with no other device left
 *  (`docs/ux/identity-recovery.md` 7.2). Not a loading state: no spinner. */
@Composable
private fun RestoredChatsPanel(onOpenContacts: () -> Unit) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text(
                text = t("chats.restored.card"),
                style = MaterialTheme.typography.bodyLarge,
                textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            )
            Spacer(Modifier.height(12.dp))
            Button(onClick = onOpenContacts) { Text(t("nav.contacts")) }
        }
    }
}

/** Nothing local matched and no people search is wired: say so. */
@Composable
private fun NoMatchPanel() {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = t("chats.search.no_results"),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/** A plain spoken label on a decorative glyph. */
private fun Modifier.semanticsLabel(label: String): Modifier =
    this.semantics { contentDescription = label }
