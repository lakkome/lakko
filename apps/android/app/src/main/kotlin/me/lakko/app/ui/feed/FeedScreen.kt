// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.feed

import android.content.Context
import android.content.Intent
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Info
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.IconButton
import androidx.compose.material3.SnackbarDuration
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.SnackbarResult
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.channels.ChannelsUiState
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.ChatRepository
import me.lakko.app.chat.Direction
import me.lakko.app.config.ApiEndpoints
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.devices.sync.DeviceGroupSyncRepository
import me.lakko.app.feed.FEED_SEEN_DWELL_MS
import me.lakko.app.feed.FeedAttachment
import me.lakko.app.feed.FeedCandidateRef
import me.lakko.app.feed.FeedContext
import me.lakko.app.feed.FeedCore
import me.lakko.app.feed.FeedIcon
import me.lakko.app.feed.FeedReading
import me.lakko.app.feed.FeedReadingStore
import me.lakko.app.feed.advanceChannelRead
import me.lakko.app.feed.hiddenSources
import me.lakko.app.feed.hideFromFeeds
import me.lakko.app.feed.isChannelPostUnread
import me.lakko.app.feed.newDividerIndex
import me.lakko.app.feed.newItemCount
import me.lakko.app.feed.pruneLooked
import me.lakko.app.feed.showInFeedsAgain
import me.lakko.app.feed.withLooked
import me.lakko.app.feed.withoutHidden
import me.lakko.app.feed.FeedInput
import me.lakko.app.feed.FeedMessage
import me.lakko.app.feed.FeedPrefs
import me.lakko.app.feed.FeedPresetKey
import me.lakko.app.feed.FeedReaction
import me.lakko.app.feed.FeedRuleRefusal
import me.lakko.app.feed.FeedSealedStore
import me.lakko.app.feed.FeedSource
import me.lakko.app.feed.FeedSourceKind
import me.lakko.app.feed.FeedState
import me.lakko.app.feed.GroupSeenState
import me.lakko.app.feed.RankedItem
import me.lakko.app.feed.SavedFeed
import me.lakko.app.feed.UniffiFeedCore
import me.lakko.app.feed.blankDraft
import me.lakko.app.feed.bodyOf
import me.lakko.app.feed.buildFeedInput
import me.lakko.app.feed.exportDraft
import me.lakko.app.feed.isSeen
import me.lakko.app.feed.itemKey
import me.lakko.app.feed.newFeedId
import me.lakko.app.feed.rankFeedInput
import me.lakko.app.feed.reactionSummary
import me.lakko.app.feed.readCursorBytes
import me.lakko.app.feed.readCursorId
import me.lakko.app.feed.removeFeed
import me.lakko.app.feed.repliesLabel
import me.lakko.app.feed.shareFileName
import me.lakko.app.feed.shareLink
import me.lakko.app.feed.upsertFeed
import me.lakko.app.feed.whyLine
import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.ui.channels.channelTitle
import me.lakko.app.ui.discover.DiscoverCardView
import me.lakko.app.ui.discover.DiscoverView
import me.lakko.app.ui.chat.conversationTitle
import me.lakko.app.ui.chat.formatListTime
import me.lakko.app.ui.chat.isGroupConversation
import uniffi.lakko_ffi_uniffi.FeedRule
import java.time.ZoneId
import java.util.Locale

/**
 * The Feed destination (`docs/ux/feed.md`, ADR-0063). Mirror of
 * `apps/web/src/routes/Feed.tsx`.
 *
 * Messages from the person's groups and posts from their channels, ranked on
 * this device by the one rule engine in the core. What is here: the preset
 * switcher and the saved feeds, the items with their reactions and folded
 * replies, opening an item in its chat or channel, the "Show N new" pill, the
 * empty and first-run states, Edit and Share for a saved feed, and the editor.
 *
 * Seen (ADR-0063 section 5): a group message fully on screen for
 * [FEED_SEEN_DWELL_MS] joins its group's seen set; when that makes the set
 * contiguous further than before, the group gets a read receipt and the other
 * devices a read cursor at the new watermark, never past a message the person
 * did not see. A channel post seen the same way moves that channel's "newest
 * read post" marker ([FeedReadingStore]), which the channel view moves too and
 * the conversation list counts from.
 *
 * Discover (ADR-0059, UX spec section 6) is the chip after the presets:
 * channel suggestions instead of items ([DiscoverView]). A saved feed whose
 * rule asks for suggestions ("rarely" / "sometimes") mixes the same cards in
 * between its items ([cardsAfterItems]).
 *
 * Also here (UX spec section 5): the item menu on long-press, Hide from feeds
 * with its Undo and its list in the feed menu, the "New" divider per feed,
 * and pull to refresh. Not yet here: unpinning a preset.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun FeedScreen(
    chatRepository: ChatRepository,
    channels: ChannelsUiState,
    deviceGroupSync: DeviceGroupSyncRepository,
    contactsRepository: ContactsRepository,
    ownUsername: String?,
    onOpenGroupMessage: (groupIdHex: String, messageId: Long) -> Unit,
    onOpenChannel: (channelId: String) -> Unit,
    onGoToChats: () -> Unit,
    /** ADR-0059 — the directory routes Discover reads. */
    channelsClient: me.lakko.app.channels.ChannelsClient,
    /** ADR-0059 — subscribe to a channel a card verified (the subscribe route). */
    onSubscribeChannel: (channelId: String, handle: String) -> Unit,
    /** UX spec section 5 — the item menu's Forward: open the chat's "Forward
     *  to" picker; the message is already in [me.lakko.app.chat.PendingForward]. */
    onForward: () -> Unit = {},
    core: FeedCore = UniffiFeedCore,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val account = chatRepository.currentAccountIdB64()

    val chatState by chatRepository.state.collectAsStateWithLifecycle()
    val flags by deviceGroupSync.flags.collectAsStateWithLifecycle()
    val readCursors by deviceGroupSync.readCursors.collectAsStateWithLifecycle()
    val channelList by channels.channels.collectAsStateWithLifecycle()
    val ownChannelKeys by channels.keysByChannel.collectAsStateWithLifecycle()
    val contactsState by contactsRepository.state.collectAsStateWithLifecycle()
    // UX spec section 6.2 — a Discover card for a channel this device holds
    // says so and offers only Open.
    val relationOf: (String) -> me.lakko.app.discover.CardRelation = remember(ownChannelKeys, channelList) {
        val publishes = ownChannelKeys.keys
        val inList = channelList.map { it.id }
        ({ id -> me.lakko.app.discover.cardRelation(id, publishes, inList) })
    }
    val postsVersion by FeedSealedStore.channelPostsChanged.collectAsStateWithLifecycle()
    val directory = remember(chatRepository) { ProfileDirectory.get(chatRepository.context) }
    val nameVersion by directory.version.collectAsStateWithLifecycle()
    val groupNameVersion by me.lakko.app.chat.GroupNameStore.version.collectAsStateWithLifecycle()
    val strings = LocalStrings.current

    // UX spec section 5 — hidden from feeds, the "New" divider's last looks,
    // and the channel read markers, per account on this device.
    val readingFlow = remember(account) {
        account?.let { FeedReadingStore.flow(context, it) } ?: kotlinx.coroutines.flow.MutableStateFlow(FeedReading())
    }
    val reading by readingFlow.collectAsStateWithLifecycle()
    fun updateReading(change: (FeedReading) -> FeedReading) {
        val acct = account ?: return
        FeedReadingStore.update(context, acct, change)
    }

    // ----- Selection + saved feeds (UX spec section 4.1) -------------------

    var selection by remember { mutableStateOf(initialSelection(FeedPrefs.selection(context))) }
    fun choose(key: String) {
        selection = key
        FeedPrefs.setSelection(context, key)
    }

    var savedState by remember { mutableStateOf(FeedState()) }
    var saved by remember { mutableStateOf<List<SavedView>>(emptyList()) }
    suspend fun reloadSaved() {
        val acct = account ?: return
        val state = withContext(Dispatchers.IO) { FeedSealedStore.loadFeedState(context, acct) }
        savedState = state
        saved = state.feeds.mapNotNull { f ->
            try {
                SavedView(f.id, core.decode(f.rule))
            } catch (_: FeedRuleRefusal) {
                // A stored rule the core refuses is not shown.
                null
            } catch (_: Throwable) {
                null
            }
        }
        if (selection.startsWith(CUSTOM_PREFIX) && saved.none { CUSTOM_PREFIX + it.id == selection }) {
            choose(FeedPresetKey.LATEST.name)
        }
    }
    LaunchedEffect(account) { reloadSaved() }

    suspend fun persistSaved(next: FeedState): Boolean {
        val acct = account ?: return false
        val ok = withContext(Dispatchers.IO) { FeedSealedStore.saveFeedState(context, acct, next) }
        if (ok) reloadSaved()
        return ok
    }

    val selectedSaved = saved.firstOrNull { CUSTOM_PREFIX + it.id == selection }
    val whyPreset: FeedPresetKey = presetOf(selection)
        ?: if ((selectedSaved?.rule?.weights?.reactions?.toInt() ?: 0) > 0) FeedPresetKey.POPULAR else FeedPresetKey.LATEST

    // ----- Discover (ADR-0059) ----------------------------------------------

    val discover = remember(account) {
        me.lakko.app.discover.DiscoverModel(
            backend = me.lakko.app.discover.ChannelsDiscoverBackend(context.applicationContext, channelsClient),
            account = { chatRepository.currentAccountIdB64() },
            scope = scope,
            appLanguageCode = me.lakko.app.discover.appLanguage(Locale.getDefault().toLanguageTag()),
        )
    }
    LaunchedEffect(discover) {
        discover.load()
        runCatching { chatRepository.sharedChannels.open() }
    }
    val discoverPrefs by discover.prefs.collectAsStateWithLifecycle()
    val discoverDetails by discover.details.collectAsStateWithLifecycle()
    val discoverGone by discover.gone.collectAsStateWithLifecycle()
    val sharedRow by chatRepository.sharedChannels.row.collectAsStateWithLifecycle()
    val suggestionRate = when (selectedSaved?.rule?.suggestions) {
        uniffi.lakko_ffi_uniffi.FeedSuggestions.RARELY -> me.lakko.app.discover.SuggestionRate.RARELY
        uniffi.lakko_ffi_uniffi.FeedSuggestions.SOMETIMES -> me.lakko.app.discover.SuggestionRate.SOMETIMES
        else -> me.lakko.app.discover.SuggestionRate.NEVER
    }
    val discoverLoaded by discover.loaded.collectAsStateWithLifecycle()
    LaunchedEffect(suggestionRate, discoverLoaded) {
        if (suggestionRate != me.lakko.app.discover.SuggestionRate.NEVER && discoverLoaded) discover.refresh()
    }
    val discoverCards: List<me.lakko.app.discover.DirectoryCard> = remember(
        discoverPrefs, discoverDetails, discoverGone, sharedRow, channelList, contactsState, chatState, nameVersion, groupNameVersion,
        reading.hidden, strings,
    ) {
        val liveGroups = chatState.threads
            .filter { !it.membershipEnded && isGroupConversation(chatRepository, it.groupIdHex) }
            .map { it.groupIdHex }
            .toSet()
        val contactKeys = contactsState.accepted
            .map { java.util.Base64.getEncoder().encodeToString(it.request.fromPubkey) }
            .toSet()
        // A group hidden from feeds suggests nothing either (UX spec section 5).
        val shared = me.lakko.app.discover.aggregateShared(
            sharedRow,
            chatRepository.ownPersonKeyB64(),
            liveGroups - reading.hidden,
        )
        val group = discover.groupCards(
            shared = shared,
            groupTitle = { hex -> conversationTitle(context, chatRepository, directory, hex, strings) },
            contactName = { key -> if (key in contactKeys) directory.resolve(key).primary else null },
        )
        me.lakko.app.discover.rankCards(
            discover.bucketCards() + group,
            discover.context(channelList.map { it.id }.toSet()),
        ).filter { it.channelIdHex !in reading.hidden }
    }

    var editing by remember { mutableStateOf<Editing?>(null) }
    var editError by remember { mutableStateOf<UiText?>(null) }
    var sharing by remember { mutableStateOf<SavedView?>(null) }
    var bannerDismissed by remember { mutableStateOf(FeedPrefs.bannerDismissed(context)) }
    var showingHidden by remember { mutableStateOf(false) }
    var feedMenuOpen by remember { mutableStateOf(false) }
    val snackbar = remember { SnackbarHostState() }

    // ----- Input -----------------------------------------------------------

    val channelPosts = remember(account, postsVersion) {
        account?.let { FeedSealedStore.loadChannelPostLog(context, it).channels } ?: emptyMap()
    }

    // Every group and channel; the editor and the "Hidden from feeds" list
    // read these. The feed itself reads [sources], without the hidden ones.
    val allSources: List<FeedSource> = remember(chatState, flags, channelList, nameVersion, groupNameVersion, strings) {
        val groups = chatState.threads
            .filter { isGroupConversation(chatRepository, it.groupIdHex) }
            .map { t ->
                FeedSource(
                    kind = FeedSourceKind.GROUP,
                    idHex = t.groupIdHex,
                    title = conversationTitle(context, chatRepository, directory, t.groupIdHex, strings),
                    muted = flags[t.groupIdHex]?.muted == true,
                    audience = chatRepository.memberCount(t.groupIdHex) ?: 0,
                )
            }
        val chans = channelList.map { c ->
            FeedSource(
                kind = FeedSourceKind.CHANNEL,
                idHex = c.id,
                title = channelTitle(c, strings),
                muted = flags[c.id]?.muted == true,
                audience = 0,
            )
        }
        groups + chans
    }
    // UX spec section 5 — hidden before ranking, so no preset, own feed or
    // preview sees them.
    val sources: List<FeedSource> = remember(allSources, reading.hidden) { withoutHidden(allSources, reading.hidden) }

    // Recency moves with the clock; a minute is fine-grained enough for a
    // ranking whose shortest half-life is six hours.
    var clock by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(Unit) {
        while (true) {
            delay(60_000L)
            clock = System.currentTimeMillis()
        }
    }

    val input: FeedInput = remember(sources, chatState, channelPosts, ownChannelKeys, contactsState, clock) {
        val groupIds = sources.filter { it.kind == FeedSourceKind.GROUP }.map { it.idHex }.toSet()
        buildFeedInput(
            FeedContext(
                sources = sources,
                groupMessages = chatState.threads
                    .filter { it.groupIdHex in groupIds }
                    .associate { t -> t.groupIdHex to t.messages.map(::feedMessageOf) },
                channelPosts = channelPosts,
                ownChannels = ownChannelKeys.keys,
                ownUsername = ownUsername?.takeIf { it.isNotBlank() },
                contactKeysB64 = contactsState.accepted
                    .map { java.util.Base64.getEncoder().encodeToString(it.request.fromPubkey) }
                    .toSet(),
                mentions = { text, name -> core.mentionsUsername(text, name) },
                nowMs = clock,
            ),
        )
    }

    fun ruleForSelection(): FeedRule? {
        presetOf(selection)?.let { return core.preset(it.preset) }
        return selectedSaved?.rule
    }

    var ranked by remember { mutableStateOf<List<RankedItem>?>(null) }
    LaunchedEffect(input, selection, saved) {
        val rule = ruleForSelection() ?: return@LaunchedEffect
        ranked = withContext(Dispatchers.Default) {
            try {
                rankFeedInput(core, rule, input, clock)
            } catch (_: Throwable) {
                emptyList()
            }
        }
    }

    // What is drawn. New results replace it at once near the top or on a
    // selection change; further down they wait behind "Show N new".
    val listState = rememberLazyListState()
    var shown by remember { mutableStateOf<List<RankedItem>?>(null) }
    var pending by remember { mutableStateOf<List<RankedItem>?>(null) }
    var shownSelection by remember { mutableStateOf<String?>(null) }
    // UX spec section 5, "New since you last looked": what this visit of the
    // shown feed compares against, read when the feed comes on screen and
    // left alone until it goes. `null` on a first visit (no divider).
    var visitBaseline by remember { mutableStateOf<Set<Long>?>(null) }
    val presetKeys = remember { FeedPresetKey.entries.map { it.name }.toSet() }
    // Every feed viewed during this visit, with what it showed when last on
    // screen. Written only when the person leaves the Feed screen (or the app
    // stops), so switching feeds inside the screen moves no baseline.
    val visitLooks = remember { LinkedHashMap<String, List<String>>() }
    fun noteLook(sel: String?, items: List<RankedItem>?) {
        if (sel == null || items == null || sel == DISCOVER) return
        visitLooks[sel] = items.map(RankedItem::key)
    }
    fun recordLooks(sel: String?, items: List<RankedItem>?) {
        noteLook(sel, items)
        if (visitLooks.isEmpty()) return
        val looks = visitLooks.toMap()
        val keep = presetKeys + saved.map { CUSTOM_PREFIX + it.id }
        updateReading { r ->
            var next = r
            for ((k, v) in looks) next = withLooked(next, k, v)
            pruneLooked(next, keep + looks.keys)
        }
    }
    LaunchedEffect(ranked) {
        val next = ranked ?: return@LaunchedEffect
        if (shown == null || shownSelection != selection || listState.isNearTop()) {
            if (shownSelection != selection) {
                // Leaving one feed for another: note what it showed (written
                // when the person leaves the Feed screen).
                noteLook(shownSelection, shown)
                visitBaseline = reading.looked[selection]?.toSet()
            }
            shownSelection = selection
            shown = next
            pending = null
        } else {
            pending = next
        }
    }
    // Leaving the Feed screen (or the app going to the background) records
    // the shown feed's look.
    val latestShown by rememberUpdatedState(shown)
    val latestShownSelection by rememberUpdatedState(shownSelection)
    val lookOwner = LocalLifecycleOwner.current
    DisposableEffect(lookOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) recordLooks(latestShownSelection, latestShown)
        }
        lookOwner.lifecycle.addObserver(observer)
        onDispose {
            lookOwner.lifecycle.removeObserver(observer)
            recordLooks(latestShownSelection, latestShown)
        }
    }
    LaunchedEffect(listState) {
        snapshotFlow { listState.isNearTop() }.collect { top ->
            if (top && pending != null) {
                shown = pending
                pending = null
            }
        }
    }
    val newCount = newItemCount(shown?.map { it.key }, pending?.map { it.key })
    val dividerIndex = remember(shown, visitBaseline) {
        shown?.let { items -> newDividerIndex(items.map { it.key }, visitBaseline) }
    }

    // ----- Seen (ADR-0063 section 5) ---------------------------------------

    fun seenStateOf(hex: String): GroupSeenState {
        val thread = chatState.threads.firstOrNull { it.groupIdHex == hex }
        val cursor = readCursorId(readCursors[hex]?.upToMessageId)
        val through = listOfNotNull(cursor, thread?.feedSeenThrough).maxOrNull()
        return GroupSeenState(through, thread?.feedSeen ?: emptySet())
    }

    fun wantsSeen(ref: FeedCandidateRef): Boolean = when (ref.source.kind) {
        FeedSourceKind.GROUP ->
            ref.message.direction == "incoming" &&
                !isSeen(seenStateOf(ref.source.idHex), ref.message.messageId, ref.message.direction)
        // UX spec section 5, "Channel posts": read once, wherever it is read.
        FeedSourceKind.CHANNEL -> isChannelPostUnread(
            readingFlow.value,
            ref.source.idHex,
            ref.message.messageId,
            own = ref.message.direction == "outgoing",
        )
    }

    val expanded = remember { mutableStateListOf<String>() }
    // The item whose menu is open; a reply row opens its parent's.
    var menuFor by remember { mutableStateOf<String?>(null) }
    val lifecycleOwner = LocalLifecycleOwner.current
    val pendingSeen = remember { HashMap<String, MutableSet<Long>>() }

    suspend fun flushSeen() {
        if (pendingSeen.isEmpty()) return
        val batch: Map<String, List<Long>> = pendingSeen.mapValues { it.value.toList() }
        pendingSeen.clear()
        val advanced = chatRepository.recordFeedSeen(
            seen = batch,
            readCursorThrough = { hex -> readCursorId(deviceGroupSync.readCursors.value[hex]?.upToMessageId) },
            contiguous = { ids, seen -> core.contiguousSeen(ids, seen) },
        )
        for ((hex, upTo) in advanced) {
            val thread = chatRepository.state.value.threads.firstOrNull { it.groupIdHex == hex }
            if (thread == null || thread.membershipEnded) continue
            // A read receipt wakes nobody (ADR-0024); the repository sends it
            // best-effort, as the chat does.
            scope.launch { chatRepository.sendReadReceipt(groupIdHex = hex, upTo = upTo) }
            val gid = hexToBytes(hex) ?: continue
            scope.launch {
                try {
                    deviceGroupSync.setReadCursor(
                        groupId = gid,
                        upToMessageId = readCursorBytes(upTo),
                        readAtMs = System.currentTimeMillis().toULong(),
                    )
                } catch (e: Throwable) {
                    android.util.Log.w("FeedScreen", "read cursor failed", e)
                }
            }
        }
    }

    // Dwell: every quarter second, the items fully on screen are timed; one on
    // screen for FEED_SEEN_DWELL_MS is queued, and the queue is filed shortly
    // after. Only while the screen is resumed, so a backgrounded app marks
    // nothing.
    LaunchedEffect(listState, lifecycleOwner) {
        lifecycleOwner.lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            val since = HashMap<String, Long>()
            // Queued once per visit; the flush makes `wantsSeen` false.
            val queued = HashSet<String>()
            var lastQueuedAt = 0L
            try {
                while (true) {
                    delay(SEEN_TICK_MS)
                    val now = android.os.SystemClock.elapsedRealtime()
                    val refsByKey = visibleRefs(shown, expanded)
                    val full = fullyVisibleKeys(listState)
                    since.keys.retainAll(full)
                    for (key in full) {
                        val ref = refsByKey[key] ?: continue
                        if (key in queued || !wantsSeen(ref)) continue
                        val start = since.getOrPut(key) { now }
                        if (now - start >= FEED_SEEN_DWELL_MS) {
                            queued += key
                            if (ref.source.kind == FeedSourceKind.CHANNEL) {
                                // A channel has no read receipt; its marker moves
                                // at once and never backwards.
                                updateReading { advanceChannelRead(it, ref.source.idHex, ref.message.messageId) }
                            } else {
                                pendingSeen.getOrPut(ref.source.idHex) { LinkedHashSet() } += ref.message.messageId
                                lastQueuedAt = now
                            }
                        }
                    }
                    if (pendingSeen.isNotEmpty() && now - lastQueuedAt >= FLUSH_DELAY_MS) {
                        try {
                            flushSeen()
                        } catch (e: Throwable) {
                            android.util.Log.w("FeedScreen", "marking seen failed", e)
                        }
                    }
                }
            } finally {
                // Leaving the screen files what was already queued.
                if (pendingSeen.isNotEmpty()) scope.launch { runCatching { flushSeen() } }
            }
        }
    }

    // ----- Editor ------------------------------------------------------------

    val edit = editing
    if (edit != null) {
        val initial = when (edit) {
            Editing.New -> blankDraft(core)
            is Editing.Existing -> saved.firstOrNull { it.id == edit.id }?.rule
        }
        if (initial != null) {
            FeedEditorScreen(
                core = core,
                initial = initial,
                isNew = edit == Editing.New,
                sources = allSources,
                preview = { draft ->
                    try {
                        rankFeedInput(core, draft, input, clock).take(PREVIEW_ITEMS).map {
                            PreviewLine(it.key, it.ref.source.title, bodyOf(it.ref, strings))
                        }
                    } catch (_: Throwable) {
                        null
                    }
                },
                error = editError,
                onSave = { draft ->
                    editError = null
                    scope.launch {
                        try {
                            val id = (edit as? Editing.Existing)?.id ?: newFeedId()
                            val ok = persistSaved(upsertFeed(savedState, SavedFeed(id, core.encode(draft))))
                            if (!ok) {
                                editError = UiText("feed.error.save")
                            } else {
                                editing = null
                                choose(CUSTOM_PREFIX + id)
                            }
                        } catch (_: Throwable) {
                            editError = UiText("feed.error.save")
                        }
                    }
                },
                onCancel = { editing = null },
                onDelete = (edit as? Editing.Existing)?.let { e ->
                    {
                        editError = null
                        scope.launch {
                            if (persistSaved(removeFeed(savedState, e.id))) {
                                editing = null
                                choose(FeedPresetKey.LATEST.name)
                            } else {
                                editError = UiText("feed.error.delete")
                            }
                        }
                        Unit
                    }
                },
            )
            return
        }
    }

    // ----- The feed ----------------------------------------------------------

    val zone = remember { ZoneId.systemDefault() }
    val locale = remember { Locale.getDefault() }
    val use24h = remember(context) { android.text.format.DateFormat.is24HourFormat(context) }

    fun authorOf(ref: FeedCandidateRef): String {
        if (ref.source.kind == FeedSourceKind.CHANNEL) return ref.source.title
        if (ref.message.direction == "outgoing") return strings["chat.author.you"]
        val key = ref.message.authorKeyB64 ?: return strings["chat.members.unnamed"]
        return directory.resolve(key).primary
    }

    fun open(ref: FeedCandidateRef) {
        if (ref.source.kind == FeedSourceKind.CHANNEL) {
            onOpenChannel(ref.source.idHex)
        } else {
            onOpenGroupMessage(ref.source.idHex, ref.message.messageId)
        }
    }

    // ----- The item menu (UX spec section 5) ---------------------------------

    val clipboard = LocalClipboardManager.current

    /** A toast for [TOAST_MS], replacing any one on screen. */
    suspend fun showToast(message: String, action: String?): SnackbarResult {
        snackbar.currentSnackbarData?.dismiss()
        return kotlinx.coroutines.withTimeoutOrNull(TOAST_MS) {
            snackbar.showSnackbar(message = message, actionLabel = action, duration = SnackbarDuration.Indefinite)
        } ?: run {
            snackbar.currentSnackbarData?.dismiss()
            SnackbarResult.Dismissed
        }
    }

    fun reply(ref: FeedCandidateRef) {
        if (ref.source.kind != FeedSourceKind.GROUP) return
        me.lakko.app.chat.PendingReplyArm.publish(ref.source.idHex, ref.message.messageId)
        onOpenGroupMessage(ref.source.idHex, ref.message.messageId)
    }

    fun forward(ref: FeedCandidateRef) {
        val request = if (ref.source.kind == FeedSourceKind.GROUP) {
            val message = chatState.threads
                .firstOrNull { it.groupIdHex == ref.source.idHex }
                ?.messages?.firstOrNull { it.messageId == ref.message.messageId }
            if (message == null) {
                scope.launch { showToast(strings["chat.forward.error.generic"], null) }
                return
            }
            me.lakko.app.chat.ForwardRequest(
                sourceGroupIdHex = ref.source.idHex,
                message = message,
                authorLabel = authorOf(ref),
            )
        } else {
            // A channel post is not a group message: it travels as its words
            // under the channel's name, with no origin to jump to.
            me.lakko.app.chat.ForwardRequest(
                sourceGroupIdHex = ref.source.idHex,
                message = ChatMessage(
                    messageId = 0L,
                    direction = Direction.Incoming,
                    text = ref.message.text,
                    createdAt = ref.message.createdAt,
                ),
                authorLabel = ref.source.title,
            )
        }
        me.lakko.app.chat.PendingForward.publish(request)
        onForward()
    }

    fun hide(ref: FeedCandidateRef) {
        val id = ref.source.idHex
        updateReading { hideFromFeeds(it, id) }
        scope.launch {
            if (showToast(strings["feed.hidden.toast"], strings["common.undo"]) == SnackbarResult.ActionPerformed) {
                updateReading { showInFeedsAgain(it, id) }
            }
        }
    }

    val itemMenu = ItemMenuActions(
        open = ::open,
        reply = ::reply,
        copy = { ref -> clipboard.setText(AnnotatedString(ref.message.text)) },
        forward = ::forward,
        hide = ::hide,
    )

    if (showingHidden) {
        HiddenFromFeedsScreen(
            hidden = hiddenSources(allSources, reading.hidden),
            onShowAgain = { id -> updateReading { showInFeedsAgain(it, id) } },
            onBack = { showingHidden = false },
        )
        return
    }

    // ----- Refresh (UX spec section 5, "Show N new") --------------------------

    var refreshing by remember { mutableStateOf(false) }
    // The latest group check and channel check, kept apart (UX spec state
    // table, "refresh failed (network)"); the banner is derived from both.
    // App-wide and per account: the session shell's receive loop and channel
    // post sync report into the same state as this screen's pull to refresh
    // and Retry.
    val feedRefresh = chatRepository.feedRefresh
    val refreshState by feedRefresh.state.collectAsStateWithLifecycle()
    val refreshFailed = me.lakko.app.feed.feedRefreshFailed(refreshState)
    // Shares the session shell's single-writer lock, so a refresh waits for a
    // sync already running instead of writing the log beside it.
    val refreshChannelSync = remember(channels, channelsClient) {
        me.lakko.app.feed.ChannelPostSync(context.applicationContext, channels, channelsClient)
    }
    fun refresh() {
        scope.launch {
            refreshing = true
            try {
                // Both checks run (Retry included); each folds into its own
                // latest outcome. A failed poll counts as not answered.
                val refreshAccount = chatRepository.currentAccountIdB64()
                val groups = try {
                    chatRepository.pollAllGroupsReporting()
                } catch (e: kotlin.coroutines.cancellation.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    me.lakko.app.feed.FAILED_CHECK
                }
                feedRefresh.record(refreshAccount, me.lakko.app.feed.CheckKind.GROUPS, groups)
                val channelCheck = refreshChannelSync.syncAndWait()
                feedRefresh.record(refreshAccount, me.lakko.app.feed.CheckKind.CHANNELS, channelCheck)
            } finally {
                refreshing = false
            }
            // Lands at the top, with whatever was waiting in place.
            pending?.let { shown = it }
            pending = null
            listState.scrollToItem(0)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("feed.title")) },
                actions = {
                    Box {
                        IconButton(onClick = { feedMenuOpen = true }) {
                            Icon(Icons.Filled.MoreVert, contentDescription = t("feed.menu.a11y"))
                        }
                        DropdownMenu(expanded = feedMenuOpen, onDismissRequest = { feedMenuOpen = false }) {
                            DropdownMenuItem(
                                text = { Text(t("feed.new")) },
                                onClick = {
                                    feedMenuOpen = false
                                    editError = null
                                    editing = Editing.New
                                },
                            )
                            DropdownMenuItem(
                                text = { Text(t("feed.hidden.title")) },
                                onClick = {
                                    feedMenuOpen = false
                                    showingHidden = true
                                },
                            )
                        }
                    }
                },
            )
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding),
        ) {
            // The switcher: presets, saved feeds, New feed. Each chip shows its
            // icon before the name (UX spec section 4.1).
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .horizontalScroll(rememberScrollState())
                    .padding(horizontal = 12.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                for (p in FeedPresetKey.entries) {
                    FilterChip(
                        selected = selection == p.name,
                        onClick = { choose(p.name) },
                        label = { Text(p.label.text()) },
                        leadingIcon = { ChipIcon(p.vector()) },
                    )
                }
                FilterChip(
                    selected = selection == DISCOVER,
                    onClick = { choose(DISCOVER) },
                    label = { Text(t("feed.preset.discover")) },
                    leadingIcon = { ChipIcon(DiscoverIcon) },
                )
                for (v in saved) {
                    FilterChip(
                        selected = selection == CUSTOM_PREFIX + v.id,
                        onClick = { choose(CUSTOM_PREFIX + v.id) },
                        label = { Text(v.rule.name) },
                        leadingIcon = { ChipIcon(FeedIcon.of(v.rule.icon).vector()) },
                    )
                }
                TextButton(onClick = {
                    editError = null
                    editing = Editing.New
                }) { Text(t("feed.new")) }
            }
            if (selectedSaved != null) {
                Row(modifier = Modifier.padding(horizontal = 8.dp)) {
                    TextButton(onClick = {
                        editError = null
                        editing = Editing.Existing(selectedSaved.id)
                    }) { Text(t("feed.edit")) }
                    TextButton(onClick = { sharing = selectedSaved }) { Text(t("feed.share")) }
                }
            }

            if (selection == DISCOVER) {
                DiscoverView(
                    model = discover,
                    cards = discoverCards,
                    onSubscribe = onSubscribeChannel,
                    relationOf = relationOf,
                    onOpenChannel = onOpenChannel,
                )
            } else {
            if (!bannerDismissed && input.candidates.isNotEmpty()) {
                Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 12.dp, vertical = 4.dp),
                    shape = MaterialTheme.shapes.medium,
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Icon(Icons.Outlined.Info, contentDescription = null)
                        Spacer(Modifier.width(8.dp))
                        Text(
                            t("feed.banner.local"),
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = {
                            bannerDismissed = true
                            FeedPrefs.dismissBanner(context)
                        }) { Text(t("feed.banner.dismiss")) }
                    }
                }
            }
            if (refreshFailed) {
                Surface(
                    color = MaterialTheme.colorScheme.errorContainer,
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(horizontal = 12.dp, vertical = 4.dp),
                    shape = MaterialTheme.shapes.medium,
                ) {
                    Row(
                        modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
                        verticalAlignment = Alignment.CenterVertically,
                    ) {
                        Text(
                            t("feed.refresh_failed"),
                            style = MaterialTheme.typography.bodySmall,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(onClick = { refresh() }) { Text(t("common.retry")) }
                    }
                }
            }

            val items = shown
            PullToRefreshBox(
                isRefreshing = refreshing,
                onRefresh = { refresh() },
                modifier = Modifier.fillMaxSize(),
            ) {
            when {
                items == null -> Box(
                    modifier = Modifier.fillMaxSize(),
                    contentAlignment = Alignment.Center,
                ) {
                    Text(
                        t("common.loading"),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                items.isEmpty() -> EmptyFeed(
                    hasSources = allSources.isNotEmpty(),
                    hasCandidates = input.candidates.isNotEmpty(),
                    onDiscover = { choose(DISCOVER) },
                    onEditFeed = selectedSaved?.let { v ->
                        {
                            editError = null
                            editing = Editing.Existing(v.id)
                        }
                    },
                )
                else -> LazyColumn(
                    state = listState,
                    modifier = Modifier.fillMaxSize(),
                    contentPadding = PaddingValues(vertical = 4.dp),
                ) {
                    // A saved feed that asks for suggestions shows a channel card
                    // after some items (ADR-0059, UX spec section 4.1).
                    val cardAfter = me.lakko.app.discover.cardsAfterItems(items.size, discoverCards.size, suggestionRate)
                    for ((index, item) in items.withIndex()) {
                        val key = item.key
                        if (index == dividerIndex) {
                            item(key = NEW_DIVIDER_KEY) { NewDivider() }
                        }
                        item(key = key) {
                            FeedItemCard(
                                item = item,
                                why = whyLine(item.ref, whyPreset)?.text(),
                                author = authorOf(item.ref),
                                time = parseTime(item.ref)?.let {
                                    formatListTime(it, clock, zone, use24h, locale)
                                }.orEmpty(),
                                expanded = key in expanded,
                                lastReplyLine = item.replies.lastOrNull()?.let {
                                    strings.format("chats.preview.sender", mapOf("name" to authorOf(it), "text" to bodyOf(it, strings)))
                                },
                                onOpen = { open(item.ref) },
                                onToggleReplies = {
                                    if (key in expanded) expanded.remove(key) else expanded.add(key)
                                },
                                menu = itemMenu,
                                menuOpen = menuFor == key,
                                onMenuOpenChange = { open -> menuFor = if (open) key else null },
                            )
                        }
                        if (key in expanded) {
                            for (reply in item.replies) {
                                item(key = REPLY_PREFIX + itemKey(reply)) {
                                    ReplyRow(
                                        author = authorOf(reply),
                                        text = bodyOf(reply, strings),
                                        onOpen = { open(reply) },
                                        // A reply belongs to its item: long-press
                                        // opens the parent's menu.
                                        onLongPress = { menuFor = key },
                                    )
                                }
                            }
                        }
                        item(key = "$key/divider") { HorizontalDivider() }
                        cardAfter[index]?.let { discoverCards.getOrNull(it) }?.let { card ->
                            item(key = "$key/suggestion:${card.channelIdHex}") {
                                Box(Modifier.padding(horizontal = 12.dp)) {
                                    DiscoverCardView(
                                        card = card,
                                        model = discover,
                                        onSubscribe = onSubscribeChannel,
                                        relation = relationOf(card.channelIdHex),
                                        onOpenChannel = onOpenChannel,
                                    )
                                }
                            }
                        }
                    }
                    item(key = "end") {
                        Text(
                            t("feed.end"),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(24.dp),
                        )
                    }
                }
            }
            // The pill sits over the list, so new items never shift it while
            // the person is scrolled away from the top.
            if (newCount > 0) {
                Button(
                    onClick = {
                        pending?.let { shown = it }
                        pending = null
                        scope.launch { listState.scrollToItem(0) }
                    },
                    modifier = Modifier
                        .align(Alignment.TopCenter)
                        .padding(top = 8.dp),
                ) { Text(t("feed.show_new", emptyMap(), mapOf("count" to newCount.toULong()))) }
            }
            }
            }
        }
    }

    sharing?.let { v -> ShareFeedDialog(context, core, v, onClose = { sharing = null }) }
}

// ============================================================================
// Pieces
// ============================================================================

private const val CUSTOM_PREFIX = "custom:"

/** The Discover chip's selection key (ADR-0059). */
private const val DISCOVER = "DISCOVER"
private const val REPLY_PREFIX = "reply:"
private const val NEW_DIVIDER_KEY = "new-divider"
private const val TOAST_MS = 5_000L
private const val PREVIEW_ITEMS = 8
private const val SEEN_TICK_MS = 250L
private const val FLUSH_DELAY_MS = 800L

/** Within this many pixels of the top, new items are shown at once. */
private const val TOP_SLACK_PX = 40

internal class SavedView(val id: String, val rule: FeedRule)

private sealed interface Editing {
    data object New : Editing
    data class Existing(val id: String) : Editing
}

private fun initialSelection(stored: String?): String {
    if (stored != null && stored.startsWith(CUSTOM_PREFIX)) return stored
    if (stored == DISCOVER) return DISCOVER
    return FeedPresetKey.entries.firstOrNull { it.name == stored }?.name ?: FeedPresetKey.LATEST.name
}

private fun presetOf(selection: String): FeedPresetKey? = FeedPresetKey.entries.firstOrNull { it.name == selection }

private fun LazyListState.isNearTop(): Boolean =
    firstVisibleItemIndex == 0 && firstVisibleItemScrollOffset <= TOP_SLACK_PX

/** The refs the list may be showing, by lazy key. */
private fun visibleRefs(shown: List<RankedItem>?, expanded: List<String>): Map<String, FeedCandidateRef> {
    val out = HashMap<String, FeedCandidateRef>()
    for (item in shown.orEmpty()) {
        out[item.key] = item.ref
        if (item.key in expanded) for (r in item.replies) out[REPLY_PREFIX + itemKey(r)] = r
    }
    return out
}

/** Keys of lazy items fully on screen, or filling the screen when taller. */
private fun fullyVisibleKeys(state: LazyListState): Set<String> {
    val info = state.layoutInfo
    val start = info.viewportStartOffset
    val end = info.viewportEndOffset
    val viewport = (end - start).coerceAtLeast(1)
    val out = HashSet<String>()
    for (it in info.visibleItemsInfo) {
        val key = it.key as? String ?: continue
        val visible = minOf(end, it.offset + it.size) - maxOf(start, it.offset)
        if (visible >= it.size * 0.99 || visible >= viewport * 0.9) out += key
    }
    return out
}

private fun parseTime(ref: FeedCandidateRef): Long? = me.lakko.app.feed.parseFeedTimeMs(ref.message.createdAt)

internal fun feedMessageOf(m: ChatMessage): FeedMessage = FeedMessage(
    messageId = m.messageId,
    direction = when (m.direction) {
        Direction.Incoming -> "incoming"
        Direction.Outgoing -> "outgoing"
        Direction.System -> "system"
    },
    text = m.text,
    attachments = m.attachments.map { FeedAttachment(it.kind, it.mime) },
    createdAt = m.createdAt,
    authorKeyB64 = m.authorKey,
    edited = m.edited,
    deleted = m.deleted,
    reactions = m.reactions.map { FeedReaction(it.reactor, it.emoji) },
    replyTarget = m.reply?.target,
    forwardAuthor = m.forward?.author,
)

private fun hexToBytes(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[i * 2], 16)
        val lo = Character.digit(hex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}

@Composable
private fun EmptyFeed(
    hasSources: Boolean,
    hasCandidates: Boolean,
    onDiscover: () -> Unit,
    onEditFeed: (() -> Unit)?,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        val muted = MaterialTheme.colorScheme.onSurfaceVariant
        when {
            !hasSources -> {
                Text(t("feed.empty.no_sources"), color = muted)
                Spacer(Modifier.height(12.dp))
                Button(onClick = onDiscover) { Text(t("feed.empty.discover")) }
            }
            !hasCandidates -> Text(
                t("feed.empty.nothing_yet"),
                color = muted,
            )
            else -> {
                Text(t("feed.empty.no_match"), color = muted)
                if (onEditFeed != null) {
                    Spacer(Modifier.height(12.dp))
                    Button(onClick = onEditFeed) { Text(t("feed.edit_feed")) }
                }
            }
        }
    }
}

/** What the item menu can do (UX spec section 5). */
private class ItemMenuActions(
    val open: (FeedCandidateRef) -> Unit,
    val reply: (FeedCandidateRef) -> Unit,
    val copy: (FeedCandidateRef) -> Unit,
    val forward: (FeedCandidateRef) -> Unit,
    val hide: (FeedCandidateRef) -> Unit,
)

@Composable
private fun ChipIcon(vector: androidx.compose.ui.graphics.vector.ImageVector) {
    Icon(vector, contentDescription = null, modifier = Modifier.size(18.dp))
}

/** UX spec section 5 — the thin "New" divider above the first item the feed
 *  did not show when the person last left it. */
@Composable
private fun NewDivider() {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        HorizontalDivider(modifier = Modifier.weight(1f), color = MaterialTheme.colorScheme.primary)
        Text(
            t("feed.divider.new"),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(horizontal = 8.dp),
        )
        HorizontalDivider(modifier = Modifier.weight(1f), color = MaterialTheme.colorScheme.primary)
    }
}

/** UX spec section 5 — the conversations hidden from feeds on this device,
 *  each with Show again. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun HiddenFromFeedsScreen(
    hidden: List<FeedSource>,
    onShowAgain: (idHex: String) -> Unit,
    onBack: () -> Unit,
) {
    androidx.activity.compose.BackHandler(onBack = onBack)
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(t("feed.hidden.title")) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = t("common.back"))
                    }
                },
            )
        },
    ) { padding ->
        if (hidden.isEmpty()) {
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .padding(24.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    t("feed.hidden.empty"),
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            LazyColumn(
                modifier = Modifier
                    .fillMaxSize()
                    .padding(padding),
            ) {
                for (s in hidden) {
                    item(key = s.idHex) {
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 6.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                s.title,
                                style = MaterialTheme.typography.bodyLarge,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                                modifier = Modifier.weight(1f),
                            )
                            TextButton(onClick = { onShowAgain(s.idHex) }) { Text(t("feed.hidden.show_again")) }
                        }
                    }
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun FeedItemCard(
    item: RankedItem,
    why: String?,
    author: String,
    time: String,
    expanded: Boolean,
    lastReplyLine: String?,
    onOpen: () -> Unit,
    onToggleReplies: () -> Unit,
    menu: ItemMenuActions,
    menuOpen: Boolean,
    onMenuOpenChange: (Boolean) -> Unit,
) {
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    val ref = item.ref
    val closeMenu = { onMenuOpenChange(false) }
    Column(modifier = Modifier.fillMaxWidth()) {
        Box {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .combinedClickable(onClick = onOpen, onLongClick = { onMenuOpenChange(true) })
                .padding(horizontal = 16.dp, vertical = 10.dp),
        ) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    ref.source.title,
                    style = MaterialTheme.typography.labelMedium,
                    color = muted,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f),
                )
                Text(time, style = MaterialTheme.typography.labelSmall, color = muted)
            }
            // A channel speaks as itself: its name is already the source.
            if (ref.source.kind == FeedSourceKind.GROUP) {
                Text(author, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            }
            ref.message.forwardAuthor?.let {
                Text(t("chat.forward.from", mapOf("name" to it)), style = MaterialTheme.typography.labelSmall, color = muted)
            }
            Text(
                bodyOf(ref, LocalStrings.current) + if (ref.message.edited) " " + t("chat.message.edited") else "",
                style = MaterialTheme.typography.bodyMedium,
            )
            why?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = muted) }
        }
        // UX spec section 5 — Open, Reply (group messages only), Copy,
        // Forward, Hide from feeds.
        DropdownMenu(expanded = menuOpen, onDismissRequest = closeMenu) {
            DropdownMenuItem(text = { Text(t("common.open")) }, onClick = {
                closeMenu()
                menu.open(ref)
            })
            if (ref.source.kind == FeedSourceKind.GROUP) {
                DropdownMenuItem(text = { Text(t("chat.message.reply")) }, onClick = {
                    closeMenu()
                    menu.reply(ref)
                })
            }
            DropdownMenuItem(
                text = { Text(t("common.copy")) },
                enabled = ref.message.text.isNotBlank(),
                onClick = {
                    closeMenu()
                    menu.copy(ref)
                },
            )
            DropdownMenuItem(text = { Text(t("chat.message.forward")) }, onClick = {
                closeMenu()
                menu.forward(ref)
            })
            DropdownMenuItem(text = { Text(t("feed.action.hide")) }, onClick = {
                closeMenu()
                menu.hide(ref)
            })
        }
        }
        val reactions = reactionSummary(ref.message)
        if (reactions.isNotEmpty()) {
            Row(
                modifier = Modifier.padding(horizontal = 16.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                for (r in reactions) {
                    Text("${r.emoji} ${r.count}", style = MaterialTheme.typography.labelMedium)
                }
            }
        }
        if (item.replies.isNotEmpty()) {
            Column(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable(onClick = onToggleReplies)
                    .padding(horizontal = 16.dp, vertical = 6.dp),
            ) {
                Text(
                    repliesLabel(item.replies.size).text(),
                    style = MaterialTheme.typography.labelMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
                if (!expanded && lastReplyLine != null) {
                    Text(
                        lastReplyLine,
                        style = MaterialTheme.typography.bodySmall,
                        color = muted,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun ReplyRow(author: String, text: String, onOpen: () -> Unit, onLongPress: () -> Unit) {
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(onClick = onOpen, onLongClick = onLongPress)
            .padding(start = 32.dp, end = 16.dp, top = 4.dp, bottom = 4.dp),
    ) {
        Text(author, style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
        Text(text, style = MaterialTheme.typography.bodySmall)
    }
}

@Composable
private fun ShareFeedDialog(context: Context, core: FeedCore, view: SavedView, onClose: () -> Unit) {
    val bytes = remember(view) {
        try {
            exportDraft(core, view.rule)
        } catch (_: Throwable) {
            null
        }
    }
    val link = bytes?.let { shareLink(ApiEndpoints.webOrigin, it) }
    val clipboard = LocalClipboardManager.current
    var copied by remember { mutableStateOf(false) }
    AlertDialog(
        onDismissRequest = onClose,
        title = { Text(t("feed.share.title", mapOf("name" to view.rule.name))) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                if (link == null) {
                    Text(t("feed.share.unshareable"), color = MaterialTheme.colorScheme.error)
                } else {
                    Text(
                        t("feed.share.stripped"),
                        style = MaterialTheme.typography.bodySmall,
                    )
                    SelectionContainer { Text(link, style = MaterialTheme.typography.bodySmall) }
                    Row {
                        TextButton(onClick = {
                            clipboard.setText(AnnotatedString(link))
                            copied = true
                        }) { Text(if (copied) t("common.copied") else t("chat.invite_link.copy")) }
                        TextButton(onClick = { shareFile(context, view.rule, bytes) }) { Text(t("feed.share.share_file")) }
                    }
                }
            }
        },
        confirmButton = { TextButton(onClick = onClose) { Text(t("common.close")) } },
    )
}

/** Hand the `.lakkofeed` file to the system share sheet through the app's
 *  FileProvider (the attachment share directory). */
private fun shareFile(context: Context, rule: FeedRule, bytes: ByteArray) {
    runCatching {
        val dir = java.io.File(
            java.io.File(context.cacheDir, "shared-attachments"),
            java.util.UUID.randomUUID().toString(),
        ).apply { mkdirs() }
        val out = java.io.File(dir, shareFileName(rule))
        out.writeBytes(bytes)
        val uri = androidx.core.content.FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", out)
        val send = Intent(Intent.ACTION_SEND).apply {
            putExtra(Intent.EXTRA_STREAM, uri)
            type = "application/octet-stream"
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        }
        context.startActivity(Intent.createChooser(send, me.lakko.app.l10n.Localization.strings.value["feed.share.a11y"]).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }.onFailure { android.util.Log.w("FeedScreen", "share file failed", it) }
}
