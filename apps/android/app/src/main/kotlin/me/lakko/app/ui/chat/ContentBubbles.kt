// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import kotlinx.coroutines.delay
import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.MemberIdentity
import me.lakko.app.chat.OwnBallotStatus
import me.lakko.app.chat.checklistViewOf
import me.lakko.app.chat.pollViewOf
import me.lakko.app.l10n.t
import uniffi.lakko_ffi_uniffi.MessageContent

/** What a contact card offers on this device (UX 10.2). */
internal enum class ContactCardAction { OPEN_CHAT, THIS_IS_YOU, ADD }

/** Everything a content bubble needs from the screen. */
internal class ContentBubbleContext(
    val groupId: ByteArray?,
    val ownKey: MemberIdentity?,
    val isOwnerOrAdmin: Boolean,
    val isOneToOne: Boolean,
    val nameOf: (MemberIdentity) -> String?,
    val onVote: (Long, ByteArray) -> Unit,
    val onRetryVote: (Long) -> Unit,
    val onViewResults: (ChatMessage) -> Unit,
    val onTick: (Long, uniffi.lakko_ffi_uniffi.ChecklistItemRef, Boolean) -> Unit,
    val onAddItem: (Long, String) -> Unit,
    val onOpenLocation: (uniffi.lakko_ffi_uniffi.SharedLocation) -> Unit,
    val contactCardAction: (ByteArray) -> ContactCardAction,
    val onContactCard: (uniffi.lakko_ffi_uniffi.SharedContactCard, ContactCardAction) -> Unit,
    val onOpenArticle: (ChatMessage) -> Unit,
    /** UX 7.3 / 8.2 — close a poll or checklist, after the confirmation. */
    val onClose: (ChatMessage) -> Unit = {},
    val onEditChecklist: (ChatMessage) -> Unit = {},
    val onEditArticle: (ChatMessage) -> Unit = {},
    val onCopyText: (String) -> Unit = {},
    /** ADR-0092 section 4.1 — a channel post: every poll is anonymous. */
    val channel: Boolean = false,
    /** ADR-0092 section 4 — a channel poll draws the service's counts. */
    val pollViewOverride: ((ChatMessage) -> me.lakko.app.chat.PollView?)? = null,
    /** ADR-0092 section 3.6 — this account's choice as its devices recorded it. */
    val syncedChoiceOf: (Long) -> ByteArray? = { null },
    /** UX 11.2 — an article card's banner: its first image, as a photo bubble draws it. */
    val banner: (@Composable (uniffi.lakko_ffi_uniffi.AttachmentRef) -> Unit)? = null,
)

/**
 * ADR-0092 / UX 7.3, 8.2, 9.3, 11.3 — the rows a content message adds to its
 * message menu, for this device and person.
 */
internal fun contentMenuRows(
    msg: ChatMessage,
    ctx: ContentBubbleContext,
    strings: me.lakko.app.l10n.Strings,
    nowMs: Long,
): List<ContentMenuRow> {
    val isAuthor = msg.authorKey != null && msg.authorKey == ctx.ownKey
    return when (val c = msg.content) {
        is MessageContent.Poll -> buildList {
            val view = ctx.pollViewOverride?.invoke(msg) ?: ctx.groupId?.let { pollViewOf(msg, it, ctx.ownKey, nowMs) }
            if (view != null && !view.closed && view.ownChoices.isNotEmpty()) {
                add(ContentMenuRow(strings["poll.retract"]) { ctx.onVote(msg.messageId, ByteArray(0)) })
            }
            if (view != null && !view.closed && (isAuthor || (!ctx.isOneToOne && ctx.isOwnerOrAdmin))) {
                add(ContentMenuRow(strings["poll.close"]) { ctx.onClose(msg) })
            }
        }
        is MessageContent.Checklist -> buildList {
            val view = checklistViewOf(msg)
            if (isAuthor && !ctx.channel && view != null && !view.closed) {
                add(ContentMenuRow(strings["checklist.edit"]) { ctx.onEditChecklist(msg) })
            }
            if ((isAuthor || (ctx.channel && ctx.isOwnerOrAdmin)) && view != null && !view.closed) {
                add(ContentMenuRow(strings["checklist.close"]) { ctx.onClose(msg) })
            }
        }
        is MessageContent.Location -> listOf(
            ContentMenuRow(strings["location.copy"]) {
                ctx.onCopyText(formatCoordinates(c.location.latE7, c.location.lonE7))
            },
        )
        is MessageContent.Article -> if (isAuthor) {
            listOf(ContentMenuRow(strings["chat.message.edit"]) { ctx.onEditArticle(msg) })
        } else {
            emptyList()
        }
        else -> emptyList()
    }
}

internal val LocalContentBubble = staticCompositionLocalOf<ContentBubbleContext?> { null }

/** UX 7.2 — the kind line under a poll's question. */
internal fun pollKindKey(anonymous: Boolean, quiz: Boolean): String = when {
    anonymous && quiz -> "poll.kind.anonymous_quiz"
    anonymous -> "poll.kind.anonymous"
    quiz -> "poll.kind.quiz"
    else -> "poll.kind.public"
}

/** UX 7.2 — a share as a whole percentage, the rounding every client uses. */
internal fun pollPercent(count: Int, voters: Int): Int = if (voters <= 0) 0 else Math.round(count * 100f / voters)

/** Draw the bubble of a content message (UX 7.2, 8.2, 9.3, 10.2, 11.2). */
@Composable
internal fun ContentBubble(msg: ChatMessage, contentColor: Color, isOwn: Boolean) {
    val ctx = LocalContentBubble.current ?: return
    when (val content = msg.content) {
        is MessageContent.Poll -> PollCard(msg, content, ctx, contentColor)
        is MessageContent.Checklist -> ChecklistCard(msg, ctx, contentColor, isOwn)
        is MessageContent.Location -> LocationCard(content.location, ctx, contentColor)
        is MessageContent.ContactCard -> ContactCardBubble(content.card, ctx, contentColor)
        is MessageContent.Article -> ArticleCard(msg, ctx, contentColor)
        null -> Unit
    }
}

@Composable
private fun PollCard(msg: ChatMessage, content: MessageContent.Poll, ctx: ContentBubbleContext, contentColor: Color) {
    val body = content.poll.body
    var now by remember { mutableLongStateOf(System.currentTimeMillis()) }
    LaunchedEffect(body.closeAtMs) {
        while (body.closeAtMs != null && now < body.closeAtMs!!) {
            delay(30_000)
            now = System.currentTimeMillis()
        }
    }
    val view = (ctx.pollViewOverride?.invoke(msg) ?: ctx.groupId?.let { pollViewOf(msg, it, ctx.ownKey, now) } ?: return).let { v ->
        // A vote cast on another of this account's devices (anonymous: the
        // own choice is not in the tally) shows as this person's choice too.
        if (v.ownChoices.isEmpty() && v.ownBallotStatus == null) {
            ctx.syncedChoiceOf(msg.messageId)?.takeIf { it.isNotEmpty() }
                ?.let { c -> v.copy(ownChoices = c.map { it.toInt() and 0xff }.toSet()) } ?: v
        } else {
            v
        }
    }
    val quiz = body.quiz
    val muted = contentColor.copy(alpha = 0.7f)
    var picked by remember(msg.messageId) { mutableStateOf(setOf<Int>()) }
    val voted = view.ownChoices.isNotEmpty() && view.ownBallotStatus != OwnBallotStatus.Failed
    val showResults = voted || view.closed
    val sending = view.ownBallotStatus == OwnBallotStatus.Sending
    Column(
        verticalArrangement = Arrangement.spacedBy(6.dp),
        modifier = Modifier.widthIn(min = 240.dp, max = 360.dp),
    ) {
        Text(body.question, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = contentColor)
        val kind = t(pollKindKey(body.anonymous != null || ctx.channel, quiz != null)) +
            if (body.multiple) " · " + t("poll.kind.multiple") else ""
        Text(kind, fontSize = 13.sp, color = muted)
        for ((i, option) in body.options.withIndex()) {
            val count = view.counts.getOrElse(i) { 0 }
            val percent = pollPercent(count, view.voters)
            val mine = i in view.ownChoices
            if (showResults && !view.unverifiable) {
                val tint = when {
                    quiz != null && quiz.correct.toInt() == i -> Color(0xFF2E9E5B)
                    quiz != null && mine -> MaterialTheme.colorScheme.error
                    else -> contentColor
                }
                val a11y = t(
                    "poll.option_result",
                    mapOf("option" to option, "percent" to percent.toString()),
                    mapOf("count" to count.toULong()),
                ) + if (mine) ", " + t("poll.your_choice") else ""
                Box(
                    Modifier
                        .fillMaxWidth()
                        .heightIn(min = 36.dp)
                        .clip(RoundedCornerShape(8.dp))
                        .semantics { contentDescription = a11y },
                ) {
                    Box(
                        Modifier
                            .fillMaxWidth(percent / 100f)
                            .fillMaxHeight()
                            .heightIn(min = 36.dp)
                            .background(tint.copy(alpha = 0.18f)),
                    )
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth().heightIn(min = 36.dp).padding(horizontal = 8.dp),
                    ) {
                        if (mine) me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_checkmark, contentDescription = null, tint = tint, size = 16.dp)
                        Text(option, fontSize = 15.sp, color = contentColor, modifier = Modifier.weight(1f).padding(start = 4.dp))
                        Text("$percent%", fontSize = 13.sp, color = muted)
                    }
                }
            } else {
                val enabled = !view.closed && !view.unverifiable && !sending
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier
                        .fillMaxWidth()
                        .heightIn(min = 40.dp)
                        .clickable(enabled = enabled) {
                            if (body.multiple) {
                                picked = if (i in picked) picked - i else picked + i
                            } else {
                                ctx.onVote(msg.messageId, byteArrayOf(i.toByte()))
                            }
                        },
                ) {
                    if (sending && mine) {
                        CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
                    } else if (body.multiple) {
                        Checkbox(checked = i in picked, onCheckedChange = null, enabled = enabled)
                    } else {
                        RadioButton(selected = false, onClick = null, enabled = enabled)
                    }
                    Text(option, fontSize = 15.sp, color = contentColor, modifier = Modifier.padding(start = 8.dp))
                }
            }
        }
        if (body.multiple && !showResults && picked.isNotEmpty() && !sending) {
            OutlinedButton(onClick = {
                ctx.onVote(msg.messageId, picked.sorted().map { it.toByte() }.toByteArray())
                picked = emptySet()
            }) { Text(t("poll.vote")) }
        }
        if (showResults && quiz != null && quiz.explanation.isNotEmpty()) {
            Surface(shape = RoundedCornerShape(8.dp), color = contentColor.copy(alpha = 0.08f)) {
                Text(quiz.explanation, fontSize = 13.sp, color = contentColor, modifier = Modifier.padding(8.dp))
            }
        }
        if (view.unverifiable) {
            Text(t("poll.unverifiable"), fontSize = 13.sp, color = muted)
        }
        // Footer: votes, closed / closes in, sending / failed.
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            when (view.ownBallotStatus) {
                OwnBallotStatus.Sending -> Text(t("poll.sending_vote"), fontSize = 13.sp, color = muted)
                OwnBallotStatus.Failed -> {
                    Text(t("poll.vote_failed"), fontSize = 13.sp, color = MaterialTheme.colorScheme.error)
                    TextButton(onClick = { ctx.onRetryVote(msg.messageId) }) { Text(t("poll.try_again")) }
                }
                else -> {
                    Text(t("poll.votes", emptyMap(), mapOf("n" to view.voters.toULong())), fontSize = 13.sp, color = muted)
                    if (view.closed) {
                        Text("· " + t("poll.closed"), fontSize = 13.sp, color = muted)
                    } else {
                        view.closeAtMs?.let { at ->
                            Text(
                                "· " + t("poll.closes_in", mapOf("time" to formatRemaining(at - now))),
                                fontSize = 13.sp,
                                color = muted,
                            )
                        }
                    }
                }
            }
        }
        if (body.anonymous == null && showResults) {
            TextButton(onClick = { ctx.onViewResults(msg) }) { Text(t("poll.view_results")) }
        }
    }
}

/** `{time}` of "Closes in {time}", formatted for the locale. */
internal fun formatRemaining(ms: Long): String {
    val minutes = (ms.coerceAtLeast(0) / 60_000).toInt()
    return android.icu.text.MeasureFormat.getInstance(
        java.util.Locale.getDefault(),
        android.icu.text.MeasureFormat.FormatWidth.SHORT,
    ).format(
        when {
            minutes >= 60 * 24 -> android.icu.util.Measure(minutes / (60 * 24), android.icu.util.MeasureUnit.DAY)
            minutes >= 60 -> android.icu.util.Measure(minutes / 60, android.icu.util.MeasureUnit.HOUR)
            else -> android.icu.util.Measure(minutes.coerceAtLeast(1), android.icu.util.MeasureUnit.MINUTE)
        },
    )
}

@Composable
private fun ChecklistCard(msg: ChatMessage, ctx: ContentBubbleContext, contentColor: Color, isOwn: Boolean) {
    val view = checklistViewOf(msg) ?: return
    val muted = contentColor.copy(alpha = 0.7f)
    val isAuthor = msg.authorKey != null && msg.authorKey == ctx.ownKey
    // UX 8.1 — in a channel the publishers tick and add (each a post of
    // theirs); subscribers read.
    val mayTick = !view.closed && if (ctx.channel) ctx.isOwnerOrAdmin else (isAuthor || view.othersMark || ctx.isOwnerOrAdmin)
    val mayAdd = !view.closed && if (ctx.channel) ctx.isOwnerOrAdmin else (isAuthor || view.othersAdd || ctx.isOwnerOrAdmin)
    val done = view.items.count { it.doneBy != null }
    var adding by remember(msg.messageId) { mutableStateOf(false) }
    var newItem by remember(msg.messageId) { mutableStateOf("") }
    Column(verticalArrangement = Arrangement.spacedBy(4.dp), modifier = Modifier.widthIn(min = 240.dp, max = 360.dp)) {
        Text(view.title, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = contentColor)
        Text(
            t("checklist.done_of", mapOf("done" to done.toString(), "total" to view.items.size.toString())),
            fontSize = 13.sp,
            color = muted,
        )
        for (item in view.items) {
            val doneBy = item.doneBy
            val ticker = doneBy?.let { me.lakko.app.chat.outboxB64(it) }
            val tickerName = ticker?.let { key ->
                if (ctx.channel || (ctx.isOneToOne && key == ctx.ownKey)) null else ctx.nameOf(key)?.substringBefore(' ')
            }
            val a11y = if (doneBy != null) {
                t("checklist.item_done", mapOf("text" to item.text, "name" to (tickerName ?: "")))
            } else {
                t("checklist.item_not_done", mapOf("text" to item.text))
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier
                    .fillMaxWidth()
                    .heightIn(min = 40.dp)
                    .semantics { contentDescription = a11y }
                    .clickable(enabled = mayTick) { ctx.onTick(msg.messageId, item.item, doneBy == null) },
            ) {
                Checkbox(checked = doneBy != null, onCheckedChange = null, enabled = mayTick)
                Text(
                    item.text,
                    fontSize = 15.sp,
                    color = if (doneBy != null) muted else contentColor,
                    modifier = Modifier.weight(1f).padding(start = 4.dp),
                )
                if (tickerName != null) Text(tickerName, fontSize = 12.sp, color = muted)
            }
        }
        if (mayAdd) {
            if (adding) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    OutlinedTextField(
                        value = newItem,
                        onValueChange = { newItem = it.take(100) },
                        singleLine = true,
                        modifier = Modifier.weight(1f),
                    )
                    TextButton(onClick = {
                        ctx.onAddItem(msg.messageId, newItem)
                        newItem = ""
                        adding = false
                    }, enabled = newItem.isNotBlank()) { Text(t("checklist.add_item")) }
                }
            } else {
                TextButton(onClick = { adding = true }) { Text(t("checklist.add_item")) }
            }
        }
    }
}

@Composable
private fun LocationCard(loc: uniffi.lakko_ffi_uniffi.SharedLocation, ctx: ContentBubbleContext, contentColor: Color) {
    val coords = formatCoordinates(loc.latE7, loc.lonE7)
    val distance = formatDistance(loc.accuracyM.toLong())
    val a11y = (loc.name?.let { "$it, " } ?: "") + t("location.a11y", mapOf("coordinates" to coords, "distance" to distance))
    Row(
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        modifier = Modifier
            .widthIn(min = 220.dp, max = 360.dp)
            .semantics { contentDescription = a11y }
            .clickable { ctx.onOpenLocation(loc) },
    ) {
        Box(
            Modifier.size(44.dp).clip(CircleShape).background(MaterialTheme.colorScheme.primary.copy(alpha = 0.15f)),
            contentAlignment = Alignment.Center,
        ) { me.lakko.app.ui.components.LkIcon(me.lakko.app.R.drawable.ic_lk_location, contentDescription = null, tint = MaterialTheme.colorScheme.primary) }
        Column {
            loc.name?.let { Text(it, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = contentColor) }
            Text(coords, fontSize = 13.sp, color = contentColor.copy(alpha = 0.7f))
            Text(t("location.accuracy", mapOf("distance" to distance)), fontSize = 12.sp, color = contentColor.copy(alpha = 0.7f))
        }
    }
}

@Composable
private fun ContactCardBubble(card: uniffi.lakko_ffi_uniffi.SharedContactCard, ctx: ContentBubbleContext, contentColor: Color) {
    val action = ctx.contactCardAction(card.accountKey)
    Column(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.widthIn(min = 220.dp, max = 360.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            me.lakko.app.ui.components.LakkoAvatar(name = card.name, size = 44.dp, personKey = card.accountKey)
            Column {
                Text(card.name, fontSize = 15.sp, fontWeight = FontWeight.SemiBold, color = contentColor, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text("@${card.username}", fontSize = 13.sp, color = contentColor.copy(alpha = 0.7f))
            }
        }
        when (action) {
            ContactCardAction.THIS_IS_YOU -> Text(t("contact.this_is_you"), fontSize = 13.sp, color = contentColor.copy(alpha = 0.7f))
            ContactCardAction.OPEN_CHAT -> OutlinedButton(onClick = { ctx.onContactCard(card, action) }) { Text(t("contact.open_chat")) }
            ContactCardAction.ADD -> OutlinedButton(onClick = { ctx.onContactCard(card, action) }) { Text(t("contact.add")) }
        }
    }
}

@Composable
private fun ArticleCard(msg: ChatMessage, ctx: ContentBubbleContext, contentColor: Color) {
    val (article, images) = me.lakko.app.chat.articleOf(msg) ?: return
    val bannerRef = article.blocks.firstNotNullOfOrNull { b ->
        (b as? uniffi.lakko_ffi_uniffi.ArticleBlock.Image)?.let { images.getOrNull(it.attachmentIndex.toInt()) }
    }
    val excerpt = try {
        uniffi.lakko_ffi_uniffi.articleExcerpt(article)
    } catch (_: Throwable) {
        ""
    }
    Column(
        verticalArrangement = Arrangement.spacedBy(6.dp),
        modifier = Modifier.widthIn(min = 240.dp, max = 360.dp).clickable { ctx.onOpenArticle(msg) },
    ) {
        if (bannerRef != null) ctx.banner?.invoke(bannerRef)
        Text(article.title, fontSize = 17.sp, fontWeight = FontWeight.SemiBold, color = contentColor, maxLines = 3, overflow = TextOverflow.Ellipsis)
        if (excerpt.isNotEmpty()) {
            Text(excerpt, fontSize = 14.sp, color = contentColor.copy(alpha = 0.7f), maxLines = 4, overflow = TextOverflow.Ellipsis)
        }
        Text(t("article.read"), fontSize = 14.sp, fontWeight = FontWeight.Medium, color = MaterialTheme.colorScheme.primary)
    }
}
