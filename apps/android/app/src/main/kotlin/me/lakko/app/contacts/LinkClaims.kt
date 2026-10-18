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

    /** One account a founded one-to-one's Welcome goes to. */
    @Serializable
    public data class FoundedTarget(val accountKeyB64: String, val isRecipient: Boolean)

    /** The one-to-one this device founded for a request it won, recorded
     *  BEFORE any Welcome is posted, so a retry (a failed publish, a restart)
     *  delivers the SAME group's Welcome instead of founding a second one. */
    @Serializable
    public data class Founded(
        val groupIdHex: String,
        /** The Welcome (base64); emptied once every target was written. */
        val welcomeB64: String,
        /** In delivery order: this account's own mailbox first. */
        val targets: List<FoundedTarget>,
        /** The link-founded note was published into the account's
         *  DeviceGroup; no Welcome is written before it. */
        val notePosted: Boolean = false,
        val delivered: Boolean,
    )

    /**
     * The order a founded one-to-one's Welcome is written in: this account's
     * own mailbox (its other devices) FIRST, the requester's after it. So
     * whenever the requester holds the Welcome, every own device's mailbox
     * holds it too, and a device that takes the request over finds the
     * one-to-one before it could found a second. Stable within each half.
     */
    public fun <T> welcomeOrder(targets: List<T>, isRecipient: (T) -> Boolean): List<T> =
        targets.filterNot(isRecipient) + targets.filter(isRecipient)

    /** What a device that won a request does before it builds anything. */
    public enum class BuildStep {
        /** A one-to-one was founded for it here already: carry that one on. */
        RESUME,
        /** Another device of the account founded it (its note is held):
         *  never found; write the note's Welcome to the requester. */
        FORWARD,
        /** Found the one-to-one. */
        FOUND,
    }

    /** Before any device builds a one-to-one for a request (the first winner,
     *  or a device that took it over because the winner left the roster): its
     *  own founding first, else a note from another device, else found. */
    public fun buildStep(hasFounded: Boolean, hasNote: Boolean): BuildStep =
        when {
            hasFounded -> BuildStep.RESUME
            hasNote -> BuildStep.FORWARD
            else -> BuildStep.FOUND
        }

    /** A link-founded note another device of the account posted (ADR-0079):
     *  the one-to-one it founded for a request, and that group's Welcome. */
    @Serializable
    public data class HeldNote(
        /** The DS message id of the note's row: the earliest per request is kept. */
        val id: Long,
        val groupIdHex: String,
        val welcomeB64: String,
    )

    /** What the build needs from the device, injected so the order is one
     *  tested function ([runBuild]). */
    public interface BuildDeps {
        /** This device's founding for the request, if any. */
        public fun founded(): Founded?

        /** The earliest note another device posted for the request, if any. */
        public fun note(): HeldNote?

        /** Found the one-to-one locally (ONE commit with every other own
         *  device, the journal claimed, the state persisted); nothing is sent. */
        public suspend fun prepare(): Founded

        /** Record the founding durably; `false` when the write failed. */
        public fun record(founded: Founded): Boolean

        /** Publish the link-founded note into the account's DeviceGroup
         *  (once: a sealed note already recorded is not published again),
         *  read the journal past it, and answer whether its row COUNTS
         *  (sealed at the journal's epoch at its position, so every device
         *  can read it). `false` when it was straddled (posted again next
         *  pass) or not yet read; throws when the publish failed. */
        public suspend fun postNote(founded: Founded): Boolean

        /** Make the founded group a held conversation; `false` when it
         *  cannot be brought up. */
        public suspend fun hold(founded: Founded): Boolean

        /** Write the Welcome: this account's mailbox first, then the
         *  requester's. */
        public suspend fun deliver(founded: Founded)

        /** Write a note's Welcome to the requester's mailbox (a duplicate is
         *  ignored); the group's hex id when this device holds it, else `null`. */
        public suspend fun forward(note: HeldNote): String?

        /** The contact row, the ack and the use count. */
        public suspend fun finish(groupIdHex: String?)
    }

    /**
     * The build of a won request, in its one order (ADR-0079, 2026-10-17): a
     * founding of this device's own is carried on (nothing is founded twice);
     * else a note from another device means it founded already, so its
     * Welcome is written to the requester and the request answered, never
     * founded; else found, record the founding, publish the note, and only
     * after the note landed counting (its row sealed at the journal's epoch
     * there, so every device can read it) write the Welcome (own mailbox
     * first). `false` when the founding could not be recorded or the group
     * held; a throw from any step propagates with every record where it was.
     */
    public suspend fun runBuild(d: BuildDeps): Boolean {
        var founded = d.founded()
        when (buildStep(hasFounded = founded != null, hasNote = founded == null && d.note() != null)) {
            BuildStep.FORWARD -> {
                val note = d.note() ?: return false
                d.finish(d.forward(note))
                return true
            }
            BuildStep.FOUND -> {
                val made = d.prepare()
                if (!d.record(made)) return false
                founded = made
            }
            BuildStep.RESUME -> Unit
        }
        var current = founded ?: return false
        if (!current.notePosted) {
            // No Welcome on the strength of a note the other devices cannot read.
            if (!d.postNote(current)) return false
            current = current.copy(notePosted = true)
            if (!d.record(current)) return false
        }
        if (!d.hold(current)) return false
        if (!current.delivered) {
            d.deliver(current)
            current = current.copy(welcomeB64 = "", delivered = true)
            d.record(current)
        }
        d.finish(current.groupIdHex)
        return true
    }

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
        /** The one-to-one founded for the request (won claims only). */
        val founded: Founded? = null,
        /** The sealed link-founded note (hex), recorded BEFORE its publish
         *  and not yet judged at its journal row. */
        val noteWire: String? = null,
        /** The note's row counted: sealed at the journal's epoch at its
         *  position, so every device of the account can read it. */
        val noteConfirmed: Boolean = false,
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
private data class StoredNote(val note: LinkClaims.HeldNote, val at: Long)

/** The journal's epochs as this device read them (ADR-0079): the persisted
 *  form of the core's `JournalEpochs`, for one DeviceGroup. */
@Serializable
public data class StoredJournal(
    val groupIdHex: String,
    val base: Long,
    val commitRowIds: List<Long>,
    val commitEpochsAfter: List<Long>,
)

@Serializable
private data class ClaimBody(
    val v: Int = 2,
    val seen: Map<String, SeenClaims> = emptyMap(),
    val own: Map<String, LinkClaims.Own> = emptyMap(),
    /** The earliest link-founded note per request, by request key. */
    val notes: Map<String, StoredNote> = emptyMap(),
    /** The journal's epochs (ADR-0079 readability rule). */
    val journal: StoredJournal? = null,
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

    /** A journal row carried these sealed bytes at this id, and [counts] is
     *  the readability rule's answer for it (sealed at the journal's epoch at
     *  its position). A claim of this device: filed as its own when it
     *  counts, else dropped (straddled: the next pass claims again). This
     *  device's note: confirmed when it counts, else dropped (posted again).
     *  Returns `"claim"`, `"note"`, or `null`. */
    public fun judgeOwnRow(wireHex: String, messageId: Long, counts: Boolean): String? = synchronized(LOCK) {
        val body = read()
        body.own.entries.firstOrNull { wireHex in it.value.wires }?.let { (key, own) ->
            val ids = if (!counts || messageId in own.ids) own.ids else (own.ids + messageId).sorted()
            write(body.copy(own = body.own + (key to own.copy(wires = own.wires - wireHex, ids = ids))))
            return "claim"
        }
        body.own.entries.firstOrNull { it.value.noteWire == wireHex }?.let { (key, own) ->
            write(body.copy(own = body.own + (key to own.copy(noteWire = null, noteConfirmed = own.noteConfirmed || counts))))
            return "note"
        }
        null
    }

    /** `true` when some sealed claim or note still waits for its row. */
    public fun hasUnmatchedWires(): Boolean =
        synchronized(LOCK) { read().own.values.any { it.wires.isNotEmpty() || it.noteWire != null } }

    /** Record the sealed note BEFORE its publish. `false` when the write
     *  failed. */
    public fun setNoteWire(keyHex: String, wireHex: String): Boolean = synchronized(LOCK) {
        updateOwn(keyHex) { it.copy(noteWire = wireHex) }
    }

    /** After a read of the journal past the note: a sealed note no row
     *  carried did not land; drop it (posted again). */
    public fun dropUnlandedNote(keyHex: String): Unit = synchronized(LOCK) {
        if (read().own[keyHex]?.noteWire == null) return
        updateOwn(keyHex) { it.copy(noteWire = null) }
    }

    /** The journal's epochs held for the DeviceGroup [groupIdHex], or `null`
     *  (none held, or held for another DeviceGroup). */
    public fun journal(groupIdHex: String): StoredJournal? =
        synchronized(LOCK) { read().journal?.takeIf { it.groupIdHex == groupIdHex } }

    /** Keep the journal's epochs. */
    public fun setJournal(journal: StoredJournal): Boolean = synchronized(LOCK) {
        write(read().copy(journal = journal))
    }

    /** After a read of the journal past every claim: sealed claims never
     *  matched to a row did not land; drop them. */
    public fun dropUnlanded(keyHex: String): Unit = synchronized(LOCK) {
        if (read().own[keyHex]?.wires.isNullOrEmpty()) return
        updateOwn(keyHex) { it.copy(wires = emptyList()) }
    }

    /** Record the founded one-to-one (before any Welcome is posted), or its
     *  delivery. `false` when the write failed: nothing may be posted for a
     *  founding this device would not find again. */
    public fun setFounded(keyHex: String, founded: LinkClaims.Founded): Boolean = synchronized(LOCK) {
        updateOwn(keyHex) { it.copy(founded = founded) }
    }

    /** Keep a link-founded note another device posted (the earliest per
     *  request, the newest [MAX_NOTES] requests). `true` when the record
     *  changed. */
    public fun recordNote(keyHex: String, note: LinkClaims.HeldNote, nowMs: Long): Boolean = synchronized(LOCK) {
        val body = read()
        val held = body.notes[keyHex]
        if (held != null && held.note.id <= note.id) return false
        var notes = body.notes + (keyHex to StoredNote(note, nowMs))
        if (notes.size > MAX_NOTES) {
            val drop = notes.entries.sortedBy { it.value.at }.take(notes.size - MAX_NOTES).map { it.key }
            notes = notes - drop.toSet()
        }
        write(body.copy(notes = notes))
    }

    /** The earliest note held for the request, or `null`. */
    public fun note(keyHex: String): LinkClaims.HeldNote? = synchronized(LOCK) { read().notes[keyHex]?.note }

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

        /** How many link-founded notes are kept; past it the oldest go.
         *  Chosen, not measured (the web value). */
        public const val MAX_NOTES: Int = 64

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
