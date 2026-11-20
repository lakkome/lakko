// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.feed

import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
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
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.feed.FEED_IMPORT_REFUSED
import me.lakko.app.feed.FeedCore
import me.lakko.app.feed.FeedPrefs
import me.lakko.app.feed.FeedSealedStore
import me.lakko.app.feed.SavedFeed
import me.lakko.app.feed.UniffiFeedCore
import me.lakko.app.feed.bytesFromFragment
import me.lakko.app.feed.draftSummary
import me.lakko.app.feed.newFeedId
import me.lakko.app.feed.upsertFeed
import me.lakko.app.l10n.LocalStrings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import uniffi.lakko_ffi_uniffi.FeedRule

/**
 * A tapped `https://lakko.me/feed/import#<rule>` link. The fragment carries the
 * rule and is read on the device only; the slot is consumed once the import
 * screen has opened.
 */
public object FeedImportIntake {
    private val slot = MutableStateFlow<String?>(null)

    /** The fragment of the last tapped link ("" when the link had none). */
    public val pending: StateFlow<String?> = slot.asStateFlow()

    public fun publish(fragment: String?) {
        slot.value = fragment.orEmpty()
    }

    private val fileSlot = MutableStateFlow<Uri?>(null)

    /** A `.lakkofeed` file opened from another app ("Open with" / share). */
    public val pendingFile: StateFlow<Uri?> = fileSlot.asStateFlow()

    public fun publishFile(uri: Uri) {
        fileSlot.value = uri
    }

    public fun consume() {
        slot.value = null
    }

    public fun consumeFile() {
        fileSlot.value = null
    }
}

/**
 * Import a shared feed (`docs/ux/feed.md` section 4.1): from a link, whose
 * fragment carries the rule (no server sees it), or from a `.lakkofeed` file.
 * The person sees a preview of the rules before anything is saved; anything
 * the core refuses is refused whole. Mirror of `apps/web/src/routes/FeedImport.tsx`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun FeedImportScreen(
    accountIdB64: String?,
    fragment: String?,
    onDone: () -> Unit,
    /** UX spec section 5 — a `.lakkofeed` file opened from another app. */
    fileUri: Uri? = null,
    core: FeedCore = UniffiFeedCore,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var draft by remember { mutableStateOf<FeedRule?>(null) }
    var error by remember { mutableStateOf<UiText?>(null) }
    var busy by remember { mutableStateOf(false) }

    fun open(bytes: ByteArray) {
        try {
            draft = core.decode(bytes)
            error = null
        } catch (_: Throwable) {
            draft = null
            error = FEED_IMPORT_REFUSED
        }
    }

    LaunchedEffect(fragment) {
        val bytes = bytesFromFragment(fragment)
        if (bytes != null) {
            open(bytes)
        } else if (!fragment.isNullOrEmpty()) {
            error = FEED_IMPORT_REFUSED
        }
    }

    fun openFile(uri: Uri) {
        scope.launch {
            // The core refuses anything larger than a rule can be; reading far
            // more than that is pointless, so the read is cut off above it.
            val limit = core.ruleMaxLen() * 2
            val bytes = withContext(Dispatchers.IO) {
                runCatching {
                    context.contentResolver.openInputStream(uri)?.use { input ->
                        val out = java.io.ByteArrayOutputStream()
                        val chunk = ByteArray(4096)
                        while (out.size() <= limit) {
                            val n = input.read(chunk)
                            if (n < 0) break
                            out.write(chunk, 0, n)
                        }
                        out.toByteArray().takeIf { it.size <= limit }
                    }
                }.getOrNull()
            }
            if (bytes == null) {
                draft = null
                error = FEED_IMPORT_REFUSED
            } else {
                open(bytes)
            }
        }
    }

    // The same preview and the same refusal as a link (UX spec section 5).
    LaunchedEffect(fileUri) { fileUri?.let(::openFile) }

    val picker = rememberLauncherForActivityResult(ActivityResultContracts.OpenDocument()) { uri: Uri? ->
        if (uri != null) openFile(uri)
    }

    fun save() {
        val d = draft ?: return
        val acct = accountIdB64 ?: return
        busy = true
        scope.launch {
            try {
                val id = newFeedId()
                val encoded = core.encode(d)
                val ok = withContext(Dispatchers.IO) {
                    val state = FeedSealedStore.loadFeedState(context, acct)
                    FeedSealedStore.saveFeedState(context, acct, upsertFeed(state, SavedFeed(id, encoded)))
                }
                if (ok) {
                    FeedPrefs.setSelection(context, "custom:$id")
                    onDone()
                } else {
                    error = UiText("feed.error.save")
                }
            } catch (cause: Throwable) {
                me.lakko.app.rethrowIfCancelled(cause)
                error = UiText("feed.error.save")
            } finally {
                busy = false
            }
        }
    }

    Scaffold(topBar = { TopAppBar(title = { Text(t("feed.import.title")) }) }) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            val d = draft
            if (d == null) {
                Text(
                    t("feed.import.hint"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Button(onClick = { picker.launch(arrayOf("*/*")) }) { Text(t("feed.import.choose_file")) }
                TextButton(onClick = onDone) { Text(t("common.cancel")) }
            } else {
                Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                    me.lakko.app.ui.components.LkIcon(
                        me.lakko.app.feed.FeedIcon.of(d.icon).drawable(),
                        contentDescription = null,
                    )
                    androidx.compose.foundation.layout.Spacer(Modifier.padding(start = 8.dp))
                    Text(d.name, style = MaterialTheme.typography.titleMedium)
                }
                for (line in draftSummary(d, LocalStrings.current)) Text(line, style = MaterialTheme.typography.bodyMedium)
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { save() }, enabled = !busy && accountIdB64 != null) { Text(t("feed.import.add")) }
                    TextButton(onClick = onDone) { Text(t("common.cancel")) }
                }
            }
            error?.let { Text(it.text(), color = MaterialTheme.colorScheme.error) }
        }
    }
}
