// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

@file:OptIn(
    androidx.compose.material3.ExperimentalMaterial3Api::class,
    androidx.compose.foundation.ExperimentalFoundationApi::class,
)

package me.lakko.app.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyGridState
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.lazy.grid.rememberLazyGridState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.t
import me.lakko.app.media.EmojiDataset
import me.lakko.app.media.EmojiEntry
import me.lakko.app.media.GIF_ATTRIBUTION
import me.lakko.app.media.GIF_NO_RESULTS
import me.lakko.app.media.GIF_RATE_LIMITED
import me.lakko.app.media.GIF_UNAVAILABLE
import me.lakko.app.media.GifItem
import me.lakko.app.media.GifPickerModel
import me.lakko.app.media.MediaPrefs
import me.lakko.app.media.displayEmoji
import me.lakko.app.media.isSingleEmojiGrapheme
import me.lakko.app.media.pushRecent
import me.lakko.app.media.searchEmoji
import me.lakko.app.media.withTonePref

/** Which picker: the composer's (Emoji + GIF, keeps open) or the reaction bar's
 *  (Emoji only, a tap reacts and closes). */
internal enum class PickerMode { COMPOSER, REACTION }

/** One glyph per category for the jump strip, in dataset group order. */
private val GROUP_ICONS = listOf(
    "😀", "👋", "🐻", "🍔", "✈️",
    "⚽", "💡", "❤️", "🏁",
)

/**
 * UX spec §4 / §5 — one picker, two tabs, as a modal bottom sheet.
 *
 * Tapping an emoji inserts it (composer mode, sheet stays open so several can
 * be added) or reacts and closes (reaction mode, GIF tab hidden). Emoji data is
 * bundled; the GIF tab only talks to Lakko's proxy, and says so once before the
 * first search.
 */
@Composable
internal fun EmojiPickerSheet(
    mode: PickerMode,
    dataset: EmojiDataset?,
    prefs: MediaPrefs,
    gifModel: GifPickerModel?,
    loadGifPreview: suspend (String) -> ByteArray?,
    onEmoji: (String) -> Unit,
    onGif: (GifItem) -> Unit,
    onDismiss: () -> Unit,
    /** reactions-and-emoji section 5 — `Some reactions`: only these, as one
     *  grid in the owner's order, search kept, no categories. */
    only: List<String>? = null,
    /** reactions-and-emoji section 10 — the person holds 3 here: only these
     *  (their own) stay pressable; the rest are dimmed, with the line. */
    capOwn: Set<String>? = null,
) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val gifState = gifModel?.state?.collectAsStateWithLifecycle()?.value
    val gifAvailable = mode == PickerMode.COMPOSER && gifModel != null &&
        gifState?.phase != GifPickerModel.Phase.HIDDEN
    var tab by remember { mutableStateOf(0) }
    if (!gifAvailable && tab == 1) tab = 0

    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        val sheetLabel = t(
            when {
                mode == PickerMode.REACTION -> "chat.emoji.choose_reaction"
                // No GIF tab (the call chat, a server with no GIF provider):
                // the sheet is named for what it holds.
                !gifAvailable -> "chat.emoji.tab"
                else -> "chat.composer.emoji_gifs"
            },
        )
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .height(460.dp)
                .semantics { contentDescription = sheetLabel },
        ) {
            if (gifAvailable) {
                EmojiGifTabs(selected = tab, onSelect = { tab = it })
            }
            if (tab == 0 || !gifAvailable) {
                if (capOwn != null) {
                    Text(
                        t("chat.reaction.cap_reached"),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 4.dp),
                    )
                }
                EmojiTab(
                    dataset = dataset,
                    prefs = prefs,
                    only = only,
                    capOwn = capOwn,
                    onEmoji = { emoji ->
                        // A reaction is exactly one emoji grapheme; the picker
                        // only produces those, and the check keeps it that way.
                        if (!isSingleEmojiGrapheme(emoji)) return@EmojiTab
                        prefs.recentEmoji = pushRecent(prefs.recentEmoji, emoji)
                        onEmoji(emoji)
                        if (mode == PickerMode.REACTION) onDismiss()
                    },
                )
            } else if (gifModel != null) {
                LaunchedEffect(gifModel) { gifModel.open() }
                GifTab(gifModel, loadGifPreview, onGif = { item ->
                    onGif(item)
                    onDismiss()
                })
            }
        }
    }
}

/**
 * The picker's two tabs. Each is named by what it shows ("Emoji", "GIF"),
 * never by the sheet's own label, and carries the tab role and its selected
 * state, so TalkBack reads "Emoji, tab, selected" and "GIF, tab".
 */
@Composable
internal fun EmojiGifTabs(selected: Int, onSelect: (Int) -> Unit) {
    TabRow(selectedTabIndex = selected) {
        Tab(selected = selected == 0, onClick = { onSelect(0) }, text = { Text(t("chat.emoji.tab")) })
        Tab(selected = selected == 1, onClick = { onSelect(1) }, text = { Text(t("chat.media.gif")) })
    }
}

private sealed interface EmojiGridItem {
    data class Header(val title: String) : EmojiGridItem

    data class Glyph(val entry: EmojiEntry?, val glyph: String, val keySuffix: String) : EmojiGridItem
}

@Composable
private fun EmojiTab(
    dataset: EmojiDataset?,
    prefs: MediaPrefs,
    onEmoji: (String) -> Unit,
    only: List<String>? = null,
    capOwn: Set<String>? = null,
) {
    var query by remember { mutableStateOf("") }
    var tones by remember { mutableStateOf(prefs.tonePrefs) }
    var recent by remember { mutableStateOf(prefs.recentEmoji) }
    val gridState = rememberLazyGridState()
    val scope = rememberCoroutineScope()

    if (dataset == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        return
    }

    val results = remember(query, dataset) { searchEmoji(dataset, query) }
    val strings = LocalStrings.current
    val (items, headerIndex) = remember(dataset, recent, tones, query, strings, only) {
        val list = ArrayList<EmojiGridItem>()
        val headers = HashMap<Int, Int>()
        if (only != null) {
            val found = if (query.isBlank()) null else results.map { it.emoji }.toSet()
            only.filter { found == null || it in found }
                .forEach { list.add(EmojiGridItem.Glyph(dataset.byEmoji[it], it, "a")) }
        } else if (query.isBlank()) {
            if (recent.isNotEmpty()) {
                list.add(EmojiGridItem.Header(strings["chat.emoji.recent"]))
                recent.forEach { list.add(EmojiGridItem.Glyph(dataset.byEmoji[it], it, "recent")) }
            }
            dataset.groups.forEachIndexed { gi, dataName ->
            val name = emojiGroupName(gi, dataName, strings)
                headers[gi] = list.size
                list.add(EmojiGridItem.Header(emojiGroupName(gi, name, strings)))
                dataset.entriesInGroup(gi).forEach { e ->
                    list.add(EmojiGridItem.Glyph(e, displayEmoji(e, tones), "g"))
                }
            }
        } else {
            results.forEach { e -> list.add(EmojiGridItem.Glyph(e, displayEmoji(e, tones), "s")) }
        }
        list to headers
    }

    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            placeholder = { Text(t("chat.emoji.search")) },
            singleLine = true,
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 6.dp)
                .semantics { contentDescription = strings["chat.emoji.search"] },
        )
        if (query.isNotBlank() && results.isEmpty()) {
            Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
                Text(t("chat.emoji.no_results"), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        } else {
            LazyVerticalGrid(
                columns = GridCells.Adaptive(44.dp),
                state = gridState,
                modifier = Modifier.fillMaxWidth().weight(1f).padding(horizontal = 8.dp),
            ) {
                items.forEachIndexed { index, item ->
                    when (item) {
                        is EmojiGridItem.Header -> item(
                            key = "h-$index",
                            span = { GridItemSpan(maxLineSpan) },
                        ) {
                            Text(
                                item.title,
                                style = MaterialTheme.typography.labelMedium,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.padding(horizontal = 4.dp, vertical = 6.dp),
                            )
                        }
                        is EmojiGridItem.Glyph -> item(key = "e-$index-${item.keySuffix}") {
                            EmojiCell(
                                item = item,
                                enabled = capOwn == null || item.glyph in capOwn,
                                onPick = { glyph ->
                                    onEmoji(glyph)
                                    recent = prefs.recentEmoji
                                },
                                onTone = { entry, variant ->
                                    tones = withTonePref(tones, entry, variant)
                                    prefs.tonePrefs = tones
                                    onEmoji(variant)
                                    recent = prefs.recentEmoji
                                },
                            )
                        }
                    }
                }
            }
            if (query.isBlank()) {
                CategoryStrip(dataset, gridState) { gi ->
                    headerIndex[gi]?.let { idx -> scope.launch { gridState.scrollToItem(idx) } }
                }
            }
        }
    }
}

@Composable
private fun EmojiCell(
    item: EmojiGridItem.Glyph,
    enabled: Boolean = true,
    onPick: (String) -> Unit,
    onTone: (EmojiEntry, String) -> Unit,
) {
    var toneMenu by remember { mutableStateOf(false) }
    val entry = item.entry
    Box(
        contentAlignment = Alignment.Center,
        modifier = Modifier
            .size(44.dp)
            .clip(RoundedCornerShape(8.dp))
            .alpha(if (enabled) 1f else 0.35f)
            .combinedClickable(
                enabled = enabled,
                onClickLabel = t("chat.emoji.insert"),
                onLongClickLabel = if (entry?.tones != null) {
                    t("chat.emoji.skin_tones", mapOf("name" to (entry?.name?.ifEmpty { null } ?: item.glyph)))
                } else {
                    null
                },
                onClick = { onPick(item.glyph) },
                onLongClick = { if (entry?.tones != null) toneMenu = true },
            )
            .semantics { contentDescription = entry?.name ?: item.glyph },
    ) {
        Text(item.glyph, fontSize = 26.sp)
        val tones = entry?.tones
        if (toneMenu && entry != null && tones != null) {
            DropdownMenu(expanded = true, onDismissRequest = { toneMenu = false }) {
                Row(Modifier.padding(horizontal = 6.dp)) {
                    (listOf(entry.emoji) + tones).forEach { variant ->
                        Box(
                            contentAlignment = Alignment.Center,
                            modifier = Modifier
                                .size(44.dp)
                                .clickable(onClickLabel = t("chat.emoji.choose")) {
                                    toneMenu = false
                                    onTone(entry, variant)
                                },
                        ) {
                            Text(variant, fontSize = 26.sp)
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun CategoryStrip(dataset: EmojiDataset, gridState: LazyGridState, onJump: (Int) -> Unit) {
    val strings = LocalStrings.current
    Row(
        horizontalArrangement = Arrangement.SpaceBetween,
        modifier = Modifier
            .fillMaxWidth()
            .horizontalScroll(rememberScrollState())
            .padding(horizontal = 8.dp, vertical = 4.dp),
    ) {
        dataset.groups.forEachIndexed { gi, dataName ->
            val name = emojiGroupName(gi, dataName, strings)
            Box(
                contentAlignment = Alignment.Center,
                modifier = Modifier
                    .size(40.dp)
                    .clip(RoundedCornerShape(8.dp))
                    .clickable(onClickLabel = name) { onJump(gi) }
                    .semantics { contentDescription = name },
            ) {
                Text(GROUP_ICONS.getOrElse(gi) { "•" }, fontSize = 20.sp)
            }
        }
    }
}

@Composable
private fun GifTab(
    model: GifPickerModel,
    loadPreview: suspend (String) -> ByteArray?,
    onGif: (GifItem) -> Unit,
) {
    val state by model.state.collectAsStateWithLifecycle()
    val gridState = rememberLazyGridState()
    val searchGifLabel = t("chat.gif.search")
    val loadingGifsLabel = t("chat.gif.loading")

    Column(Modifier.fillMaxSize()) {
        OutlinedTextField(
            value = state.query,
            onValueChange = model::onQueryChange,
            placeholder = { Text(t("chat.gif.search")) },
            singleLine = true,
            keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
            keyboardActions = KeyboardActions(onSearch = { model.submit() }),
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = 12.dp, vertical = 6.dp)
                .semantics { contentDescription = searchGifLabel },
        )
        Box(Modifier.fillMaxWidth().weight(1f)) {
            when (state.phase) {
                GifPickerModel.Phase.LOADING -> LazyVerticalGrid(
                    columns = GridCells.Fixed(2),
                    modifier = Modifier.fillMaxSize().padding(horizontal = 8.dp)
                        .semantics { contentDescription = loadingGifsLabel },
                ) {
                    items(6) { PlaceholderTile() }
                }
                GifPickerModel.Phase.EMPTY -> CenterText(t(GIF_NO_RESULTS))
                GifPickerModel.Phase.RATE_LIMITED -> CenterText(t(GIF_RATE_LIMITED))
                GifPickerModel.Phase.UNAVAILABLE -> Column(
                    horizontalAlignment = Alignment.CenterHorizontally,
                    verticalArrangement = Arrangement.Center,
                    modifier = Modifier.fillMaxSize(),
                ) {
                    Text(t(GIF_UNAVAILABLE), color = MaterialTheme.colorScheme.onSurfaceVariant)
                    TextButton(onClick = model::retry) { Text(t("common.retry")) }
                }
                GifPickerModel.Phase.HIDDEN -> Unit
                GifPickerModel.Phase.READY -> {
                    val nearEnd by remember {
                        derivedStateOf {
                            val last = gridState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: 0
                            last >= gridState.layoutInfo.totalItemsCount - 4
                        }
                    }
                    LaunchedEffect(nearEnd, state.items.size) {
                        if (nearEnd) model.loadMore()
                    }
                    LazyVerticalGrid(
                        columns = GridCells.Fixed(2),
                        state = gridState,
                        modifier = Modifier.fillMaxSize().padding(horizontal = 8.dp),
                    ) {
                        items(state.items, key = { it.id }) { item ->
                            GifTile(item, loadPreview, onGif)
                        }
                        if (state.loadingMore) {
                            item(span = { GridItemSpan(maxLineSpan) }) {
                                Box(Modifier.fillMaxWidth().padding(8.dp), contentAlignment = Alignment.Center) {
                                    CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp)
                                }
                            }
                        }
                    }
                }
            }
        }
        Text(
            t(GIF_ATTRIBUTION),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.align(Alignment.CenterHorizontally).padding(6.dp),
        )
    }
}

/**
 * The emoji category names, in the order of the bundled data's groups
 * (Unicode's own order); an unknown group falls back to the data's name.
 * Emoji names themselves stay English (batch 3).
 */
private val EMOJI_GROUP_KEYS: List<String> = listOf(
    "chat.emoji.group.smileys",
    "chat.emoji.group.people",
    "chat.emoji.group.animals",
    "chat.emoji.group.food",
    "chat.emoji.group.travel",
    "chat.emoji.group.activities",
    "chat.emoji.group.objects",
    "chat.emoji.group.symbols",
    "chat.emoji.group.flags",
)

private fun emojiGroupName(index: Int, fallback: String, strings: Strings): String =
    EMOJI_GROUP_KEYS.getOrNull(index)?.let { strings[it] } ?: fallback

@Composable
private fun CenterText(text: String) {
    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        Text(text, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
}

@Composable
private fun PlaceholderTile() {
    Box(
        Modifier
            .padding(4.dp)
            .aspectRatio(1.3f)
            .clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant),
    )
}

@Composable
private fun GifTile(item: GifItem, loadPreview: suspend (String) -> ByteArray?, onGif: (GifItem) -> Unit) {
    var bytes by remember(item.id) { mutableStateOf<ByteArray?>(null) }
    LaunchedEffect(item.id) { bytes = loadPreview(item.previewUrl) }
    val ratio = (item.width.toFloat() / item.height).coerceIn(0.5f, 2f)
    val sendLabel = t("chat.gif.send")
    Box(
        modifier = Modifier
            .padding(4.dp)
            .aspectRatio(ratio)
            .clip(RoundedCornerShape(8.dp))
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .clickable(onClickLabel = sendLabel) { onGif(item) }
            .semantics { contentDescription = sendLabel },
    ) {
        bytes?.let { b ->
            AnimatedImage(
                source = AnimatedSource.OfBytes(item.id, b),
                contentDescription = t("chat.media.gif"),
                modifier = Modifier.fillMaxSize(),
            )
        }
    }
}
