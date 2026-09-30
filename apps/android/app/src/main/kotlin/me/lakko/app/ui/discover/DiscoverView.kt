// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.discover

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
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.FilterChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.discover.DirectoryCard
import me.lakko.app.discover.DiscoverCopy
import me.lakko.app.discover.DiscoverModel
import me.lakko.app.discover.SearchState
import me.lakko.app.discover.languageLabel
import me.lakko.app.discover.languageOptions
import me.lakko.app.discover.reasonLine
import me.lakko.app.ui.components.LakkoAvatar

/**
 * The Discover chip of the Feed (docs/ux/feed.md section 6, ADR-0059). Mirror
 * of the view half of `apps/web/src/discover/Discover.tsx`: the search field
 * with its "sent to Lakko's server" line, the pick-topics card on first open,
 * the Discover settings (Edit topics, Mix in random topics), and the ranked
 * channel cards. The ranking and the state live in [DiscoverModel].
 */
@Composable
public fun DiscoverView(
    model: DiscoverModel,
    /** The ranked suggestions (buckets + group-shared), computed by the caller. */
    cards: List<DirectoryCard>,
    onSubscribe: (channelIdHex: String, handle: String) -> Unit,
) {
    val prefs by model.prefs.collectAsStateWithLifecycle()
    val loaded by model.loaded.collectAsStateWithLifecycle()
    val refreshing by model.refreshing.collectAsStateWithLifecycle()
    val unavailable by model.unavailable.collectAsStateWithLifecycle()
    val search by model.search.collectAsStateWithLifecycle()
    val notice by model.notice.collectAsStateWithLifecycle()
    var editing by remember { mutableStateOf(false) }
    var query by remember { mutableStateOf("") }
    LaunchedEffect(loaded) { if (loaded) model.refresh() }
    val showIntro = loaded && (editing || (prefs.topics.isEmpty() && !prefs.introDismissed))
    val muted = MaterialTheme.colorScheme.onSurfaceVariant

    LazyColumn(modifier = Modifier.fillMaxSize(), contentPadding = PaddingValues(12.dp)) {
        item(key = "search") {
            Column {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(
                        value = query,
                        onValueChange = {
                            query = it.take(64)
                            if (it.isEmpty()) model.clearSearch()
                        },
                        placeholder = { Text("Search channels") },
                        singleLine = true,
                        enabled = !unavailable,
                        keyboardOptions = KeyboardOptions(imeAction = ImeAction.Search),
                        keyboardActions = KeyboardActions(onSearch = { model.runSearch(query) }),
                        modifier = Modifier.weight(1f),
                    )
                    Spacer(Modifier.width(8.dp))
                    TextButton(
                        onClick = { model.runSearch(query) },
                        enabled = !unavailable && search != SearchState.Busy,
                    ) { Text("Search") }
                }
                Text(
                    if (unavailable) DiscoverCopy.UNAVAILABLE else DiscoverCopy.SEARCH_HELPER,
                    style = MaterialTheme.typography.bodySmall,
                    color = muted,
                    modifier = Modifier.padding(vertical = 4.dp),
                )
                notice?.let {
                    Surface(
                        color = MaterialTheme.colorScheme.surfaceVariant,
                        shape = MaterialTheme.shapes.medium,
                        modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                    ) { Text(it, modifier = Modifier.padding(12.dp), style = MaterialTheme.typography.bodyMedium) }
                }
            }
        }
        when (val s = search) {
            SearchState.Idle -> {
                if (showIntro) {
                    item(key = "pick") {
                        PickTopicsCard(model = model, first = !editing, onDone = { editing = false })
                    }
                } else {
                    item(key = "menu") {
                        DiscoverSettings(model = model, mixRandom = prefs.mixRandom, onEditTopics = { editing = true })
                    }
                }
                if (cards.isEmpty()) {
                    item(key = "empty") {
                        if (refreshing) {
                            Text("Loading", color = muted, modifier = Modifier.padding(24.dp))
                        } else if (!showIntro) {
                            Text(
                                if (unavailable) DiscoverCopy.UNAVAILABLE else DiscoverCopy.NOTHING_YET,
                                color = muted,
                                modifier = Modifier.padding(24.dp),
                            )
                        }
                    }
                } else {
                    for (c in cards) {
                        item(key = "card:" + c.channelIdHex) { DiscoverCardView(c, model, onSubscribe) }
                    }
                }
            }
            SearchState.Busy -> item(key = "busy") { Text("Searching...", color = muted, modifier = Modifier.padding(24.dp)) }
            is SearchState.Failed -> item(key = "failed") { Text(s.message, color = muted, modifier = Modifier.padding(24.dp)) }
            is SearchState.Done -> {
                if (s.cards.isEmpty()) {
                    item(key = "none") { Text(DiscoverCopy.NO_RESULTS, color = muted, modifier = Modifier.padding(24.dp)) }
                } else {
                    for (c in s.cards) {
                        item(key = "result:" + c.channelIdHex) { DiscoverCardView(c, model, onSubscribe) }
                    }
                }
            }
        }
    }
}

/** One suggestion card; also where a saved feed mixes suggestions in. */
@Composable
public fun DiscoverCardView(
    card: DirectoryCard,
    model: DiscoverModel,
    onSubscribe: (channelIdHex: String, handle: String) -> Unit,
) {
    val scope = rememberCoroutineScope()
    var busy by remember(card.channelIdHex) { mutableStateOf(false) }
    val muted = MaterialTheme.colorScheme.onSurfaceVariant
    Card(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                LakkoAvatar(name = card.name, seed = card.channelIdHex, size = 40.dp)
                Spacer(Modifier.width(12.dp))
                Column(Modifier.weight(1f)) {
                    Text(card.name, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
                    // The handle is a claim until the lookup before subscribing
                    // checks it: plain text, never a link (ADR-0059 section 3).
                    Text("@${card.handle}", style = MaterialTheme.typography.bodySmall, color = muted)
                }
            }
            val description = card.description
            if (description == null) {
                TextButton(onClick = { model.openCard(card) }) { Text("Show description") }
            } else if (description.isNotEmpty()) {
                var expanded by remember(card.channelIdHex) { mutableStateOf(false) }
                Text(
                    description,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = if (expanded) Int.MAX_VALUE else 2,
                    overflow = TextOverflow.Ellipsis,
                )
                if (!expanded && description.length > 80) {
                    TextButton(onClick = { expanded = true }) { Text("More") }
                }
            }
            val facts = listOfNotNull(
                card.topic?.let { model.topicName(it) },
                card.language?.takeIf { it.isNotEmpty() }?.let(::languageLabel),
                card.band?.let { "$it subscribers" },
            )
            if (facts.isNotEmpty()) {
                Text(facts.joinToString("  ·  "), style = MaterialTheme.typography.labelSmall, color = muted)
            }
            Text(reasonLine(card, model::topicName), style = MaterialTheme.typography.labelSmall, color = muted)
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(
                    enabled = !busy,
                    onClick = {
                        busy = true
                        scope.launch {
                            try {
                                model.subscribe(card, onSubscribe)
                            } finally {
                                busy = false
                            }
                        }
                    },
                ) { Text("Subscribe") }
                OutlinedButton(onClick = { model.notInterested(card.channelIdHex) }) { Text("Not interested") }
            }
            val topic = card.topic
            if (topic != null) {
                TextButton(onClick = { model.hideTopic(topic) }) { Text("Hide this topic") }
            }
        }
    }
}

/** The "Discover settings": Edit topics and Mix in random topics. */
@Composable
private fun DiscoverSettings(model: DiscoverModel, mixRandom: Boolean, onEditTopics: () -> Unit) {
    var open by remember { mutableStateOf(false) }
    Column(Modifier.fillMaxWidth()) {
        TextButton(onClick = { open = !open }) { Text("Discover settings") }
        if (open) {
            TextButton(onClick = onEditTopics) { Text("Edit topics") }
            Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp)) {
                Text(DiscoverCopy.MIX_LABEL, modifier = Modifier.weight(1f))
                Switch(checked = mixRandom, onCheckedChange = { model.setMixRandom(it) })
            }
            Text(
                DiscoverCopy.MIX_OFF_LINE,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(horizontal = 12.dp),
            )
        }
    }
}

/** The pick-topics card (first open, and "Edit topics"). */
@Composable
private fun PickTopicsCard(model: DiscoverModel, first: Boolean, onDone: () -> Unit) {
    var chosen by remember { mutableStateOf(model.prefs.value.topics.toSet()) }
    var lang by remember { mutableStateOf(model.language()) }
    var langMenu by remember { mutableStateOf(false) }
    Card(
        modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        border = androidx.compose.foundation.BorderStroke(1.dp, MaterialTheme.colorScheme.outlineVariant),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(DiscoverCopy.PICK_TITLE, style = MaterialTheme.typography.titleMedium)
            Text(DiscoverCopy.PICK_BODY, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            // Chips in rows of a scrolling strip each, so a long list never
            // pushes the actions off screen.
            for (row in model.topics.chunked(4)) {
                Row(
                    modifier = Modifier.horizontalScroll(rememberScrollState()),
                    horizontalArrangement = Arrangement.spacedBy(6.dp),
                ) {
                    for ((id, name) in row) {
                        FilterChip(
                            selected = id in chosen,
                            onClick = { chosen = if (id in chosen) chosen - id else chosen + id },
                            label = { Text(name) },
                        )
                    }
                }
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Language", modifier = Modifier.weight(1f))
                Box {
                    TextButton(onClick = { langMenu = true }) { Text(languageLabel(lang)) }
                    DropdownMenu(expanded = langMenu, onDismissRequest = { langMenu = false }) {
                        for (code in languageOptions(lang)) {
                            DropdownMenuItem(
                                text = { Text(languageLabel(code)) },
                                onClick = {
                                    lang = code
                                    langMenu = false
                                },
                            )
                        }
                    }
                }
            }
            Spacer(Modifier.height(4.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.fillMaxWidth()) {
                Spacer(Modifier.weight(1f))
                TextButton(onClick = {
                    if (first) model.dismissIntro()
                    onDone()
                }) { Text(if (first) "Not now" else "Cancel") }
                Button(onClick = {
                    model.savePicks(chosen.toList(), lang)
                    onDone()
                }) { Text("Save") }
            }
        }
    }
}
