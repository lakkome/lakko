// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Base64
import me.lakko.app.profile.toFfi
import uniffi.lakko_ffi_uniffi.ChannelCommentReactionKey
import uniffi.lakko_ffi_uniffi.ChannelCommentReactionWriteKey
import uniffi.lakko_ffi_uniffi.ChannelCommentWriteKey
import uniffi.lakko_ffi_uniffi.ChannelReactionKey
import uniffi.lakko_ffi_uniffi.ChannelReactionWriteKey
import uniffi.lakko_ffi_uniffi.FfiBanList
import uniffi.lakko_ffi_uniffi.FfiComment

/**
 * ADR-0087 section 13a — reactions and comments under a channel's posts.
 *
 * A subscriber holds a read key and no MLS leaf, so its write goes to the
 * channels service, signed by a key of its own per reaction and per comment
 * (derived from the account seed, so no key spans two posts), and spends an
 * anonymous token of its own context. Readers get label counts, resolved
 * against the emoji this app knows; a comment is sealed under a key of its
 * post's epoch seed. A ban list (publisher-signed, a post) hides the comments
 * of the author keys it names, in a private channel as in a public one.
 *
 * The account's own reactions and comment nonces are kept on this device
 * ([OwnChannelFeedbackStore]) and in the `lakko-sync` registers the
 * DeviceGroup carries to the account's other devices.
 */
public class ChannelFeedback(
    private val context: Context,
    private val state: ChannelsUiState,
    private val client: ChannelsClient,
    private val channelId: String,
    private val reader: ChannelReader,
) {
    private val channelBytes: ByteArray = ChannelsComposer.channelIdFromHex(channelId)

    /** One emoji and how many reacted with it, and whether this account did. */
    public data class Chip(val emoji: String, val count: Long, val own: Boolean)

    /** A comment as the thread shows it. */
    public data class Comment(
        val cursor: Long,
        val name: String,
        val text: String,
        val replyTo: Long?,
        val authorKey: ByteArray,
        val own: Boolean,
        val tsMs: Long,
        val writerKey: ByteArray = ByteArray(0),
        /** The author's photo reference, when they shared one. */
        val avatar: uniffi.lakko_ffi_uniffi.FfiAvatarRef? = null,
    )

    /** One page of a thread. */
    public data class CommentPage(val comments: List<Comment>, val nextAfter: Long, val more: Boolean)

    private fun account(): String = state.boundAccount ?: throw IllegalStateException("no account bound")

    /** The emoji a reader can name: the quick row, the recent ones and its
     *  own reactions. */
    private fun candidates(extra: Collection<String>): List<String> {
        val quick = try {
            uniffi.lakko_ffi_uniffi.quickReactions()
        } catch (_: Throwable) {
            emptyList()
        }
        val recent = try {
            me.lakko.app.media.MediaPrefs.get(context).recentEmoji
        } catch (_: Throwable) {
            emptyList()
        }
        return (quick + recent + extra).distinct()
    }

    /** Chips for the posts at [cursors] (at most 50 a call). A post this
     *  device cannot open gets none. */
    public suspend fun chips(cursors: List<Long>): Map<Long, List<Chip>> {
        val readable = cursors.filter { reader.epochOf(it) != null }.takeLast(50)
        if (readable.isEmpty()) return emptyMap()
        val summary = client.reactionSummary(channelId, readable.map { it.toULong() })
        val counts = summary.labels
        for ((cursor, v) in summary.votes) pollVotes[cursor.toLong()] = v
        for ((cursor, n) in summary.comments) commentCounts[cursor.toLong()] = n
        val acct = account()
        val out = HashMap<Long, List<Chip>>()
        for (cursor in readable) {
            val own = ownReactions(acct, cursor)
            val labels = counts[cursor.toULong()].orEmpty()
            val epoch = reader.epochOf(cursor) ?: continue
            val seed = reader.seedFor(epoch) ?: continue
            val resolved = ChannelReactionKey.derive(seed, channelBytes, epoch).use { key ->
                key.resolveCounts(cursor.toULong(), candidates(own), labels)
            }
            val chips = resolved.map { Chip(it.emoji, it.count.toLong(), it.emoji in own) }.toMutableList()
            // A reaction of this account the service has not counted yet (a
            // token wait) shows at once.
            for (e in own) if (chips.none { it.emoji == e }) chips += Chip(e, 1, true)
            out[cursor] = chips
        }
        return out
    }

    // ------------------------------------------------------- ADR-0090 section 4

    /** Per poll post: voters and per-label counts as the last [chips] read them. */
    private val pollVotes = java.util.concurrent.ConcurrentHashMap<Long, Pair<Long, List<uniffi.lakko_ffi_uniffi.FfiReactionLabelCount>>>()

    /** ADR-0090 section 3.6 — the own-choice register names a channel poll by
     *  the channel and the post cursor (set version 0: no ring here). */
    private val channelRef: uniffi.lakko_ffi_uniffi.FolderConversationRef
        get() = uniffi.lakko_ffi_uniffi.FolderConversationRef(
            kind = uniffi.lakko_ffi_uniffi.FolderConversationKind.CHANNEL,
            id = channelBytes,
        )

    /** This account's choice in the poll at [cursor], from any of its devices. */
    public fun ownChoice(cursor: Long): ByteArray? =
        me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.pollChoice(channelRef, cursor)?.choices
            ?.takeIf { it.isNotEmpty() }

    /** The opaque labels of a poll post's options (the service counts these). */
    private fun optionLabels(cursor: Long, optionCount: Int): List<ByteArray>? {
        val epoch = reader.epochOf(cursor) ?: return null
        val seed = reader.seedFor(epoch) ?: return null
        return ChannelReactionKey.derive(seed, channelBytes, epoch).use { key ->
            uniffi.lakko_ffi_uniffi.channelPollOptionLabels(key, cursor.toULong(), optionCount.toUByte())
        }
    }

    /**
     * The card of a channel poll: the frozen tally once a publisher closed it,
     * else the service's counts resolved through the option labels.
     */
    public fun pollView(
        cursor: Long,
        body: uniffi.lakko_ffi_uniffi.PollBody,
        closed: Boolean,
        frozen: uniffi.lakko_ffi_uniffi.ChannelPollTally?,
        nowMs: Long,
    ): me.lakko.app.chat.PollView {
        val tally = frozen ?: pollVotes[cursor]?.let { (voters, counts) ->
            runCatching {
                val labels = optionLabels(cursor, body.options.size) ?: return@runCatching null
                uniffi.lakko_ffi_uniffi.channelPollTallyFromCounts(labels, counts, voters.toULong())
            }.getOrNull()
        }
        return me.lakko.app.chat.PollView(
            counts = tally?.counts?.map { it.toInt() } ?: List(body.options.size) { 0 },
            voters = tally?.voters?.toInt() ?: 0,
            ownChoices = ownChoice(cursor)?.map { it.toInt() and 0xff }?.toSet() ?: emptySet(),
            closed = closed || (body.closeAtMs?.let { nowMs >= it } ?: false),
            closeAtMs = body.closeAtMs,
        )
    }

    /**
     * Vote [choices] (empty = take the vote back) on the poll at [cursor]: one
     * vote per account through the anonymous credential. Returns whether the
     * service stored it.
     */
    public suspend fun votePoll(cursor: Long, choices: ByteArray, optionCount: Int): Boolean {
        val creds = ChannelPollCredentials.active ?: return false
        val labels = optionLabels(cursor, optionCount) ?: return false
        val chosen = choices.map { labels[it.toInt() and 0xff] }
        val ok = creds.vote(channelId, channelBytes, cursor.toULong(), chosen)
        if (ok) {
            me.lakko.app.devices.sync.DeviceGroupSyncRepository.active?.recordPollChoice(channelRef, cursor, choices, 0u)
        }
        return ok
    }

    /** The tally a publisher freezes into the post when it closes the poll. */
    public fun currentTallyBytes(cursor: Long, optionCount: Int): ByteArray? = runCatching {
        val (voters, counts) = pollVotes[cursor] ?: return@runCatching null
        val labels = optionLabels(cursor, optionCount) ?: return@runCatching null
        uniffi.lakko_ffi_uniffi.encodeChannelPollTally(
            uniffi.lakko_ffi_uniffi.channelPollTallyFromCounts(labels, counts, voters.toULong()),
        )
    }.getOrNull()

    /** Comment counts the service maintains, per post, as the last
     *  [chips] read them. */
    private val commentCounts = java.util.concurrent.ConcurrentHashMap<Long, Long>()

    /** The comment count of the post at [cursor] as the service keeps it,
     *  or `null` before the counts were read. */
    public fun commentCount(cursor: Long): Long? = commentCounts[cursor]

    /** Chips for the comments at [commentCursors] under the post at
     *  [postCursor] (at most 50 a call). */
    public suspend fun commentChips(postCursor: Long, commentCursors: List<Long>): Map<Long, List<Chip>> {
        if (commentCursors.isEmpty()) return emptyMap()
        val epoch = reader.epochOf(postCursor) ?: return emptyMap()
        val seed = reader.seedFor(epoch) ?: return emptyMap()
        val counts = client.commentReactionCounts(channelId, postCursor.toULong(), commentCursors.takeLast(50))
        val out = HashMap<Long, List<Chip>>()
        ChannelCommentReactionKey.derive(seed, channelBytes, epoch).use { key ->
            for (c in commentCursors.takeLast(50)) {
                val own = sync()?.commentReactionsOn(channelBytes, postCursor.toULong(), c.toULong()).orEmpty().toSet()
                val resolved = key.resolveCounts(postCursor.toULong(), c.toULong(), candidates(own), counts[c].orEmpty())
                val chips = resolved.map { Chip(it.emoji, it.count.toLong(), it.emoji in own) }.toMutableList()
                for (e in own) if (chips.none { it.emoji == e }) chips += Chip(e, 1, true)
                if (chips.isNotEmpty()) out[c] = chips
            }
        }
        return out
    }

    /** Section 13a (phase B) — add or take off [emoji] on a comment, as on a
     *  post: the own record first, the write after; a write that does not
     *  land stays owed and is sent again app-wide ([ChannelFeedbackOutbox]). */
    public suspend fun toggleCommentReaction(postCursor: Long, commentCursor: Long, emoji: String) {
        val acct = account()
        val epoch = reader.epochOf(postCursor) ?: return
        val seed = reader.seedFor(epoch) ?: return
        // Section 13a (phase B) — the account's own comment reactions live in
        // the synced register: every device of the account sees them, and the
        // register holds the three-per-comment limit.
        val sync = sync() ?: return
        val held = sync.commentReactionsOn(channelBytes, postCursor.toULong(), commentCursor.toULong())
        val adding = emoji !in held
        if (adding && held.size >= MAX_OWN_REACTIONS) return
        val recorded = sync.setCommentReaction(
            channelBytes,
            postCursor.toULong(),
            commentCursor.toULong(),
            emoji,
            adding,
            postAt(postCursor),
        )
        if (!recorded) return
        val label = ChannelCommentReactionKey.derive(seed, channelBytes, epoch).use {
            it.label(postCursor.toULong(), commentCursor.toULong(), emoji)
        }
        val op = ChannelFeedbackOutbox.Op(
            channelId = channelId,
            postCursor = postCursor,
            commentCursor = commentCursor,
            labelHex = hex(label),
            add = adding,
        )
        ChannelFeedbackOutbox.put(context, acct, op)
        ChannelFeedbackOutbox.send(context, state, client, acct, op)
    }

    /** Section 13.1 — at most 3 reactions per person on a post. */
    public fun ownCount(cursor: Long): Int =
        state.boundAccount?.let { ownReactions(it, cursor).size } ?: 0

    /** Section 13a — this account's reactions on a post: the synced register
     *  (every device's) and this device's own record. */
    private fun ownReactions(acct: String, cursor: Long): Set<String> =
        OwnChannelFeedbackStore.reactions(context, acct, channelId, cursor) +
            (sync()?.channelReactionsOn(channelBytes, cursor.toULong()).orEmpty())

    private fun sync() = me.lakko.app.devices.sync.DeviceGroupSyncRepository.active

    private fun postAt(cursor: Long): ULong = (reader.tsOf(cursor) ?: System.currentTimeMillis()).toULong()

    /**
     * Add or take off [emoji] on the post at [cursor]. The own record changes
     * first, so the chip shows at once; the write goes out after. Taking off
     * needs no token.
     */
    public suspend fun toggle(cursor: Long, emoji: String) {
        val acct = account()
        val epoch = reader.epochOf(cursor) ?: return
        val seed = reader.seedFor(epoch) ?: return
        val held = ownReactions(acct, cursor)
        val adding = emoji !in held
        if (adding && held.size >= MAX_OWN_REACTIONS) return
        OwnChannelFeedbackStore.setReaction(context, acct, channelId, cursor, emoji, adding)
        sync()?.setChannelReaction(channelBytes, cursor.toULong(), emoji, adding, postAt(cursor))
        val label = ChannelReactionKey.derive(seed, channelBytes, epoch).use { it.label(cursor.toULong(), emoji) }
        // Section 13a — the write is owed until the service took it; a write
        // that does not land now is sent again app-wide.
        val op = ChannelFeedbackOutbox.Op(
            channelId = channelId,
            postCursor = cursor,
            commentCursor = null,
            labelHex = hex(label),
            add = adding,
        )
        ChannelFeedbackOutbox.put(context, acct, op)
        ChannelFeedbackOutbox.send(context, state, client, acct, op)
    }

    /** This account's comment author key in the channel. */
    public suspend fun ownAuthorKey(): ByteArray? {
        val pub = state.publisher ?: return null
        return pub.withIdentity(persist = false) { uniffi.lakko_ffi_uniffi.commentAuthorKey(it, channelBytes) }
    }

    /** One page of the thread under the post at [cursor], oldest first; a
     *  comment that does not open, or whose author is banned, is left out. */
    public suspend fun comments(cursor: Long, after: Long = 0): CommentPage {
        val epoch = reader.epochOf(cursor) ?: return CommentPage(emptyList(), after, false)
        val seed = reader.seedFor(epoch) ?: return CommentPage(emptyList(), after, false)
        val page = client.listComments(channelId, cursor.toULong(), after)
        val banned = reader.banList?.authors.orEmpty()
        val own = ownAuthorKey()
        val out = ArrayList<Comment>()
        for (row in page.rows) {
            val opened = try {
                uniffi.lakko_ffi_uniffi.openChannelComment(
                    seed,
                    channelBytes,
                    epoch,
                    cursor.toULong(),
                    row.writerKey,
                    row.sealed,
                )
            } catch (_: Throwable) {
                continue
            }
            if (banned.any { it.contentEquals(opened.authorKey) }) continue
            out += Comment(
                cursor = row.cursor,
                name = opened.comment.name,
                text = opened.comment.text,
                replyTo = opened.comment.replyTo?.toLong(),
                authorKey = opened.authorKey,
                own = own != null && own.contentEquals(opened.authorKey),
                tsMs = row.serverTsMs,
                writerKey = row.writerKey,
                avatar = opened.comment.avatar,
            )
        }
        return CommentPage(out, page.nextAfter, page.rows.size >= COMMENT_PAGE_LEN)
    }

    /** Write a comment under the post at [cursor]. Returns its cursor. */
    public suspend fun comment(cursor: Long, text: String, replyTo: Long?): Long {
        val acct = account()
        uniffi.lakko_ffi_uniffi.checkCommentText(text)
        val epoch = reader.epochOf(cursor) ?: throw IllegalStateException("post not readable")
        val seed = reader.seedFor(epoch) ?: throw IllegalStateException("post not readable")
        val pub = state.publisher ?: throw IllegalStateException("no identity")
        val name = ownName(acct)
        val nonce = uniffi.lakko_ffi_uniffi.newChannelCommentNonce()
        val (keyHex, sealed, proof) = pub.withIdentity(persist = false) { identity ->
            ChannelCommentWriteKey(identity, channelBytes, cursor.toULong(), nonce).use { key ->
                val sealed = uniffi.lakko_ffi_uniffi.sealChannelComment(
                    identity,
                    seed,
                    channelBytes,
                    epoch,
                    cursor.toULong(),
                    key.publicKey(),
                    FfiComment(name = name, avatar = ownAvatar(acct), text = text, replyTo = replyTo?.toULong()),
                )
                Triple(hex(key.publicKey()), sealed, key.signAdd(sealed))
            }
        }
        val commentCursor = client.addComment(channelId, cursor.toULong(), keyHex, sealed, proof)
        OwnChannelFeedbackStore.putCommentNonce(context, acct, channelId, commentCursor, nonce)
        sync()?.recordChannelComment(channelBytes, cursor.toULong(), nonce, postAt(cursor))
        return commentCursor
    }

    /** Delete a comment: this account's own with its write key, anyone's as
     *  a publisher. */
    public suspend fun deleteComment(postCursor: Long, commentCursor: Long, writerKey: ByteArray = ByteArray(0)) {
        val acct = account()
        val pub = state.publisher ?: return
        // This device's record, else the synced nonces of the account's
        // comments under the post, matched by the write key they derive.
        val synced = if (writerKey.isEmpty()) emptyList() else sync()?.channelCommentsOn(channelBytes, postCursor.toULong()).orEmpty()
        val nonce = OwnChannelFeedbackStore.commentNonce(context, acct, channelId, commentCursor)
            ?: pub.withIdentity(persist = false) { identity ->
                synced.firstOrNull { n ->
                    ChannelCommentWriteKey(identity, channelBytes, postCursor.toULong(), n).use { it.publicKey().contentEquals(writerKey) }
                }
            }
        if (nonce != null) {
            val proof = pub.withIdentity(persist = false) { identity ->
                ChannelCommentWriteKey(identity, channelBytes, postCursor.toULong(), nonce).use {
                    it.signDelete(commentCursor.toULong())
                }
            }
            client.deleteCommentAsAuthor(channelId, postCursor.toULong(), commentCursor, proof)
            OwnChannelFeedbackStore.putCommentNonce(context, acct, channelId, commentCursor, null)
            sync()?.deleteChannelComment(channelBytes, postCursor.toULong(), nonce, postAt(postCursor))
        } else {
            ChannelPublisherSigners.derive(pub, channelId).use {
                client.deleteCommentAsPublisher(channelId, postCursor.toULong(), commentCursor, it)
            }
        }
    }

    /**
     * Section 13.3 — ban an author from comments (owner and publishers): a
     * new ban list naming them, posted like the metadata record. In a private
     * channel too: a comment's author key does not lead to a subscriber.
     */
    public suspend fun ban(authorKey: ByteArray, banned: Boolean) {
        val held = reader.banList?.authors.orEmpty()
        val next = if (banned) {
            (held.filterNot { it.contentEquals(authorKey) } + listOf(authorKey)).takeLast(MAX_BANNED)
        } else {
            held.filterNot { it.contentEquals(authorKey) }
        }
        val stamp = maxOf(System.currentTimeMillis().toULong(), (reader.banList?.updatedAtMs ?: 0uL) + 1uL)
        val plaintext = uniffi.lakko_ffi_uniffi.composeBanList(FfiBanList(stamp, next))
        ChannelMetaPublisher.postRecord(state, client, channelId, plaintext)
    }

    /** Section 13a — the still of this account's profile photo, carried
     *  inside the sealed comment; `null` without one. */
    private fun ownAvatar(acct: String): uniffi.lakko_ffi_uniffi.FfiAvatarRef? {
        val id = try {
            Base64.decode(acct, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            return null
        }
        val refs = me.lakko.app.profile.OwnProfileStore.load(context, id)?.avatar ?: return null
        return refs.toFfi()?.still
    }

    private fun ownName(acct: String): String {
        val id = try {
            Base64.decode(acct, Base64.NO_WRAP)
        } catch (_: IllegalArgumentException) {
            null
        }
        val profile = id?.let { me.lakko.app.profile.OwnProfileStore.load(context, it) }
        return profile?.displayName?.takeIf { it.isNotBlank() }
            ?: profile?.username?.let { "@$it" }
            ?: "Lakko"
    }

    private companion object {
        /** Section 13.1 — reactions one person gives one post. */
        private const val MAX_OWN_REACTIONS: Int = 3

        /** The service's comment page. */
        private const val COMMENT_PAGE_LEN: Int = 20

        /** ADR-0087 section 13a — keys a list (chosen). */
        private const val MAX_BANNED: Int = 1_024

        private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }
    }
}

/** This account's own reactions and comment nonces in channels, on this
 *  device. */
public object OwnChannelFeedbackStore {
    private const val PREFS = "lakko.channels.own-feedback"

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    public fun reactions(context: Context, acct: String, channelId: String, cursor: Long): Set<String> =
        prefs(context).getStringSet("r|$acct|$channelId|$cursor", emptySet()).orEmpty().toSet()

    public fun setReaction(context: Context, acct: String, channelId: String, cursor: Long, emoji: String, on: Boolean) {
        val key = "r|$acct|$channelId|$cursor"
        val held = reactions(context, acct, channelId, cursor).toMutableSet()
        if (on) held += emoji else held -= emoji
        prefs(context).edit().putStringSet(key, held).apply()
    }

    public fun commentNonce(context: Context, acct: String, channelId: String, commentCursor: Long): ByteArray? =
        prefs(context).getString("c|$acct|$channelId|$commentCursor", null)?.let {
            try {
                Base64.decode(it, Base64.NO_WRAP)
            } catch (_: IllegalArgumentException) {
                null
            }
        }

    public fun putCommentNonce(context: Context, acct: String, channelId: String, commentCursor: Long, nonce: ByteArray?) {
        val key = "c|$acct|$channelId|$commentCursor"
        prefs(context).edit().apply {
            if (nonce == null) remove(key) else putString(key, Base64.encodeToString(nonce, Base64.NO_WRAP))
        }.apply()
    }

    /** Forget device / Delete account. */
    public fun forgetAccount(context: Context, acct: String) {
        val p = prefs(context)
        val editor = p.edit()
        p.all.keys.filter { it.startsWith("r|$acct|") || it.startsWith("c|$acct|") }
            .forEach(editor::remove)
        editor.apply()
    }
}

/**
 * ADR-0087 section 13a — reaction writes owed to the channels service. A
 * toggle records its write here before sending it; it leaves when the
 * service took it. Whatever a failed send left is sent again app-wide by the
 * session shell ([resendAll]), with or without the channel open. Each entry
 * names the label and the target, which is all the write key needs: no seed
 * is read again.
 */
public object ChannelFeedbackOutbox {
    private const val PREFS = "lakko.channels.feedback-outbox"
    private const val TAG = "ChannelFeedbackOutbox"

    /** One owed reaction write. [commentCursor] `null` = a post reaction. */
    public data class Op(
        val channelId: String,
        val postCursor: Long,
        val commentCursor: Long?,
        val labelHex: String,
        val add: Boolean,
    ) {
        internal fun slot(acct: String): String = "o|$acct|$channelId|$postCursor|${commentCursor ?: "-"}|$labelHex"
        internal fun value(): String = if (add) "add" else "del"
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Record [op]; a later op on the same label replaces an earlier one. */
    public fun put(context: Context, acct: String, op: Op) {
        prefs(context).edit().putString(op.slot(acct), op.value()).apply()
    }

    private fun parse(acct: String, key: String, value: Any?): Op? {
        val prefix = "o|$acct|"
        if (!key.startsWith(prefix)) return null
        val parts = key.removePrefix(prefix).split('|')
        if (parts.size != 4) return null
        return Op(
            channelId = parts[0],
            postCursor = parts[1].toLongOrNull() ?: return null,
            commentCursor = if (parts[2] == "-") null else parts[2].toLongOrNull() ?: return null,
            labelHex = parts[3],
            add = when (value) {
                "add" -> true
                "del" -> false
                else -> return null
            },
        )
    }

    /** The owed writes of [acct]. */
    public fun pending(context: Context, acct: String): List<Op> =
        prefs(context).all.mapNotNull { (k, v) -> parse(acct, k, v) }

    /** Send [op]; on success (or a target that is gone) it leaves the outbox.
     *  Throws on a failed send, which keeps it. */
    public suspend fun send(context: Context, state: ChannelsUiState, client: ChannelsClient, acct: String, op: Op) {
        val pub = state.publisher ?: throw IllegalStateException("no identity")
        val channelBytes = ChannelsComposer.channelIdFromHex(op.channelId)
        val label = unhex(op.labelHex) ?: run {
            drop(context, acct, op)
            return
        }
        val (keyHex, proof) = pub.withIdentity(persist = false) { identity ->
            if (op.commentCursor == null) {
                uniffi.lakko_ffi_uniffi.ChannelReactionWriteKey(identity, channelBytes, op.postCursor.toULong(), label).use { key ->
                    hex(key.publicKey()) to if (op.add) key.signAdd() else key.signDelete()
                }
            } else {
                ChannelCommentReactionWriteKey(
                    identity,
                    channelBytes,
                    op.postCursor.toULong(),
                    op.commentCursor.toULong(),
                    label,
                ).use { key -> hex(key.publicKey()) to if (op.add) key.signAdd() else key.signDelete() }
            }
        }
        try {
            when {
                op.commentCursor == null && op.add ->
                    client.addReaction(op.channelId, op.postCursor.toULong(), keyHex, op.labelHex, proof)
                op.commentCursor == null ->
                    client.deleteReaction(op.channelId, op.postCursor.toULong(), keyHex, op.labelHex, proof)
                op.add ->
                    client.addCommentReaction(op.channelId, op.postCursor.toULong(), op.commentCursor, keyHex, op.labelHex, proof)
                else ->
                    client.deleteCommentReaction(op.channelId, op.postCursor.toULong(), op.commentCursor, keyHex, op.labelHex, proof)
            }
        } catch (e: ChannelsApiException) {
            // A post or comment that is gone, or a channel that ended, takes
            // no write ever: the entry leaves. Anything else stays owed.
            if (e.status == 404 || e.status == 410) {
                drop(context, acct, op)
                return
            }
            throw e
        }
        drop(context, acct, op)
    }

    private fun drop(context: Context, acct: String, op: Op) {
        val p = prefs(context)
        // Only when nothing newer replaced it meanwhile.
        if (p.getString(op.slot(acct), null) == op.value()) p.edit().remove(op.slot(acct)).apply()
    }

    /** Send every owed write of the bound account. Best effort; what fails
     *  stays for the next pass. */
    public suspend fun resendAll(context: Context, state: ChannelsUiState, client: ChannelsClient) {
        val acct = state.boundAccount ?: return
        for (op in pending(context, acct)) {
            try {
                send(context, state, client, acct, op)
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                android.util.Log.i(TAG, "an owed reaction is not sent yet")
            }
        }
    }

    /** Forget device / Delete account. */
    public fun forgetAccount(context: Context, acct: String) {
        val p = prefs(context)
        val editor = p.edit()
        p.all.keys.filter { it.startsWith("o|$acct|") }.forEach(editor::remove)
        editor.apply()
    }

    private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }

    private fun unhex(h: String): ByteArray? {
        if (h.length % 2 != 0) return null
        return try {
            ByteArray(h.length / 2) { i -> h.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
        } catch (_: NumberFormatException) {
            null
        }
    }
}
