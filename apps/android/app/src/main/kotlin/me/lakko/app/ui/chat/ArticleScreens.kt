// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import kotlinx.coroutines.launch
import me.lakko.app.chat.from
import me.lakko.app.chat.toAttachmentRef
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone
import uniffi.lakko_ffi_uniffi.Article
import uniffi.lakko_ffi_uniffi.ArticleBlock
import uniffi.lakko_ffi_uniffi.ArticleHeadingLevel
import uniffi.lakko_ffi_uniffi.ArticleListItem
import uniffi.lakko_ffi_uniffi.ArticleRun
import uniffi.lakko_ffi_uniffi.AttachmentRef

/** The kind of one block in the article editor (UX 11.1 format bar). */
internal enum class EditKind { TEXT, HEADING, SUBHEADING, QUOTE, BULLET, NUMBER, IMAGE, DIVIDER }

/**
 * One block of the article editor. Bold and italic apply to the block's text
 * as a whole; links are added as runs after it. This is the editor's own
 * model; [toArticle] turns it into the core's bounded article.
 */
internal data class EditBlock(
    val kind: EditKind,
    val text: String = "",
    val bold: Boolean = false,
    val italic: Boolean = false,
    val links: List<Pair<String, String>> = emptyList(),
    val image: AttachmentRef? = null,
    /** UX 11.1 — Bold / Italic applied to a selection: character ranges of [text]. */
    val boldSpans: List<IntRange> = emptyList(),
    val italicSpans: List<IntRange> = emptyList(),
)

private fun IntRange.covers(i: Int): Boolean = i in this

/**
 * [b]'s text as runs: a run per stretch of equal bold/italic, the block-wide
 * flags or-ed with the selection spans; links after it.
 */
internal fun runsOf(b: EditBlock): List<ArticleRun> = buildList {
    if (b.text.isNotEmpty()) {
        var start = 0
        fun styleAt(i: Int) = Pair(
            b.bold || b.boldSpans.any { it.covers(i) },
            b.italic || b.italicSpans.any { it.covers(i) },
        )
        var current = styleAt(0)
        for (i in 1..b.text.length) {
            val next = if (i < b.text.length) styleAt(i) else null
            if (next != current) {
                add(ArticleRun.Text(current.first, current.second, b.text.substring(start, i)))
                start = i
                if (next != null) current = next
            }
        }
    }
    for ((text, url) in b.links) add(ArticleRun.Link(url, b.bold, b.italic, text))
}

/** Toggle [range] in [spans]: removed where every character was set, else added. */
internal fun toggleSpan(spans: List<IntRange>, range: IntRange): List<IntRange> {
    if (range.isEmpty()) return spans
    val allSet = range.all { i -> spans.any { i in it } }
    val chars = spans.flatMap { it.toList() }.toMutableSet()
    if (allSet) chars.removeAll(range.toSet()) else chars.addAll(range)
    return spansOf(chars)
}

private fun spansOf(chars: Set<Int>): List<IntRange> {
    val sorted = chars.sorted()
    val out = mutableListOf<IntRange>()
    var start = -1
    var prev = -2
    for (c in sorted) {
        if (c != prev + 1) {
            if (start >= 0) out.add(start..prev)
            start = c
        }
        prev = c
    }
    if (start >= 0) out.add(start..prev)
    return out
}

/** Move [spans] across an edit of [old] into [new] (common prefix and suffix kept). */
internal fun shiftSpans(old: String, new: String, spans: List<IntRange>): List<IntRange> {
    if (spans.isEmpty() || old == new) return spans
    var prefix = 0
    while (prefix < old.length && prefix < new.length && old[prefix] == new[prefix]) prefix++
    var suffix = 0
    while (suffix < old.length - prefix && suffix < new.length - prefix &&
        old[old.length - 1 - suffix] == new[new.length - 1 - suffix]
    ) {
        suffix++
    }
    val delta = new.length - old.length
    val removedEnd = old.length - suffix
    val chars = mutableSetOf<Int>()
    for (r in spans) for (i in r) {
        when {
            i < prefix -> chars.add(i)
            i >= removedEnd -> chars.add(i + delta)
        }
    }
    return spansOf(chars.filter { it in new.indices }.toSet())
}

/**
 * The editor's blocks as the wire's [Article] plus its image attachments in
 * order (`Image.attachmentIndex` indexes them). Consecutive list items of one
 * kind become one list. Empty text blocks are dropped.
 */
internal fun toArticle(title: String, blocks: List<EditBlock>): Pair<Article, List<AttachmentRef>> {
    val out = mutableListOf<ArticleBlock>()
    val images = mutableListOf<AttachmentRef>()
    var listKind: EditKind? = null
    var listItems = mutableListOf<ArticleListItem>()
    fun flushList() {
        if (listKind != null && listItems.isNotEmpty()) {
            out.add(ArticleBlock.ListBlock(listKind == EditKind.NUMBER, listItems.toList()))
        }
        listKind = null
        listItems = mutableListOf()
    }
    for (b in blocks) {
        if (b.kind == EditKind.BULLET || b.kind == EditKind.NUMBER) {
            if (listKind != b.kind) flushList()
            listKind = b.kind
            val runs = runsOf(b)
            if (runs.isNotEmpty()) listItems.add(ArticleListItem(0u, runs))
            continue
        }
        flushList()
        when (b.kind) {
            EditKind.TEXT -> runsOf(b).takeIf { it.isNotEmpty() }?.let { out.add(ArticleBlock.Paragraph(it)) }
            EditKind.HEADING -> runsOf(b).takeIf { it.isNotEmpty() }?.let { out.add(ArticleBlock.Heading(ArticleHeadingLevel.H1, it)) }
            EditKind.SUBHEADING -> runsOf(b).takeIf { it.isNotEmpty() }?.let { out.add(ArticleBlock.Heading(ArticleHeadingLevel.H2, it)) }
            EditKind.QUOTE -> runsOf(b).takeIf { it.isNotEmpty() }?.let { out.add(ArticleBlock.Quote(it)) }
            EditKind.IMAGE -> b.image?.let { ref ->
                out.add(ArticleBlock.Image(images.size.toUByte(), if (b.text.isEmpty()) emptyList() else listOf(ArticleRun.Text(false, false, b.text))))
                images.add(ref)
            }
            EditKind.DIVIDER -> out.add(ArticleBlock.Divider)
            else -> Unit
        }
    }
    flushList()
    return Article(title.trim(), out) to images
}

/** The inverse, for Edit (UX 11.3). */
internal fun editBlocksOf(article: Article, images: List<AttachmentRef>): List<EditBlock> = buildList {
    fun fromRuns(kind: EditKind, runs: List<ArticleRun>): EditBlock {
        val texts = runs.filterIsInstance<ArticleRun.Text>()
        val text = texts.joinToString("") { it.text }
        val bold = mutableSetOf<Int>()
        val italic = mutableSetOf<Int>()
        var at = 0
        for (r in texts) {
            val range = at until at + r.text.length
            if (r.bold) bold.addAll(range)
            if (r.italic) italic.addAll(range)
            at += r.text.length
        }
        val links = runs.filterIsInstance<ArticleRun.Link>().map { it.text to it.url }
        return EditBlock(kind, text, links = links, boldSpans = spansOf(bold), italicSpans = spansOf(italic))
    }
    for (b in article.blocks) when (b) {
        is ArticleBlock.Paragraph -> add(fromRuns(EditKind.TEXT, b.runs))
        is ArticleBlock.Heading -> add(fromRuns(if (b.level == ArticleHeadingLevel.H1) EditKind.HEADING else EditKind.SUBHEADING, b.runs))
        is ArticleBlock.Quote -> add(fromRuns(EditKind.QUOTE, b.runs))
        is ArticleBlock.ListBlock -> for (item in b.items) add(fromRuns(if (b.ordered) EditKind.NUMBER else EditKind.BULLET, item.runs))
        is ArticleBlock.Image -> images.getOrNull(b.attachmentIndex.toInt())?.let { ref ->
            add(EditBlock(EditKind.IMAGE, b.caption.filterIsInstance<ArticleRun.Text>().joinToString("") { it.text }, image = ref))
        }
        ArticleBlock.Divider -> add(EditBlock(EditKind.DIVIDER))
    }
}

/** UX 11.1 — the text size of the article's blocks, for the 64 KiB limit. */
internal fun articleTextBytes(title: String, blocks: List<EditBlock>): Int =
    title.toByteArray().size + blocks.sumOf { b -> b.text.toByteArray().size + b.links.sumOf { it.first.toByteArray().size + it.second.toByteArray().size } }

internal const val ARTICLE_MAX_TEXT_BYTES: Int = 64 * 1024
internal const val ARTICLE_MAX_IMAGES: Int = 20

/**
 * UX 11.1 — one draft per conversation, kept on the device (encrypted
 * preferences, per account, written through the account store gate so a wipe
 * in progress takes no late write) while the editor is open and after Save
 * draft; deleted with the account.
 */
internal object ArticleDrafts {
    private const val PREFS = "lakko-article-drafts"

    @Volatile
    private var context: android.content.Context? = null

    @Volatile
    private var account: String? = null

    @Volatile
    private var opened: android.content.SharedPreferences? = null

    public fun bind(context: android.content.Context, accountIdB64: String?) {
        this.context = context.applicationContext
        this.account = accountIdB64
    }

    private fun prefs(ctx: android.content.Context): android.content.SharedPreferences? =
        opened ?: synchronized(this) {
            opened ?: try {
                val app = ctx.applicationContext
                val master = androidx.security.crypto.MasterKey.Builder(app)
                    .setKeyScheme(androidx.security.crypto.MasterKey.KeyScheme.AES256_GCM)
                    .build()
                androidx.security.crypto.EncryptedSharedPreferences.create(
                    app,
                    PREFS,
                    master,
                    androidx.security.crypto.EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                    androidx.security.crypto.EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
                ).also { opened = it }
            } catch (_: Throwable) {
                null
            }
        }

    @kotlinx.serialization.Serializable
    private data class StoredBlock(
        val kind: String,
        val text: String,
        val bold: Boolean,
        val italic: Boolean,
        val boldRanges: List<Int> = emptyList(),
        val italicRanges: List<Int> = emptyList(),
        val links: List<Pair<String, String>>,
        val image: me.lakko.app.chat.PersistedAttachment? = null,
    )

    @kotlinx.serialization.Serializable
    private data class StoredDraft(val title: String, val blocks: List<StoredBlock>)

    private val json = kotlinx.serialization.json.Json { ignoreUnknownKeys = true }

    public fun get(conversation: String): Pair<String, List<EditBlock>>? {
        val ctx = context ?: return null
        val acct = account ?: return null
        val raw = prefs(ctx)?.getString("$acct/$conversation", null) ?: return null
        return try {
            val d = json.decodeFromString(StoredDraft.serializer(), raw)
            d.title to d.blocks.map { b ->
                EditBlock(
                    kind = EditKind.valueOf(b.kind),
                    text = b.text,
                    bold = b.bold,
                    italic = b.italic,
                    links = b.links,
                    image = b.image?.toAttachmentRef(),
                    boldSpans = b.boldRanges.chunked(2).mapNotNull { if (it.size == 2) it[0] until it[1] else null },
                    italicSpans = b.italicRanges.chunked(2).mapNotNull { if (it.size == 2) it[0] until it[1] else null },
                )
            }
        } catch (_: Throwable) {
            null
        }
    }

    public fun put(conversation: String, title: String, blocks: List<EditBlock>) {
        val ctx = context ?: return
        val acct = account ?: return
        val stored = StoredDraft(
            title,
            blocks.map { b ->
                StoredBlock(
                    kind = b.kind.name,
                    text = b.text,
                    bold = b.bold,
                    italic = b.italic,
                    boldRanges = b.boldSpans.flatMap { listOf(it.first, it.last + 1) },
                    italicRanges = b.italicSpans.flatMap { listOf(it.first, it.last + 1) },
                    links = b.links,
                    image = b.image?.let { me.lakko.app.chat.PersistedAttachment.from(it) },
                )
            },
        )
        me.lakko.app.account.AccountStoreGate.write(acct) {
            prefs(ctx)?.edit()?.putString("$acct/$conversation", json.encodeToString(StoredDraft.serializer(), stored))?.apply()
        }
    }

    public fun clear(conversation: String) {
        val ctx = context ?: return
        val acct = account ?: return
        me.lakko.app.account.AccountStoreGate.write(acct) {
            prefs(ctx)?.edit()?.remove("$acct/$conversation")?.apply()
        }
    }

    /** Forget device / account wipe. */
    public fun deleteAccountRows(context: android.content.Context, accountIdB64: String) {
        val p = prefs(context) ?: return
        val edit = p.edit()
        for (k in p.all.keys) if (k.startsWith("$accountIdB64/")) edit.remove(k)
        edit.apply()
    }
}

/**
 * UX 11.1 — the article editor: a title and a list of blocks with a format
 * bar. [uploadImage] processes a picked image as a photo (section 3) and
 * uploads it encrypted.
 */
@Composable
internal fun ArticleEditor(
    conversation: String,
    initial: Pair<String, List<EditBlock>>?,
    editing: Boolean,
    uploadImage: suspend (android.net.Uri) -> AttachmentRef?,
    onPublish: (Article, List<AttachmentRef>) -> Unit,
    onClose: () -> Unit,
) {
    val start = initial ?: ArticleDrafts.get(conversation).takeIf { !editing }
    var title by remember { mutableStateOf(start?.first ?: "") }
    val blocks = remember {
        mutableStateListOf<EditBlock>().apply { addAll(start?.second ?: listOf(EditBlock(EditKind.TEXT))) }
    }
    var focused by remember { mutableIntStateOf(0) }
    val selections = remember { androidx.compose.runtime.mutableStateMapOf<Int, androidx.compose.ui.text.TextRange>() }
    // UX 11.1 — Bold and Italic apply to the selection; with nothing selected,
    // to the whole block.
    fun toggleStyle(bold: Boolean) {
        val i = focused.coerceIn(0, blocks.lastIndex)
        val b = blocks[i]
        val sel = selections[i]
        blocks[i] = if (sel != null && !sel.collapsed) {
            val range = sel.min until sel.max
            if (bold) b.copy(boldSpans = toggleSpan(b.boldSpans, range)) else b.copy(italicSpans = toggleSpan(b.italicSpans, range))
        } else if (bold) {
            b.copy(bold = !b.bold)
        } else {
            b.copy(italic = !b.italic)
        }
    }
    var linkDialog by remember { mutableStateOf(false) }
    var confirmClose by remember { mutableStateOf(false) }
    var uploading by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val atLimit = articleTextBytes(title, blocks) >= ARTICLE_MAX_TEXT_BYTES ||
        blocks.count { it.kind == EditKind.IMAGE } >= ARTICLE_MAX_IMAGES
    val pickImage = rememberLauncherForActivityResult(ActivityResultContracts.PickVisualMedia()) { uri ->
        if (uri == null) return@rememberLauncherForActivityResult
        uploading = true
        scope.launch {
            val ref = uploadImage(uri)
            uploading = false
            if (ref != null) {
                val at = (focused + 1).coerceIn(0, blocks.size)
                blocks.add(at, EditBlock(EditKind.IMAGE, image = ref))
                blocks.add(at + 1, EditBlock(EditKind.TEXT))
            }
        }
    }
    fun updateFocused(f: (EditBlock) -> EditBlock) {
        val i = focused.coerceIn(0, blocks.lastIndex)
        blocks[i] = f(blocks[i])
    }
    // UX 11.1 — kept on the device while the editor is open.
    androidx.compose.runtime.LaunchedEffect(title, blocks.toList()) {
        if (!editing && (title.isNotBlank() || blocks.any { it.text.isNotBlank() || it.image != null })) {
            kotlinx.coroutines.delay(800)
            ArticleDrafts.put(conversation, title, blocks.toList())
        }
    }
    val (article, _) = toArticle(title, blocks)
    ContentEditorScaffold(
        title = t(if (editing) "article.editor.edit_title" else "article.editor.title"),
        actionLabel = t("article.publish"),
        actionEnabled = title.isNotBlank() && article.blocks.isNotEmpty() && !uploading,
        onAction = {
            val (a, images) = toArticle(title, blocks)
            ArticleDrafts.clear(conversation)
            onPublish(a, images)
        },
        onClose = {
            if (title.isNotBlank() || blocks.any { it.text.isNotBlank() || it.image != null }) confirmClose = true else onClose()
        },
    ) {
        OutlinedTextField(
            value = title,
            onValueChange = { title = it.take(255) },
            label = { Text(t("article.title")) },
            modifier = Modifier.fillMaxWidth(),
        )
        // The format bar (UX 11.1), above the body.
        Row(
            horizontalArrangement = Arrangement.spacedBy(4.dp),
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
        ) {
            val current = blocks.getOrNull(focused)
            TextButton(onClick = {
                updateFocused {
                    it.copy(
                        kind = when (it.kind) {
                            EditKind.HEADING -> EditKind.SUBHEADING
                            EditKind.SUBHEADING -> EditKind.TEXT
                            else -> EditKind.HEADING
                        },
                    )
                }
            }) {
                Text(
                    t(
                        when (current?.kind) {
                            EditKind.HEADING -> "article.format.heading"
                            EditKind.SUBHEADING -> "article.format.subheading"
                            else -> "article.format.text"
                        },
                    ),
                )
            }
            TextButton(onClick = { toggleStyle(bold = true) }) {
                Text(t("article.format.bold"), fontWeight = FontWeight.Bold)
            }
            TextButton(onClick = { toggleStyle(bold = false) }) {
                Text(t("article.format.italic"), fontStyle = FontStyle.Italic)
            }
            TextButton(onClick = { updateFocused { it.copy(kind = if (it.kind == EditKind.BULLET) EditKind.TEXT else EditKind.BULLET) } }) {
                Text(t("article.format.bullets"))
            }
            TextButton(onClick = { updateFocused { it.copy(kind = if (it.kind == EditKind.NUMBER) EditKind.TEXT else EditKind.NUMBER) } }) {
                Text(t("article.format.numbers"))
            }
            TextButton(onClick = { updateFocused { it.copy(kind = if (it.kind == EditKind.QUOTE) EditKind.TEXT else EditKind.QUOTE) } }) {
                Text(t("article.format.quote"))
            }
            TextButton(onClick = { linkDialog = true }, enabled = !atLimit) { Text(t("article.format.link")) }
            TextButton(
                onClick = { pickImage.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
                enabled = !atLimit && !uploading,
            ) { Text(t("article.format.image")) }
            TextButton(onClick = {
                val at = (focused + 1).coerceIn(0, blocks.size)
                blocks.add(at, EditBlock(EditKind.DIVIDER))
                blocks.add(at + 1, EditBlock(EditKind.TEXT))
                focused = at + 1
            }) { Text(t("article.format.divider")) }
        }
        if (atLimit) {
            Text(t("article.at_limit"), fontSize = 13.sp, color = MaterialTheme.colorScheme.error)
        }
        for ((i, b) in blocks.withIndex()) {
            when (b.kind) {
                EditKind.DIVIDER -> HorizontalDivider(Modifier.padding(vertical = 8.dp))
                else -> {
                    if (b.kind == EditKind.IMAGE) {
                        Text("[${t("article.format.image")}]", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    OutlinedTextField(
                        value = androidx.compose.ui.text.input.TextFieldValue(
                            b.text,
                            selections[i]?.let { androidx.compose.ui.text.TextRange(it.start.coerceAtMost(b.text.length), it.end.coerceAtMost(b.text.length)) }
                                ?: androidx.compose.ui.text.TextRange(b.text.length),
                        ),
                        onValueChange = { tv ->
                            val v = tv.text
                            selections[i] = tv.selection
                            if (v.endsWith("\n") && b.kind != EditKind.IMAGE) {
                                val kept = v.dropLast(1)
                                blocks[i] = b.copy(
                                    text = kept,
                                    boldSpans = shiftSpans(b.text, kept, b.boldSpans),
                                    italicSpans = shiftSpans(b.text, kept, b.italicSpans),
                                )
                                val nextKind = if (b.kind == EditKind.BULLET || b.kind == EditKind.NUMBER) b.kind else EditKind.TEXT
                                blocks.add(i + 1, EditBlock(nextKind))
                                focused = i + 1
                            } else {
                                blocks[i] = b.copy(
                                    text = v,
                                    boldSpans = shiftSpans(b.text, v, b.boldSpans),
                                    italicSpans = shiftSpans(b.text, v, b.italicSpans),
                                )
                            }
                        },
                        visualTransformation = { raw ->
                            androidx.compose.ui.text.input.TransformedText(
                                buildAnnotatedString {
                                    append(raw.text)
                                    for (r in b.boldSpans) if (r.last < raw.text.length) addStyle(SpanStyle(fontWeight = FontWeight.Bold), r.first, r.last + 1)
                                    for (r in b.italicSpans) if (r.last < raw.text.length) addStyle(SpanStyle(fontStyle = FontStyle.Italic), r.first, r.last + 1)
                                },
                                androidx.compose.ui.text.input.OffsetMapping.Identity,
                            )
                        },
                        placeholder = {
                            Text(t(if (b.kind == EditKind.IMAGE) "article.caption" else if (i == 0) "article.body" else "article.format.text"))
                        },
                        textStyle = MaterialTheme.typography.bodyLarge.copy(
                            fontWeight = if (b.bold || b.kind == EditKind.HEADING || b.kind == EditKind.SUBHEADING) FontWeight.SemiBold else FontWeight.Normal,
                            fontStyle = if (b.italic) FontStyle.Italic else FontStyle.Normal,
                            fontSize = when (b.kind) {
                                EditKind.HEADING -> 22.sp
                                EditKind.SUBHEADING -> 17.sp
                                else -> 15.sp
                            },
                        ),
                        leadingIcon = when (b.kind) {
                            EditKind.BULLET -> ({ Text("•") })
                            EditKind.NUMBER -> ({ Text("${blocks.subList(0, i + 1).takeLastWhile { it.kind == EditKind.NUMBER }.size}.") })
                            EditKind.QUOTE -> ({ Box(Modifier.width(3.dp).height(24.dp).background(MaterialTheme.colorScheme.primary)) })
                            else -> null
                        },
                        modifier = Modifier
                            .fillMaxWidth()
                            .onFocusChanged { if (it.isFocused) focused = i },
                    )
                    if (b.links.isNotEmpty()) {
                        Text(
                            b.links.joinToString("  ") { it.first },
                            color = MaterialTheme.colorScheme.primary,
                            fontSize = 13.sp,
                        )
                    }
                }
            }
        }
    }
    if (linkDialog) {
        var url by remember { mutableStateOf("https://") }
        var text by remember { mutableStateOf("") }
        val valid = try {
            uniffi.lakko_ffi_uniffi.articleUrlValid(url)
        } catch (_: Throwable) {
            url.startsWith("https://") && url.length > 8
        }
        Dialog(onDismissRequest = { linkDialog = false }) {
            Surface(shape = MaterialTheme.shapes.medium, color = MaterialTheme.colorScheme.surface) {
                Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    Text(t("article.link.insert"), fontSize = 17.sp, fontWeight = FontWeight.SemiBold)
                    OutlinedTextField(value = url, onValueChange = { url = it }, label = { Text(t("article.link.url")) }, singleLine = true)
                    if (!valid && url.length > 8) Text(t("article.link.invalid"), color = MaterialTheme.colorScheme.error, fontSize = 13.sp)
                    OutlinedTextField(value = text, onValueChange = { text = it }, label = { Text(t("article.link.text")) }, singleLine = true)
                    Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                        TextButton(onClick = { linkDialog = false }) { Text(t("common.cancel")) }
                        TextButton(
                            enabled = valid && text.isNotBlank(),
                            onClick = {
                                updateFocused { it.copy(links = it.links + (text.trim() to url.trim())) }
                                linkDialog = false
                            },
                        ) { Text(t("article.link.insert")) }
                    }
                }
            }
        }
    }
    if (confirmClose) {
        LakkoDialog(
            onDismissRequest = { confirmClose = false },
            title = t("article.discard"),
            confirm = LakkoDialogAction(t("article.save_draft"), LakkoDialogTone.Primary) {
                if (!editing) ArticleDrafts.put(conversation, title, blocks.toList())
                onClose()
            },
            extraActions = listOf(
                LakkoDialogAction(t("editor.discard"), LakkoDialogTone.Danger) {
                    ArticleDrafts.clear(conversation)
                    onClose()
                },
            ),
            cancel = LakkoDialogAction(t("editor.keep_editing"), LakkoDialogTone.Secondary) { confirmClose = false },
        )
    }
}

private fun annotated(runs: List<ArticleRun>, linkColor: androidx.compose.ui.graphics.Color): AnnotatedString = buildAnnotatedString {
    for (r in runs) when (r) {
        is ArticleRun.Text -> withStyle(
            SpanStyle(
                fontWeight = if (r.bold) FontWeight.Bold else null,
                fontStyle = if (r.italic) FontStyle.Italic else null,
            ),
        ) { append(r.text) }
        is ArticleRun.Link -> {
            pushStringAnnotation("URL", r.url)
            withStyle(
                SpanStyle(
                    color = linkColor,
                    fontWeight = if (r.bold) FontWeight.Bold else null,
                    fontStyle = if (r.italic) FontStyle.Italic else null,
                ),
            ) { append(r.text) }
            pop()
        }
    }
}

/**
 * UX 11.3 — the reader: full screen, the person's text size, headings,
 * quotes with an accent rule, lists, images with their caption, links that
 * open as message links do. The top bar carries the author, the date, the
 * message menu (`…`) and Close.
 */
@Composable
internal fun ArticleReader(
    article: Article,
    images: List<AttachmentRef>,
    author: String,
    date: String,
    image: @Composable (AttachmentRef) -> Unit,
    onOpenLink: (String) -> Unit,
    onMenu: (() -> Unit)?,
    onClose: () -> Unit,
) {
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false, decorFitsSystemWindows = false)) {
        Surface(color = MaterialTheme.colorScheme.background, modifier = Modifier.fillMaxSize()) {
            Column(Modifier.fillMaxSize().safeDrawingPadding()) {
                Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp)) {
                    IconButton(onClick = onClose) { me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_xmark, contentDescription = t("common.close")) }
                    Column(Modifier.weight(1f)) {
                        Text(author, fontSize = 15.sp, fontWeight = FontWeight.SemiBold)
                        Text(date, fontSize = 13.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
                    }
                    if (onMenu != null) {
                        IconButton(onClick = onMenu) { me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_ellipsis_circle, contentDescription = t("article.more")) }
                    }
                }
                me.lakko.app.ui.theme.ChatTextScale {
                    Column(
                        verticalArrangement = Arrangement.spacedBy(12.dp),
                        modifier = Modifier
                            .fillMaxWidth()
                            .weight(1f)
                            .verticalScroll(rememberScrollState())
                            .padding(horizontal = 16.dp, vertical = 12.dp)
                            .widthIn(max = 720.dp),
                    ) {
                        val link = MaterialTheme.colorScheme.primary
                        Text(article.title, fontSize = 22.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.semantics { heading() })
                        for (b in article.blocks) when (b) {
                            is ArticleBlock.Paragraph -> ArticleText(annotated(b.runs, link), 15, onOpenLink)
                            is ArticleBlock.Heading -> Box(Modifier.semantics { heading() }) {
                                ArticleText(annotated(b.runs, link), if (b.level == ArticleHeadingLevel.H1) 22 else 17, onOpenLink, bold = true)
                            }
                            is ArticleBlock.Quote -> Row {
                                Box(Modifier.width(3.dp).fillMaxHeight().heightIn(min = 20.dp).background(link))
                                Box(Modifier.padding(start = 12.dp)) { ArticleText(annotated(b.runs, link), 15, onOpenLink) }
                            }
                            is ArticleBlock.ListBlock -> Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                                for ((n, item) in b.items.withIndex()) {
                                    Row(Modifier.padding(start = (16 * (item.depth.toInt() + 1)).dp)) {
                                        Text(if (b.ordered) "${n + 1}." else "•", fontSize = 15.sp, modifier = Modifier.width(24.dp))
                                        ArticleText(annotated(item.runs, link), 15, onOpenLink)
                                    }
                                }
                            }
                            is ArticleBlock.Image -> Column {
                                images.getOrNull(b.attachmentIndex.toInt())?.let { image(it) }
                                if (b.caption.isNotEmpty()) {
                                    Text(annotated(b.caption, link), fontSize = 13.sp, color = MaterialTheme.colorScheme.onSurfaceVariant)
                                }
                            }
                            ArticleBlock.Divider -> HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun ArticleText(text: AnnotatedString, sizeSp: Int, onOpenLink: (String) -> Unit, bold: Boolean = false) {
    @Suppress("DEPRECATION")
    androidx.compose.foundation.text.ClickableText(
        text = text,
        style = MaterialTheme.typography.bodyLarge.copy(
            fontSize = sizeSp.sp * me.lakko.app.ui.theme.chatTextFactor(),
            fontWeight = if (bold) FontWeight.SemiBold else null,
            color = MaterialTheme.colorScheme.onBackground,
        ),
        onClick = { offset ->
            text.getStringAnnotations("URL", offset, offset).firstOrNull()?.let { onOpenLink(it.item) }
        },
    )
}
