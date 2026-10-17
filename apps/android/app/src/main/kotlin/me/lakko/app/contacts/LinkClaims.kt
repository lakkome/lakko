// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import java.io.File
import java.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.DeviceSealedBlobStorage
import me.lakko.app.auth.KeyRotationFiles
import me.lakko.app.auth.SealedBlobStorage

/*
 * ADR-0079 (reworked 2026-10-17) - only one of the inviter's devices answers
 * a request sent through one of the account's invite links: CLAIM, THEN
 * BUILD, on the account's own DeviceGroup journal (the pattern ADR-0073 R.6
 * uses for successor groups, `chat/SuccessorClaim.kt`).
 *
 *   1. a device that decides `accept` seals a claim naming the request,
 *      records the sealed bytes, then publishes them into its DeviceGroup;
 *      it builds nothing yet;
 *   2. it reads its DeviceGroup journal past the claim and asks the core
 *      (`linkClaimOutcome`): the EARLIEST claim for the request from a
 *      device in the current roster wins, and the device whose leaf posted
 *      it builds;
 *   3. only the winner founds the one-to-one, sends the Welcome, records the
 *      contact and counts the use;
 *   4. a device that lost, or holds no claim and sees one from a device in
 *      the roster, keeps the request unshown until the link register says
 *      it was answered, checking again against the current roster at every
 *      pass: if the claim that beat it came from a device since removed,
 *      the next earliest current claim wins (possibly this device's), so a
 *      removed device cannot leave a request unanswered.
 *
 * A device recognises its OWN claim rows by their bytes: it cannot decrypt
 * its own MLS application messages, so it records each sealed claim before
 * the publish and the DeviceGroup receive loop matches every row against
 * those bytes. A crash between the publish and the recording of its id
 * loses nothing.
 *
 * Android mirror of `apps/web/src/contacts/link-claims.ts` and the iOS
 * `Contacts/LinkClaims.swift`: the pure next-step rule, the per-account
 * record of claims (not secret: a link id, public keys and ciphertext
 * already handed to the DS), and the sealed store of the requests not
 * decided yet (they carry link secrets).
 */

/** The pure claim rule (one definition, tested). */
public object LinkClaims {

    /** A claim another device of the account posted: its DS message id and
     *  the leaf key (hex) resolved from the MLS sender at receipt. */
    @Serializable
    public data class Observed(val id: Long, val author: String)

    /** This device's own claims for one request. */
    @Serializable
    public data class Own(
        /** Sealed claim messages (hex), recorded BEFORE their publish and
         *  not yet matched to a journal row. */
        val wires: List<String> = emptyList(),
        /** DS message ids of this device's claims on the journal. */
        val ids: List<Long> = emptyList(),
        /** Decided: this device answers the request, at this claim. Kept
         *  until the build has landed, so a failed build is retried. */
        val won: Long? = null,
        /** Decided lost: every claim of this device is on the journal and
         *  known, so the next passes re-check against the current roster
         *  without reading the journal again. */
        val settled: Boolean = false,
    ) {
        /** Whether this device holds any claim for the request. */
        public val holdsClaim: Boolean get() = won != null || ids.isNotEmpty() || wires.isNotEmpty()
    }

    /** The register's verdict as the rule reads it. */
    public enum class Verdict { ACCEPT, ANSWERED, DEAD }

    /** What the device does with a request carrying a link answer, now. */
    public enum class Step {
        /** The synced state is not loaded yet: keep it undecided, unshown. */
        WAIT,
        /** Answered (by another device, or the register says so): remove. */
        DROP,
        /** The link no longer works: show the card with the note. */
        CARD_DEAD,
        /** Live link: post this device's claim. */
        POST_CLAIM,
        /** Claim posted: read the journal past it and decide. */
        DECIDE,
        /** Lost: decide again against the current roster (no journal read). */
        RECHECK,
        /** Claim won: found the one-to-one (again, after a failure). */
        BUILD,
    }

    /**
     * The next step for one request.
     *
     * | own claims        | answered elsewhere | verdict  | step       |
     * |-------------------|--------------------|----------|------------|
     * | won               | any                | any      | BUILD      |
     * | any other         | any                | answered | DROP       |
     * | lost (settled)    | any                | dead     | DROP       |
     * | lost (settled)    | any                | other    | RECHECK    |
     * | posted or posting | any                | any      | DECIDE     |
     * | none              | yes                | dead     | DROP       |
     * | none              | yes                | other    | WAIT       |
     * | none              | no                 | not held | WAIT       |
     * | none              | no                 | dead     | CARD_DEAD  |
     * | none              | no                 | accept   | POST_CLAIM |
     *
     * "Answered elsewhere" is the core's `linkClaimAnsweredElsewhere`: a
     * claim from a device in the current roster. A request goes only when
     * the register says it was answered, or the link is dead while another
     * device holds the claim that answers it.
     */
    public fun step(own: Own?, answeredElsewhere: Boolean, verdict: Verdict?): Step {
        if (own != null && own.holdsClaim && own.won != null) return Step.BUILD
        if (verdict == Verdict.ANSWERED) return Step.DROP
        if (own != null && own.holdsClaim) {
            if (!own.settled) return Step.DECIDE
            return if (verdict == Verdict.DEAD) Step.DROP else Step.RECHECK
        }
        if (answeredElsewhere) return if (verdict == Verdict.DEAD) Step.DROP else Step.WAIT
        return when (verdict) {
            null -> Step.WAIT
            Verdict.ANSWERED -> Step.DROP
            Verdict.DEAD -> Step.CARD_DEAD
            Verdict.ACCEPT -> Step.POST_CLAIM
        }
    }

    /** A card already shown for a request (the link no longer worked here)
     *  that a roster device claimed: the request is answered. */
    public fun shownCardAnswered(own: Own?, answeredElsewhere: Boolean): Boolean =
        !(own?.holdsClaim ?: false) && answeredElsewhere

    /** The claims the core decides over: this device's own (authored by its
     *  own leaf key) and the others seen. */
    public fun claimsToDecide(own: Own?, seen: List<Observed>, ownKeyHex: String): List<Observed> =
        (own?.ids ?: emptyList()).map { Observed(it, ownKeyHex) } + seen

    /** Lower-case hex of `link_id ‖ requester_account_key`. */
    public fun requestKeyHex(linkId: ByteArray, requesterKey: ByteArray): String = hex(linkId + requesterKey)

    /** Lower-case hex of [bytes]. */
    public fun hex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

    /** [hex] decoded, or `null`. */
    public fun bytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        return try {
            ByteArray(hex.length / 2) { hex.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
        } catch (_: NumberFormatException) {
            null
        }
    }

    /** How many requests' seen claims are kept; past it the oldest go.
     *  Chosen, not measured (the web value). */
    public const val MAX_SEEN: Int = 512

    /** How many claims per request are kept (the lowest ids). Chosen, not
     *  measured: one per device, at most 8 conversation leaves (ADR-0050). */
    public const val MAX_CLAIMS_PER_REQUEST: Int = 16
}

/** Where the claim record of an account is kept. */
public interface LinkClaimStorage {
    public fun read(accountIdB64: String): String?
    public fun write(accountIdB64: String, json: String): Boolean
    public fun delete(accountIdB64: String)
}

/** [LinkClaimStorage] in `SharedPreferences`. */
public class PrefsLinkClaimStorage(context: Context) : LinkClaimStorage {
    private val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun read(accountIdB64: String): String? = prefs.getString(KEY_PREFIX + accountIdB64, null)

    override fun write(accountIdB64: String, json: String): Boolean =
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs.edit().putString(KEY_PREFIX + accountIdB64, json).commit()
        } ?: false

    override fun delete(accountIdB64: String) {
        prefs.edit().remove(KEY_PREFIX + accountIdB64).apply()
    }

    public companion object {
        public const val PREFS: String = "lakko-link-claims"
        public const val KEY_PREFIX: String = "v1:"
    }
}

/** In-memory [LinkClaimStorage] (tests). */
public class InMemoryLinkClaimStorage : LinkClaimStorage {
    private val rows = HashMap<String, String>()
    override fun read(accountIdB64: String): String? = synchronized(this) { rows[accountIdB64] }
    override fun write(accountIdB64: String, json: String): Boolean = synchronized(this) {
        rows[accountIdB64] = json
        true
    }
    override fun delete(accountIdB64: String): Unit = synchronized(this) { rows.remove(accountIdB64) }
}

@Serializable
private data class SeenClaims(val claims: List<LinkClaims.Observed>, val at: Long)

@Serializable
private data class ClaimBody(
    val v: Int = 2,
    val seen: Map<String, SeenClaims> = emptyMap(),
    val own: Map<String, LinkClaims.Own> = emptyMap(),
)

private val claimJson = Json { ignoreUnknownKeys = true }

/**
 * One account's claim record. Every operation reads, decides and writes
 * under one lock (shared by every record of the process), so the receive
 * loop's writes and a decision never interleave.
 */
public class LinkClaimRecord(private val accountIdB64: String, private val storage: LinkClaimStorage) {

    private fun read(): ClaimBody =
        storage.read(accountIdB64)?.let {
            try {
                claimJson.decodeFromString(ClaimBody.serializer(), it).takeIf { body -> body.v == 2 }
            } catch (_: Exception) {
                null
            }
        } ?: ClaimBody()

    private fun write(body: ClaimBody): Boolean =
        try {
            storage.write(accountIdB64, claimJson.encodeToString(ClaimBody.serializer(), body))
        } catch (_: Throwable) {
            false
        }

    private fun updateOwn(keyHex: String, f: (LinkClaims.Own) -> LinkClaims.Own): Boolean {
        val body = read()
        return write(body.copy(own = body.own + (keyHex to f(body.own[keyHex] ?: LinkClaims.Own()))))
    }

    /** The claims other devices of the account posted for the request. */
    public fun seen(keyHex: String): List<LinkClaims.Observed> =
        synchronized(LOCK) { read().seen[keyHex]?.claims ?: emptyList() }

    /** Record a claim another device posted, with the leaf key resolved at
     *  receipt. `true` when the record changed. */
    public fun recordSeen(keyHex: String, messageId: Long, authorHex: String, nowMs: Long): Boolean =
        synchronized(LOCK) {
            val body = read()
            val held = body.seen[keyHex]?.claims ?: emptyList()
            if (held.any { it.id == messageId }) return false
            val claims = (held + LinkClaims.Observed(messageId, authorHex))
                .sortedBy { it.id }
                .take(LinkClaims.MAX_CLAIMS_PER_REQUEST)
            if (claims.none { it.id == messageId }) return false
            var seen = body.seen + (keyHex to SeenClaims(claims, nowMs))
            if (seen.size > LinkClaims.MAX_SEEN) {
                val drop = seen.entries.sortedBy { it.value.at }.take(seen.size - LinkClaims.MAX_SEEN).map { it.key }
                seen = seen - drop.toSet()
            }
            write(body.copy(seen = seen))
        }

    public fun own(keyHex: String): LinkClaims.Own? = synchronized(LOCK) { read().own[keyHex] }

    /** Record a sealed claim BEFORE its publish. `false` when the write
     *  failed: a claim must not be posted unrecorded. */
    public fun addOwnWire(keyHex: String, wireHex: String): Boolean = synchronized(LOCK) {
        updateOwn(keyHex) { it.copy(wires = it.wires + wireHex) }
    }

    /** A journal row (or a publish answer) carried these sealed bytes at
     *  this id: if they are one of this device's recorded claims, file the
     *  id as its own. `true` when they were. */
    public fun recordOwnClaimRow(wireHex: String, messageId: Long): Boolean = synchronized(LOCK) {
        val body = read()
        val key = body.own.entries.firstOrNull { wireHex in it.value.wires }?.key ?: return false
        val own = body.own.getValue(key)
        val ids = if (messageId in own.ids) own.ids else (own.ids + messageId).sorted()
        write(body.copy(own = body.own + (key to own.copy(wires = own.wires - wireHex, ids = ids))))
    }

    /** `true` when some sealed claim still waits for its row. */
    public fun hasUnmatchedWires(): Boolean =
        synchronized(LOCK) { read().own.values.any { it.wires.isNotEmpty() } }

    /** After a read of the journal past every claim: sealed claims never
     *  matched to a row did not land; drop them. */
    public fun dropUnlanded(keyHex: String): Unit = synchronized(LOCK) {
        if (read().own[keyHex]?.wires.isNullOrEmpty()) return
        updateOwn(keyHex) { it.copy(wires = emptyList()) }
    }

    /** Decided lost: re-check later without reading the journal again. */
    public fun setSettled(keyHex: String): Boolean = synchronized(LOCK) {
        updateOwn(keyHex) { it.copy(settled = true) }
    }

    public fun setWon(keyHex: String, messageId: Long): Boolean = synchronized(LOCK) {
        updateOwn(keyHex) { it.copy(won = messageId) }
    }

    public fun forgetOwn(keyHex: String): Unit = synchronized(LOCK) {
        val body = read()
        if (keyHex !in body.own) return
        write(body.copy(own = body.own - keyHex))
    }

    public companion object {
        private val LOCK = Any()

        public fun onDevice(context: Context, accountIdB64: String): LinkClaimRecord =
            LinkClaimRecord(accountIdB64, PrefsLinkClaimStorage(context))

        /** Forget device: drop the account's record. */
        public fun clear(context: Context, accountIdB64: String) {
            PrefsLinkClaimStorage(context).delete(accountIdB64)
        }
    }
}

/** One request that answers one of this account's invite links and is not
 *  decided yet. [linkSecret] is SECRET. */
public data class UndecidedLinkRequest(
    val request: IncomingContactRequest,
    val linkId: ByteArray,
    val linkSecret: ByteArray,
) {
    override fun equals(other: Any?): Boolean =
        other is UndecidedLinkRequest && request == other.request &&
            linkId.contentEquals(other.linkId) && linkSecret.contentEquals(other.linkSecret)

    override fun hashCode(): Int = request.hashCode() * 31 + linkId.contentHashCode()
}

@Serializable
private data class StoredUndecided(
    val messageId: Long,
    val fromPubkeyB64: String,
    val fromAccountIdB64: String?,
    val fromPseudonymB64: String,
    val validAfterMs: Long,
    val intro: String?,
    val createdAt: String,
    val linkIdB64: String,
    val linkSecretB64: String,
)

private val UNDECIDED_LIST = ListSerializer(StoredUndecided.serializer())

private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)
private fun unb64(s: String): ByteArray = Base64.getDecoder().decode(s)

internal fun encodeUndecided(rows: List<UndecidedLinkRequest>): ByteArray =
    claimJson.encodeToString(
        UNDECIDED_LIST,
        rows.map { r ->
            StoredUndecided(
                messageId = r.request.messageId,
                fromPubkeyB64 = b64(r.request.fromPubkey),
                fromAccountIdB64 = r.request.fromAccountId?.let(::b64),
                fromPseudonymB64 = b64(r.request.fromPseudonym),
                validAfterMs = r.request.validAfterMs.toLong(),
                intro = r.request.intro,
                createdAt = r.request.createdAt,
                linkIdB64 = b64(r.linkId),
                linkSecretB64 = b64(r.linkSecret),
            )
        },
    ).toByteArray(Charsets.UTF_8)

internal fun decodeUndecided(bytes: ByteArray): List<UndecidedLinkRequest>? =
    try {
        claimJson.decodeFromString(UNDECIDED_LIST, bytes.toString(Charsets.UTF_8)).map { s ->
            UndecidedLinkRequest(
                request = IncomingContactRequest(
                    messageId = s.messageId,
                    fromPubkey = unb64(s.fromPubkeyB64),
                    fromAccountId = s.fromAccountIdB64?.let(::unb64),
                    fromPseudonym = unb64(s.fromPseudonymB64),
                    validAfterMs = s.validAfterMs.toULong(),
                    intro = s.intro,
                    createdAt = s.createdAt,
                ),
                linkId = unb64(s.linkIdB64),
                linkSecret = unb64(s.linkSecretB64),
            )
        }
    } catch (_: Exception) {
        null
    }

/**
 * ADR-0079 (reworked 2026-10-17) - the requests of the account mailbox that
 * answer one of this account's invite links and are NOT DECIDED yet (the
 * synced link records are not loaded, or this device's claim is in progress
 * or won but not yet built). Kept, never shown, until [LinkClaims.step]
 * decides them; a restart finds them here (the mailbox cursor has moved past
 * their rows). A row carries the link secret, so the list is sealed under the
 * device key, one file per account under `noBackupFilesDir`, behind the
 * account store gate.
 */
public class LinkRequestInbox(private val accountIdB64: String, private val storage: SealedBlobStorage) {

    private val name: String? get() = LinkRequestInboxFiles.nameOf(accountIdB64)

    /** The undecided requests, in arrival order. */
    public fun rows(): List<UndecidedLinkRequest> = synchronized(LOCK) { readLocked() }

    private fun readLocked(): List<UndecidedLinkRequest> {
        val n = name ?: return emptyList()
        val plain = try {
            storage.read(n)
        } catch (_: Throwable) {
            null
        } ?: return emptyList()
        return decodeUndecided(plain).orEmpty()
    }

    private fun writeLocked(rows: List<UndecidedLinkRequest>): Boolean {
        val n = name ?: return false
        return try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                if (rows.isEmpty()) storage.delete(n) else storage.write(n, encodeUndecided(rows))
                true
            } ?: false
        } catch (_: Throwable) {
            false
        }
    }

    /** Keep one more undecided request (once per `messageId`). `false` when
     *  it could not be kept (the bound, or a failed write). */
    public fun add(row: UndecidedLinkRequest): Boolean = synchronized(LOCK) {
        val all = readLocked()
        if (all.any { it.request.messageId == row.request.messageId }) return true
        if (all.size >= MAX_ROWS) return false
        writeLocked(all + row)
    }

    /** The request is decided (answered, shown, or built): it goes. */
    public fun remove(messageId: Long): Unit = synchronized(LOCK) {
        val all = readLocked()
        val kept = all.filter { it.request.messageId != messageId }
        if (kept.size != all.size) writeLocked(kept)
    }

    public companion object {
        /** How many undecided requests are kept. Chosen, not measured. */
        public const val MAX_ROWS: Int = 64
        private val LOCK = Any()

        public fun onDevice(context: Context, accountIdB64: String): LinkRequestInbox =
            LinkRequestInbox(accountIdB64, DeviceSealedBlobStorage(context.applicationContext, LinkRequestInboxFiles.DIR))
    }
}

/** The directory and file names of the sealed undecided link requests. */
public object LinkRequestInboxFiles {
    public const val DIR: String = "lakko-link-requests"
    public const val SUFFIX: String = ".requests"

    public fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(AcceptedContactsStore.canonicalAccountB64(accountIdB64))?.let { it + SUFFIX }

    /** Delete [accountIdB64]'s file without opening it (the account wipe). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val name = nameOf(accountIdB64) ?: return
        File(File(context.noBackupFilesDir, DIR), name).delete()
    }
}
