// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.HandleRoute
import me.lakko.app.discovery.HandleWriteError
import me.lakko.app.l10n.UiText
import me.lakko.app.profile.toFfi
import uniffi.lakko_ffi_uniffi.FfiHandleStatementBody
import uniffi.lakko_ffi_uniffi.FfiHandleVisibility

/*
 * ADR-0087 sections 2 to 4 and 11 — a group's @name at discovery and inside
 * the group, the Android twin of web `chat/group-handle.ts`.
 *
 * The write key names no account: `handleWriteSeed(journal root seed)`, the
 * root seed being ADR-0057's from the OWNER's account seed and the group id,
 * so only the owner writes and every device of the owner derives it. A claim
 * and a replace spend one `discovery-register` token; the answer's
 * certificate binds the name to the key, and the owner then signs the
 * in-group envelope that any member relays.
 */

/** What a public group's record says (section 4). */
public class PublicGroupFacts(
    public val name: String,
    public val description: String,
    public val memberCount: Int,
    public val knockPublicKey: ByteArray,
    public val avatar: me.lakko.app.profile.AvatarRefs?,
    /** The knock-key version [knockPublicKey] is (section 5). */
    public val knockVersion: UShort = 0u,
)

/** The ADR-0059 size band of a member count (0..5). Pure. */
public fun sizeBandOf(count: Int): Int = when {
    count >= 100_000 -> 5
    count >= 10_000 -> 4
    count >= 1_000 -> 3
    count >= 100 -> 2
    count >= 10 -> 1
    else -> 0
}

/** Section 2 — the typed `@name` with its `@` dropped, lower case. Pure. */
public fun canonicalHandleInput(typed: String): String = typed.trim().removePrefix("@").trim().lowercase()

/** Whether a typed `@name` is well formed (the username rule, in Rust). */
public fun handleWellFormed(typed: String): Boolean = try {
    uniffi.lakko_ffi_uniffi.validateUsername(canonicalHandleInput(typed))
    true
} catch (_: Throwable) {
    false
}

/** Section 2.1 — the proposed `@name` (`suggestHandle`, one rule). */
public fun suggestedGroupHandle(name: String, private: Boolean, random: ULong): String = try {
    uniffi.lakko_ffi_uniffi.suggestHandle(name, uniffi.lakko_ffi_uniffi.HandleKind.GROUP, private, random)
} catch (_: Throwable) {
    ""
}

/** A fresh random for [suggestedGroupHandle]. */
public fun handleRandom(): ULong = java.security.SecureRandom().nextLong().toULong()

/** The line of a discovery `429`: the per-network request limit. */
public val HANDLE_RATE_LIMITED: UiText = UiText("common.error.too_many_attempts")

/** Section 3.3 — the copy of a refused or failed handle write. */
public fun handleWriteFailure(cause: Throwable): UiText {
    var c: Throwable? = cause
    val seen = HashSet<Throwable>()
    while (c != null && seen.add(c)) {
        if (c is HandleWriteError) {
            return when {
                c.code == "name-taken" -> UiText("group_handle.error.held")
                c.status == 429 -> UiText("group_handle.error.too_many")
                else -> UiText("group_handle.error.failed")
            }
        }
        if (c is me.lakko.app.anon.AnonTokenQuotaExhausted) return UiText("group_handle.error.too_many")
        // A 429 from a lookup or an OPRF evaluation is the per-network limit,
        // not a refusal of the name: its own line, never "couldn't save".
        if (c is me.lakko.app.discovery.DiscoveryHttpError && c.status == 429) return HANDLE_RATE_LIMITED
        if (c is me.lakko.app.discovery.DiscoveryNetworkError) return UiText("group_handle.error.offline")
        c = c.cause
    }
    return UiText("group_handle.error.failed")
}

/** One signed in-group handle value, ready to store and send. */
public class SignedGroupHandle(
    public val stored: StoredGroupHandle,
    public val envelope: ByteArray,
    /** The lease end the write answered (section 3), when it named one. */
    public val leaseUntilMs: Long? = null,
)

/** The handle write seed of a group this account owns. The caller wipes it. */
internal fun groupHandleSeed(accountSeed: ByteArray, groupId: ByteArray): ByteArray {
    val root = uniffi.lakko_ffi_uniffi.dsJournalRootSeed(accountSeed, groupId)
    try {
        return uniffi.lakko_ffi_uniffi.handleWriteSeed(root)
    } finally {
        root.fill(0)
    }
}

/** The name search terms a public name publishes beside its @name, at most
 *  `maxHandleTerms() - 1`. */
internal fun ownNameTerms(name: String): List<String> {
    val max = try {
        uniffi.lakko_ffi_uniffi.maxHandleTerms().toInt()
    } catch (_: Throwable) {
        1
    }
    return try {
        uniffi.lakko_ffi_uniffi.nameSearchTerms(name)
    } catch (_: Throwable) {
        emptyList()
    }.take((max - 1).coerceAtLeast(0))
}

/**
 * An own name's terms for one handle write: [usernames] (the @name, and for a
 * replace the old one too) and, for a public name, the name search terms of
 * [publicName], all evaluated in ONE `POST /discovery/oprf/batch` with one
 * `discovery-register` token. A private write with an @name the taken check
 * just asked reuses that answer and sends nothing.
 */
internal class OwnHandleTerms(
    val usernames: List<uniffi.lakko_ffi_uniffi.OprfResult>,
    val nameTerms: List<uniffi.lakko_ffi_uniffi.OprfResult>,
)

internal suspend fun evaluateOwnHandleTerms(
    discovery: DiscoveryClient,
    usernames: List<String>,
    publicName: String?,
): OwnHandleTerms {
    val terms = publicName?.let(::ownNameTerms).orEmpty()
    if (terms.isEmpty()) return OwnHandleTerms(usernames.map { discovery.oprfOwnName(it) }, emptyList())
    val all = usernames.map { me.lakko.app.discovery.OwnTerm.Username(it) } +
        terms.map { me.lakko.app.discovery.OwnTerm.NameTerm(it) }
    val results = discovery.oprfOwnTerms(all)
    return OwnHandleTerms(results.take(usernames.size), results.drop(usernames.size))
}

/** The record and the term lookup keys a public name publishes. */
private fun publicContent(
    groupId: ByteArray,
    handle: String,
    handleOprf: uniffi.lakko_ffi_uniffi.OprfResult,
    nameTermOprf: List<uniffi.lakko_ffi_uniffi.OprfResult>,
    facts: PublicGroupFacts,
): Pair<ByteArray, List<ByteArray>> {
    val outputs = arrayListOf(handleOprf.output)
    val lookupKeys = arrayListOf(handleOprf.lookupKey)
    for (pair in nameTermOprf) {
        outputs += pair.output
        lookupKeys += pair.lookupKey
    }
    val now = System.currentTimeMillis().toULong()
    val payload = uniffi.lakko_ffi_uniffi.FfiHandleRecordPayload(
        kind = uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.GROUP,
        updatedAtMs = now,
        id = groupId,
        key = facts.knockPublicKey,
        knockVersion = facts.knockVersion,
        handle = handle,
        name = facts.name,
        description = facts.description,
        sizeBand = sizeBandOf(facts.memberCount).toUByte(),
        avatar = facts.avatar?.toFfi(),
    )
    val record = uniffi.lakko_ffi_uniffi.composeHandleRecord(payload, outputs, now)
    return record to lookupKeys
}

private fun signedEnvelope(
    seed: ByteArray,
    groupId: ByteArray,
    handle: String,
    public: Boolean,
    certificate: ByteArray,
    leaseUntilMs: Long? = null,
): SignedGroupHandle {
    val stamp = System.currentTimeMillis().toULong()
    val value = uniffi.lakko_ffi_uniffi.signConversationHandle(seed, groupId, stamp, public, handle, certificate)
    val envelope = uniffi.lakko_ffi_uniffi.composeConversationHandleEnvelope(value)
    return SignedGroupHandle(
        StoredGroupHandle(
            handle = handle,
            public = public,
            updatedAtMs = stamp,
            certificate = certificate,
            signature = value.signature,
        ),
        envelope,
        leaseUntilMs,
    )
}

private fun visibility(public: Boolean): FfiHandleVisibility =
    if (public) FfiHandleVisibility.PUBLIC else FfiHandleVisibility.PRIVATE

/** Take [handle] for a group this account owns (Create, or a lapsed name). */
public suspend fun claimGroupHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    groupId: ByteArray,
    handle: String,
    public: Boolean,
    facts: PublicGroupFacts?,
): SignedGroupHandle {
    val name = canonicalHandleInput(handle)
    val own = evaluateOwnHandleTerms(discovery, listOf(name), facts?.name?.takeIf { public })
    val pair = own.usernames[0]
    val seed = groupHandleSeed(accountSeed, groupId)
    try {
        val (record, terms) = if (public && facts != null) {
            publicContent(groupId, name, pair, own.nameTerms, facts)
        } else {
            null to emptyList()
        }
        val hash = uniffi.lakko_ffi_uniffi.handleContentHash(record, terms)
        val statement = uniffi.lakko_ffi_uniffi.signHandleStatement(
            seed,
            pair.lookupKey,
            FfiHandleStatementBody.Claim(visibility(public), hash),
        )
        val cert = discovery.handleWrite(HandleRoute.CLAIM, statement, record, terms)
            ?: throw IllegalStateException("handle claim answered no certificate")
        return signedEnvelope(seed, groupId, name, public, cert, discovery.lastLeaseUntilMs)
    } finally {
        seed.fill(0)
    }
}

/** Flip public / private, or republish the record and renew the lease. */
public suspend fun updateGroupHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    groupId: ByteArray,
    handle: String,
    public: Boolean,
    facts: PublicGroupFacts?,
): SignedGroupHandle {
    val name = canonicalHandleInput(handle)
    val own = evaluateOwnHandleTerms(discovery, listOf(name), facts?.name?.takeIf { public })
    val pair = own.usernames[0]
    val seed = groupHandleSeed(accountSeed, groupId)
    try {
        val (record, terms) = if (public && facts != null) {
            publicContent(groupId, name, pair, own.nameTerms, facts)
        } else {
            null to emptyList()
        }
        val hash = uniffi.lakko_ffi_uniffi.handleContentHash(record, terms)
        val statement = uniffi.lakko_ffi_uniffi.signHandleStatement(
            seed,
            pair.lookupKey,
            FfiHandleStatementBody.Update(visibility(public), hash),
        )
        val cert = discovery.handleWrite(HandleRoute.UPDATE, statement, record, terms)
            ?: throw IllegalStateException("handle update answered no certificate")
        return signedEnvelope(seed, groupId, name, public, cert, discovery.lastLeaseUntilMs)
    } finally {
        seed.fill(0)
    }
}

/** Move to a new name; the old one is held 30 days. */
public suspend fun replaceGroupHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    groupId: ByteArray,
    oldHandle: String,
    newHandle: String,
    public: Boolean,
    facts: PublicGroupFacts?,
): SignedGroupHandle {
    val name = canonicalHandleInput(newHandle)
    val own = evaluateOwnHandleTerms(
        discovery,
        listOf(canonicalHandleInput(oldHandle), name),
        facts?.name?.takeIf { public },
    )
    val old = own.usernames[0]
    val next = own.usernames[1]
    val seed = groupHandleSeed(accountSeed, groupId)
    try {
        val (record, terms) = if (public && facts != null) {
            publicContent(groupId, name, next, own.nameTerms, facts)
        } else {
            null to emptyList()
        }
        val hash = uniffi.lakko_ffi_uniffi.handleContentHash(record, terms)
        val statement = uniffi.lakko_ffi_uniffi.signHandleStatement(
            seed,
            old.lookupKey,
            FfiHandleStatementBody.Replace(next.lookupKey, visibility(public), hash),
        )
        val cert = discovery.handleWrite(HandleRoute.REPLACE, statement, record, terms)
            ?: throw IllegalStateException("handle replace answered no certificate")
        return signedEnvelope(seed, groupId, name, public, cert, discovery.lastLeaseUntilMs)
    } finally {
        seed.fill(0)
    }
}

/** Retire the name (Delete group, section 11). */
public suspend fun retireGroupHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    groupId: ByteArray,
    handle: String,
) {
    val pair = discovery.oprfUsername(canonicalHandleInput(handle))
    val seed = groupHandleSeed(accountSeed, groupId)
    try {
        val statement = uniffi.lakko_ffi_uniffi.signHandleStatement(seed, pair.lookupKey, FfiHandleStatementBody.Retire)
        discovery.handleWrite(HandleRoute.RETIRE, statement)
    } finally {
        seed.fill(0)
    }
}

/** A public group or channel found by search (section 5): what its card
 *  shows and what a request needs. */
public class FoundConversation(
    public val kind: uniffi.lakko_ffi_uniffi.FfiHandleRecordKind,
    public val id: ByteArray,
    public val knockOrReadKey: ByteArray,
    /** The knock-key version the record names (a channel: 0). */
    public val knockVersion: UShort,
    public val handle: String,
    public val name: String,
    public val description: String,
    public val sizeBand: Int,
    public val avatar: me.lakko.app.profile.AvatarRefs?,
)

/** Open one record a lookup returned with the OPRF output of the searched
 *  term; `null` for a person's record or one that does not open. */
public fun openFoundConversation(record: ByteArray, oprfOutput: ByteArray): FoundConversation? = try {
    uniffi.lakko_ffi_uniffi.openHandleRecord(record, oprfOutput)?.let { p ->
        FoundConversation(
            kind = p.kind,
            id = p.id,
            knockOrReadKey = p.key,
            knockVersion = p.knockVersion,
            handle = p.handle,
            name = p.name,
            description = p.description,
            sizeBand = p.sizeBand.toInt(),
            avatar = me.lakko.app.profile.avatarRefsOf(p.avatar),
        )
    }
} catch (_: Throwable) {
    null
}

/** Section 5 — search groups and channels: `@name` exactly, and the longest
 *  word of the query by its name terms. Two tokens at most. */
public suspend fun searchPublicConversations(discovery: DiscoveryClient, query: String): List<FoundConversation> {
    val q = query.trim()
    if (q.length < 3) return emptyList()
    val out = LinkedHashMap<String, FoundConversation>()
    val asHandle = canonicalHandleInput(q)
    if (handleWellFormed(asHandle)) {
        try {
            val pair = discovery.oprfUsername(asHandle)
            discovery.lookupHandleRecord(pair.lookupKey)?.let { rec ->
                openFoundConversation(rec, pair.output)?.let { out[it.id.joinToString("") { b -> "%02x".format(b) }] = it }
            }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            // A failed exact lookup still lets the name search answer.
        }
    }
    if (!q.startsWith("@")) {
        val word = q.split(Regex("\\s+")).maxByOrNull { it.length }.orEmpty()
        val term = try {
            uniffi.lakko_ffi_uniffi.nameSearchTerms(word).maxByOrNull { it.length }
        } catch (_: Throwable) {
            null
        }
        if (term != null) {
            val pair = discovery.oprfNameTerm(term)
            for (rec in discovery.nameLookupHandles(pair.lookupKey)) {
                val found = openFoundConversation(rec, pair.output) ?: continue
                // The client filters by the whole query (section 4).
                if (!found.name.contains(q, ignoreCase = true) && !found.handle.contains(asHandle)) continue
                out.putIfAbsent(found.id.joinToString("") { "%02x".format(it) }, found)
            }
        }
    }
    return out.values.toList()
}

/**
 * ADR-0087 section 1 (one namespace) — take a channel's @name on the handle
 * mechanism. The write key comes from the channel's PublisherGroup journal
 * root ([journalId], the owner's). A channel that has an @name is found by it
 * (the creation form's existing intent), so the name is public with a record
 * of kind channel. The record's read key is the phase B public-seed key; until
 * that is built the field carries 32 zero bytes and no client reads it.
 */
public suspend fun claimChannelHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    journalId: ByteArray,
    channelId: ByteArray,
    handle: String,
    name: String,
    description: String,
): SignedGroupHandle = writeChannelHandle(discovery, accountSeed, journalId, channelId, handle, name, description, update = false)

/**
 * group-and-channel-privacy.md 4.5.4 — republish a channel's public record
 * after its name or description changed: the same record as the claim, under
 * the `update` route (no token). The owner's device only: it holds the
 * PublisherGroup journal root the write key comes from.
 */
public suspend fun updateChannelHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    journalId: ByteArray,
    channelId: ByteArray,
    handle: String,
    name: String,
    description: String,
): SignedGroupHandle = writeChannelHandle(discovery, accountSeed, journalId, channelId, handle, name, description, update = true)

private suspend fun writeChannelHandle(
    discovery: DiscoveryClient,
    accountSeed: ByteArray,
    journalId: ByteArray,
    channelId: ByteArray,
    handle: String,
    name: String,
    description: String,
    update: Boolean,
): SignedGroupHandle {
    val canonical = canonicalHandleInput(handle)
    val own = evaluateOwnHandleTerms(discovery, listOf(canonical), name)
    val pair = own.usernames[0]
    val seed = groupHandleSeed(accountSeed, journalId)
    try {
        val outputs = arrayListOf(pair.output)
        val terms = arrayListOf(pair.lookupKey)
        for (p in own.nameTerms) {
            outputs += p.output
            terms += p.lookupKey
        }
        val now = System.currentTimeMillis().toULong()
        val payload = uniffi.lakko_ffi_uniffi.FfiHandleRecordPayload(
            kind = uniffi.lakko_ffi_uniffi.FfiHandleRecordKind.CHANNEL,
            updatedAtMs = now,
            id = channelId,
            key = ByteArray(32),
            handle = canonical,
            name = name,
            description = description,
            sizeBand = 0u,
            avatar = null,
        )
        val record = uniffi.lakko_ffi_uniffi.composeHandleRecord(payload, outputs, now)
        val hash = uniffi.lakko_ffi_uniffi.handleContentHash(record, terms)
        val body = if (update) {
            FfiHandleStatementBody.Update(FfiHandleVisibility.PUBLIC, hash)
        } else {
            FfiHandleStatementBody.Claim(FfiHandleVisibility.PUBLIC, hash)
        }
        val statement = uniffi.lakko_ffi_uniffi.signHandleStatement(seed, pair.lookupKey, body)
        val cert = discovery.handleWrite(if (update) HandleRoute.UPDATE else HandleRoute.CLAIM, statement, record, terms)
            ?: throw IllegalStateException("handle write answered no certificate")
        return signedEnvelope(seed, journalId, canonical, true, cert, discovery.lastLeaseUntilMs)
    } finally {
        seed.fill(0)
    }
}

/**
 * ADR-0087 section 3 — after the owner's ADR-0074 rotation the journal root,
 * and so the handle key, moved: sign `rekey` with the OLD handle seed naming
 * the new write key, then sign the in-group value with the new key under the
 * certificate the answer carries.
 */
public suspend fun rekeyGroupHandle(
    discovery: DiscoveryClient,
    oldHandleSeed: ByteArray,
    newAccountSeed: ByteArray,
    groupId: ByteArray,
    handle: String,
    public: Boolean,
): SignedGroupHandle {
    val name = canonicalHandleInput(handle)
    val pair = discovery.oprfOwnName(name)
    val newSeed = groupHandleSeed(newAccountSeed, groupId)
    try {
        val newWritePk = uniffi.lakko_ffi_uniffi.handleWritePublicKey(newSeed)
        val statement = uniffi.lakko_ffi_uniffi.signHandleStatement(
            oldHandleSeed,
            pair.lookupKey,
            FfiHandleStatementBody.Rekey(newWritePk),
        )
        val cert = discovery.handleWrite(HandleRoute.REKEY, statement)
            ?: throw IllegalStateException("handle rekey answered no certificate")
        return signedEnvelope(newSeed, groupId, name, public, cert, discovery.lastLeaseUntilMs)
    } finally {
        newSeed.fill(0)
    }
}
