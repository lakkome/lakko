// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode

/**
 * On-device persistence for one account's STRANGER-INBOX state: the DS
 * cursor the next `pollStrangerInbox` resumes from, and the verified incoming
 * requests still waiting for Accept / Decline.
 *
 * Before this store the cursor and the pending list lived in memory only, so
 * every relaunch re-fetched the whole inbox from cursor `0` (observed live on
 * 2026-09-16): contacts accepted long ago reappeared under "Requests", the
 * pending list grew a duplicate per relaunch, and a declined request came
 * back. The accepted list ([AcceptedContactsStore]) and the ADR-0050 device
 * mailbox cursor were already persisted; this is the missing third piece and
 * it leaves both of those untouched.
 *
 * Keyed per account (like the device-mailbox cursor in `ChatRepository`), in
 * plain `SharedPreferences` under the names below. The content is non-secret
 * relationship metadata the contacts UI already shows (a peer's pubkey,
 * account id, pseudonym and decrypted intro); it never reaches a server
 * (invariants #1 / #2 unaffected). A request's ADR-0048 `profile` is NOT
 * persisted here, mirroring the accepted store: `ProfileDirectory` keeps the
 * profile by signature key, and the request card resolves the name from it.
 *
 * Preference file `lakko-contacts-inbox`, keys:
 *   * `cursor.v1:<accountIdB64>`  — the inbox cursor (Long)
 *   * `pending.v1:<accountIdB64>` — the pending requests (JSON)
 */
public interface ContactsInboxStore {
    /** The persisted snapshot; the empty snapshot on first use or a decode
     *  failure (a forward-compat / corruption guard). */
    public fun load(accountIdB64: String): ContactsInboxSnapshot

    /** Persist [cursor]. A value below the stored one is ignored: the cursor
     *  never moves backwards, whatever the caller hands in. */
    public fun saveCursor(accountIdB64: String, cursor: Long)

    /** Persist the pending list as-is (an empty list is a valid state). */
    public fun savePending(accountIdB64: String, pending: List<IncomingContactRequest>)

    /**
     * ADR-0073 R.7 case 1 / UX 16.6 — persist the requests from EXISTING
     * contacts that have no verdict yet ([withWaitingRequest]). They are
     * never shown; the mailbox cursor has moved past them, so this is the
     * only place they survive a process death. The default keeps nothing (a
     * store that predates the arm).
     */
    public fun saveWaiting(accountIdB64: String, waiting: List<IncomingContactRequest>) {}

    /**
     * `docs/ux/who-can-find-me.md` 6.4 — persist the opened group contact
     * requests not yet through an inbox pass, DURABLY (the write is on disk
     * when this returns `true`): the group journal's cursor moves past their
     * rows only after this. `false` when the write did not land.
     */
    public fun saveGroupRequests(accountIdB64: String, requests: List<StoredGroupRequest>): Boolean = true

    /** What [saveGroupRequests] last stored for [accountIdB64]. */
    public fun loadGroupRequests(accountIdB64: String): List<StoredGroupRequest> = emptyList()

    /** Drop every entry for [accountIdB64] (forget device only; log out
     *  keeps them for the next login of the same account). */
    public fun delete(accountIdB64: String)
}

/** What [ContactsInboxStore] holds for one account. */
public data class ContactsInboxSnapshot(
    val cursor: Long = 0L,
    val pending: List<IncomingContactRequest> = emptyList(),
    /** Requests from existing contacts waiting, unshown, for a verdict. */
    val waiting: List<IncomingContactRequest> = emptyList(),
)

/** How many unshown waiting requests are kept. Chosen: a bound, not a
 *  measurement (mirror of the web `MAX_WAITING_REQUESTS`). */
public const val MAX_WAITING_REQUESTS: Int = 32

/** Outcome of [withWaitingRequest]. */
public data class WaitingRequests(val waiting: List<IncomingContactRequest>, val kept: Boolean)

/**
 * The waiting list after one more request is held: appended once (by DS
 * message id), and refused when the list is full — the caller then lets the
 * request go, and the sender's next request is judged afresh.
 */
public fun withWaitingRequest(waiting: List<IncomingContactRequest>, request: IncomingContactRequest): WaitingRequests {
    if (waiting.any { it.messageId == request.messageId }) return WaitingRequests(waiting, kept = true)
    if (waiting.size >= MAX_WAITING_REQUESTS) return WaitingRequests(waiting, kept = false)
    return WaitingRequests(waiting + request, kept = true)
}

/** The production store over [SharedPreferences]. */
public class SharedPreferencesContactsInboxStore(context: Context) : ContactsInboxStore {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String): ContactsInboxSnapshot {
        val cursor = prefs.getLong(cursorKey(accountIdB64), 0L)
        val pending = prefs.getString(pendingKey(accountIdB64), null)
            ?.let { decodePendingRequests(it) }
            ?: emptyList()
        val waiting = prefs.getString(waitingKey(accountIdB64), null)
            ?.let { decodePendingRequests(it) }
            ?.take(MAX_WAITING_REQUESTS)
            ?: emptyList()
        return ContactsInboxSnapshot(cursor = cursor, pending = pending, waiting = waiting)
    }

    override fun saveWaiting(accountIdB64: String, waiting: List<IncomingContactRequest>) {
        val edit = prefs.edit()
        if (waiting.isEmpty()) {
            edit.remove(waitingKey(accountIdB64))
        } else {
            edit.putString(waitingKey(accountIdB64), encodePendingRequests(waiting.take(MAX_WAITING_REQUESTS)))
        }
        edit.apply()
    }

    override fun saveCursor(accountIdB64: String, cursor: Long) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val key = cursorKey(accountIdB64)
            val advanced = advanceInboxCursor(prefs.getLong(key, 0L), cursor)
            prefs.edit().putLong(key, advanced).apply()
        }
    }

    override fun savePending(accountIdB64: String, pending: List<IncomingContactRequest>) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs.edit().putString(pendingKey(accountIdB64), encodePendingRequests(pending)).apply()
        }
    }

    override fun saveGroupRequests(accountIdB64: String, requests: List<StoredGroupRequest>): Boolean {
        var written = false
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val edit = prefs.edit()
            if (requests.isEmpty()) {
                edit.remove(groupKey(accountIdB64))
            } else {
                edit.putString(groupKey(accountIdB64), encodeGroupRequests(requests))
            }
            // `commit`, not `apply`: on disk before the journal cursor moves.
            written = edit.commit()
        }
        return written
    }

    override fun loadGroupRequests(accountIdB64: String): List<StoredGroupRequest> =
        prefs.getString(groupKey(accountIdB64), null)?.let { decodeGroupRequests(it) } ?: emptyList()

    override fun delete(accountIdB64: String) {
        prefs.edit()
            .remove(cursorKey(accountIdB64))
            .remove(pendingKey(accountIdB64))
            .remove(waitingKey(accountIdB64))
            .remove(groupKey(accountIdB64))
            .apply()
    }

    private companion object {
        const val PREFS = "lakko-contacts-inbox"
        fun groupKey(accountIdB64: String) = "group-requests.v1:$accountIdB64"
        fun cursorKey(accountIdB64: String) = "cursor.v1:$accountIdB64"
        fun pendingKey(accountIdB64: String) = "pending.v1:$accountIdB64"
        fun waitingKey(accountIdB64: String) = "waiting.v1:$accountIdB64"
    }
}

/** Host-testable store with the same contract, for unit tests and for a
 *  repository built without a `Context`. */
public class InMemoryContactsInboxStore : ContactsInboxStore {
    private val cursors = HashMap<String, Long>()
    private val pendingByAccount = HashMap<String, String>()
    private val waitingByAccount = HashMap<String, String>()

    override fun load(accountIdB64: String): ContactsInboxSnapshot = synchronized(this) {
        ContactsInboxSnapshot(
            cursor = cursors[accountIdB64] ?: 0L,
            pending = pendingByAccount[accountIdB64]?.let { decodePendingRequests(it) } ?: emptyList(),
            waiting = waitingByAccount[accountIdB64]?.let { decodePendingRequests(it) } ?: emptyList(),
        )
    }

    override fun saveWaiting(accountIdB64: String, waiting: List<IncomingContactRequest>): Unit = synchronized(this) {
        if (waiting.isEmpty()) {
            waitingByAccount.remove(accountIdB64)
        } else {
            waitingByAccount[accountIdB64] = encodePendingRequests(waiting.take(MAX_WAITING_REQUESTS))
        }
    }

    override fun saveCursor(accountIdB64: String, cursor: Long): Unit = synchronized(this) {
        cursors[accountIdB64] = advanceInboxCursor(cursors[accountIdB64] ?: 0L, cursor)
    }

    override fun savePending(accountIdB64: String, pending: List<IncomingContactRequest>): Unit =
        synchronized(this) {
            pendingByAccount[accountIdB64] = encodePendingRequests(pending)
        }

    private val groupByAccount = HashMap<String, String>()

    /** Tests: make the next [saveGroupRequests] fail (a write that did not land). */
    @Volatile
    public var failGroupWrites: Boolean = false

    override fun saveGroupRequests(accountIdB64: String, requests: List<StoredGroupRequest>): Boolean =
        synchronized(this) {
            if (failGroupWrites) return@synchronized false
            if (requests.isEmpty()) groupByAccount.remove(accountIdB64) else groupByAccount[accountIdB64] = encodeGroupRequests(requests)
            true
        }

    override fun loadGroupRequests(accountIdB64: String): List<StoredGroupRequest> = synchronized(this) {
        groupByAccount[accountIdB64]?.let { decodeGroupRequests(it) } ?: emptyList()
    }

    override fun delete(accountIdB64: String): Unit = synchronized(this) {
        cursors.remove(accountIdB64)
        pendingByAccount.remove(accountIdB64)
        waitingByAccount.remove(accountIdB64)
        groupByAccount.remove(accountIdB64)
    }
}

/**
 * `docs/ux/who-can-find-me.md` 6.4 — one opened group contact request waiting
 * for an inbox pass: the verified request wire (its secret part stays sealed
 * to this account's key), the group it came in, its row id and stamp.
 */
@Serializable
public data class StoredGroupRequest(
    val messageId: Long,
    val groupIdHex: String,
    val wireB64: String,
    val createdAt: String,
)

private fun encodeGroupRequests(requests: List<StoredGroupRequest>): String = inboxJson.encodeToString(requests)

private fun decodeGroupRequests(raw: String): List<StoredGroupRequest>? =
    try {
        inboxJson.decodeFromString<List<StoredGroupRequest>>(raw)
    } catch (_: Exception) {
        null
    }

/**
 * `docs/ux/who-can-find-me.md` 6.4 — the opened group contact requests of one
 * account between their group row and the inbox pass that lists them. A
 * request is written to [store] when it is opened ([add]); a write that does
 * not land keeps it in memory and [flushOrThrow] — called before the journal
 * cursor moves past its row — throws until it does, so a cursor never passes
 * an unstored request. The inbox pass reads them ([all]) and, once the
 * pending list holding them is persisted, lets them go ([remove]).
 */
public class GroupRequestQueue(
    private val store: ContactsInboxStore,
    private val accountIdB64: String,
) {
    private val unstored = ArrayList<StoredGroupRequest>()

    /** Keep [request]; `true` when it is on disk now. */
    public fun add(request: StoredGroupRequest): Boolean = synchronized(this) {
        if (unstored.none { it.messageId == request.messageId }) unstored.add(request)
        flushLocked()
    }

    /** Write what is held only in memory; throws when that write fails. */
    public fun flushOrThrow(): Unit = synchronized(this) {
        if (!flushLocked()) throw IllegalStateException("group contact requests not stored")
    }

    /** Every request waiting for the inbox pass, stored or not. */
    public fun all(): List<StoredGroupRequest> = synchronized(this) {
        val stored = store.loadGroupRequests(accountIdB64)
        stored + unstored.filter { u -> stored.none { it.messageId == u.messageId } }
    }

    /** The inbox pass listed (and persisted) [messageIds]: drop them. */
    public fun remove(messageIds: Set<Long>): Unit = synchronized(this) {
        if (messageIds.isEmpty()) return@synchronized
        unstored.removeAll { it.messageId in messageIds }
        val stored = store.loadGroupRequests(accountIdB64)
        val next = stored.filterNot { it.messageId in messageIds }
        if (next.size != stored.size) store.saveGroupRequests(accountIdB64, next)
    }

    private fun flushLocked(): Boolean {
        if (unstored.isEmpty()) return true
        val stored = store.loadGroupRequests(accountIdB64)
        val next = stored + unstored.filter { u -> stored.none { it.messageId == u.messageId } }
        if (!store.saveGroupRequests(accountIdB64, next)) return false
        unstored.clear()
        return true
    }
}

// =============================================================================
// Pure rules — no Android dependency, unit-tested on the host JVM.
// =============================================================================

/** The cursor to keep after a batch that reported [fetched]: never below the
 *  [current] one, so a stale or reordered batch cannot wind the inbox back
 *  and re-deliver requests already seen. */
public fun advanceInboxCursor(current: Long, fetched: Long): Long = maxOf(current, fetched)

/** Result of [mergeFetchedRequests]: the new pending list and what was not
 *  added to it (surfaced to the caller for a debug log, never to the UI). */
public data class FetchedRequestsMerge(
    val pending: List<IncomingContactRequest>,
    val dropped: List<IncomingContactRequest>,
)

/**
 * Fold a fetched batch into the pending list. A fetched request is DROPPED
 * when its `messageId` is already pending (the same DS row seen twice — a
 * re-poll from an older cursor, or a duplicate inside one batch) or when its
 * `fromPubkey` is already an accepted contact (a re-send from someone the
 * user already accepted; surfacing that is a later design choice, for now it
 * is not a request). Order is preserved: existing rows first, then the kept
 * fetched rows in arrival order.
 *
 * ADR-0074 section 3.5a rule 5 - one request per person across a PROVEN
 * rotation of their account keys ([supersedes]: the first key is the proven
 * successor of the second). A fetched request from the successor of a
 * pending row's key REPLACES that row in place, silently (the old request's
 * reply grant names a mailbox its sender left); a fetched request from a
 * key some pending row's key already superseded is dropped; and a request
 * from the successor of an accepted contact's key is that contact's. An
 * unproven key change is two people and two rows.
 */
public fun mergeFetchedRequests(
    pending: List<IncomingContactRequest>,
    accepted: List<AcceptedContact>,
    fetched: List<IncomingContactRequest>,
    supersedes: (newKey: ByteArray, oldKey: ByteArray) -> Boolean = { _, _ -> false },
): FetchedRequestsMerge {
    val seenIds = pending.mapTo(HashSet()) { it.messageId }
    val acceptedKeys = accepted.map { it.request.fromPubkey }
    val rows = ArrayList(pending)
    val dropped = ArrayList<IncomingContactRequest>()
    var changed = false
    for (req in fetched) {
        // One request, two routes (`docs/ux/who-can-find-me.md` 6.4): the same
        // signed request read from the mailbox and from a group is one row.
        val sameRequest = rows.any {
            it.fromPubkey.contentEquals(req.fromPubkey) && it.validAfterMs == req.validAfterMs
        }
        val duplicateRow = !seenIds.add(req.messageId) || sameRequest
        val alreadyAccepted = !askedForChatThroughGroup(req, accepted) && acceptedKeys.any {
            it.contentEquals(req.fromPubkey) || supersedes(req.fromPubkey, it)
        }
        if (duplicateRow || alreadyAccepted) {
            dropped.add(req)
            continue
        }
        if (rows.any { !it.fromPubkey.contentEquals(req.fromPubkey) && supersedes(it.fromPubkey, req.fromPubkey) }) {
            // The person already asked from a later key.
            dropped.add(req)
            continue
        }
        val replaced = rows.indexOfFirst {
            !it.fromPubkey.contentEquals(req.fromPubkey) && supersedes(req.fromPubkey, it.fromPubkey)
        }
        if (replaced >= 0) {
            rows[replaced] = req
            // Any further row of the same earlier keys is the same person's.
            for (i in rows.indices.reversed()) {
                if (i != replaced && !rows[i].fromPubkey.contentEquals(req.fromPubkey) &&
                    supersedes(req.fromPubkey, rows[i].fromPubkey)
                ) {
                    rows.removeAt(i)
                }
            }
        } else {
            rows.add(req)
        }
        changed = true
    }
    return FetchedRequestsMerge(pending = if (changed) rows else pending, dropped = dropped)
}

/**
 * ADR-0074 section 3.5a rule 5 - the fetched requests that may be a
 * person's request sent again from a rotated key: one whose CLAIMED account
 * id is that of a pending row under ANOTHER key. Each pair
 * `(fetched key, pending key)` is put to a proof before anything is
 * replaced; the claim alone replaces nothing.
 */
public fun successorCandidates(
    pending: List<IncomingContactRequest>,
    fetched: List<IncomingContactRequest>,
): List<Triple<ByteArray, ByteArray, ByteArray>> {
    val out = ArrayList<Triple<ByteArray, ByteArray, ByteArray>>()
    for (req in fetched) {
        val accountId = req.fromAccountId ?: continue
        for (row in pending) {
            if (row.fromAccountId?.contentEquals(accountId) != true) continue
            if (row.fromPubkey.contentEquals(req.fromPubkey)) continue
            if (out.none { it.second.contentEquals(req.fromPubkey) && it.third.contentEquals(row.fromPubkey) }) {
                out += Triple(accountId, req.fromPubkey, row.fromPubkey)
            }
        }
    }
    return out
}

/** Drop the pending rows whose sender is in [accepted] — one row per person,
 *  so a person who became a contact by another route (accept, invite link, a
 *  pairing state bundle) does not also sit under "Requests". */
public fun withoutAcceptedSenders(
    pending: List<IncomingContactRequest>,
    accepted: List<AcceptedContact>,
): List<IncomingContactRequest> {
    if (pending.isEmpty() || accepted.isEmpty()) return pending
    val acceptedKeys = accepted.mapTo(HashSet()) { it.request.fromPubkey.toList() }
    return pending.filterNot { it.fromPubkey.toList() in acceptedKeys && !askedForChatThroughGroup(it, accepted) }
}

/**
 * `docs/ux/who-can-find-me.md` section 6.4 — a request that came inside a
 * shared group from someone who is already a contact with no chat on this
 * device is how they ask for one (a contact first met in a group holds no
 * grant to this inbox). Such a request is a request row like any other: Accept
 * founds the one-to-one through the reply key it carries. A contact who has a
 * chat here is not asking for one, so theirs goes as before.
 */
public fun askedForChatThroughGroup(request: IncomingContactRequest, accepted: List<AcceptedContact>): Boolean {
    if (request.viaGroupIdHex == null) return false
    val row = accepted.firstOrNull { it.request.fromPubkey.contentEquals(request.fromPubkey) } ?: return true
    return row.groupIdHex == null
}

/**
 * The Sent list once these people answered with a Welcome: a Welcome from the
 * person asked is the answer, whether or not an ack follows (an ack written
 * after the requester retired their reply key is refused).
 */
public fun withoutSentAnsweredBy(
    sent: List<SentContactRequest>,
    answeredKeysB64: Set<String>,
): List<SentContactRequest> =
    if (answeredKeysB64.isEmpty()) sent else sent.filterNot { it.recipientKeyB64 in answeredKeysB64 }

/** The contact list and the Sent list once these people answered. */
public data class AnsweredRequests(
    val accepted: List<AcceptedContact>,
    val sent: List<SentContactRequest>,
    /** `true` when [accepted] differs from the input and must be persisted. */
    val acceptedChanged: Boolean,
)

/**
 * The person asked joined this device to a one-to-one: they are a contact, in
 * the same step that retires their Sent entry, so there is no moment with
 * neither. Between the two a search row would offer a second request. The
 * account id comes from the Sent entry; [answered] is account key (standard
 * base64) -> the one-to-one. A person with no Sent entry is left to the
 * one-to-one rule.
 */
public fun applyAnsweringWelcomes(
    accepted: List<AcceptedContact>,
    sent: List<SentContactRequest>,
    answered: Map<String, String>,
    nowMs: ULong,
    createdAt: String,
): AnsweredRequests {
    var next = accepted
    var changed = false
    val decoder = java.util.Base64.getDecoder()
    for ((keyB64, groupIdHex) in answered) {
        val asked = sent.firstOrNull { it.recipientKeyB64 == keyB64 } ?: continue
        val key = decoder.decode(keyB64)
        val accountId = try {
            decoder.decode(asked.recipientAccountIdB64)
        } catch (_: IllegalArgumentException) {
            null
        }
        adoptingOneToOnePeer(next, key, accountId, groupIdHex, nowMs, createdAt)?.let {
            next = it
            changed = true
        }
        // A row the one-to-one rule wrote first may hold no account id yet.
        val index = next.indexOfFirst { it.request.fromPubkey.contentEquals(key) }
        if (index >= 0 && next[index].request.fromAccountId == null && accountId != null) {
            val row = next[index]
            next = next.toMutableList().also {
                it[index] = row.copy(request = row.request.copy(fromAccountId = accountId))
            }
            changed = true
        }
    }
    return AnsweredRequests(next, withoutSentAnsweredBy(sent, answered.keys), changed)
}

/**
 * The Sent list once [signatureKey] became a contact through their joined
 * one-to-one: a request to them (by the key an ack named, or by their account
 * id) is answered. Base walk 2026-11-04: an ack no longer retires it by itself.
 */
public fun sentAfterOneToOne(
    sent: List<SentContactRequest>,
    signatureKey: ByteArray,
    accountId: ByteArray?,
): List<SentContactRequest> {
    val enc = java.util.Base64.getEncoder()
    val keyB64 = enc.encodeToString(signatureKey)
    val accountB64 = accountId?.let(enc::encodeToString)
    return sent.filterNot { it.recipientKeyB64 == keyB64 || (accountB64 != null && it.recipientAccountIdB64 == accountB64) }
}

/** The length of a contact's account id. */
internal const val CONTACT_ACCOUNT_ID_LEN: Int = 16

/** The Sent list once the person with this account id is dismissed from it. */
public fun withoutSentTo(
    sent: List<SentContactRequest>,
    recipientAccountIdB64: String,
): List<SentContactRequest> = sent.filterNot { it.recipientAccountIdB64 == recipientAccountIdB64 }

/** Accepting someone answers a request sent to them too: it leaves Sent. */
public fun sentAfterAccepting(
    sent: List<SentContactRequest>,
    accepted: List<IncomingContactRequest>,
): List<SentContactRequest> {
    val encoder = java.util.Base64.getEncoder()
    return accepted.mapNotNull { it.fromAccountId }.fold(sent) { rest, accountId ->
        withoutSentTo(rest, encoder.encodeToString(accountId))
    }
}

/** How many incoming requests are waiting, as the Contacts list shows them. */
public fun waitingRequestCount(pending: List<IncomingContactRequest>): Int =
    pending.distinctBy { it.messageId }.size

/** The contact list after a batch of accept acknowledgements was applied. */
public data class AppliedContactAcks(
    val accepted: List<AcceptedContact>,
    val sent: List<SentContactRequest>,
    /** `true` when [accepted] differs from the input and must be persisted. */
    val acceptedChanged: Boolean,
)

/**
 * Fold verified `ContactAck`s into the contact list. A person who is already
 * a contact here (their Welcome was joined) gets their account id and the
 * matching sent request is retired. A person who is not yet a contact stays a
 * Sent request until their Welcome is joined: a contact always has a
 * one-to-one (base walk 2026-11-04); the ack only fills in the key the
 * Welcome will be matched by.
 *
 * Without this the requester never learned the accepter's account id. The
 * chat arrived by Welcome, but the contact row came only from the requester's
 * own records, which carry no account id, so everything keyed on it failed
 * for that person: a group call named them by a short pseudonym instead of
 * their name, and they could not be picked for a new group.
 *
 * An already-known contact without an account id gets the one from the ack
 * (the ack's signature was verified against the same key, so the binding is
 * the accepter's own claim, exactly as for a received request). A contact that
 * already carries an account id is left alone. Mirrors the iOS
 * `ContactsRepository` ack handling.
 */
public fun applyContactAcks(
    accepted: List<AcceptedContact>,
    sent: List<SentContactRequest>,
    acks: List<IncomingContactAck>,
    /** Joins the keys of one person across proven rotations (ADR-0074 section
     *  3.7): an ack from a rotated key is the person, not a second row. */
    samePerson: (ByteArray, ByteArray) -> Boolean = { a, b -> a.contentEquals(b) },
): AppliedContactAcks {
    if (acks.isEmpty()) return AppliedContactAcks(accepted, sent, acceptedChanged = false)
    val encoder = java.util.Base64.getEncoder()
    var nextAccepted = accepted
    var nextSent = sent
    var changed = false
    for (ack in acks) {
        val accepterAccountIdB64 = encoder.encodeToString(ack.accepterAccountId)
        val index = nextAccepted.indexOfFirst { samePerson(it.request.fromPubkey, ack.accepterPubkey) }
        if (index < 0) {
            // Base walk 2026-11-04 — an ack is not the conversation. The
            // person becomes a contact when their Welcome is joined
            // ([applyAnsweringWelcomes]); until then the request stays in
            // Sent, so a contact never shows without a one-to-one. The ack
            // names the key the Welcome will be matched by.
            val keyB64 = encoder.encodeToString(ack.accepterPubkey)
            nextSent = nextSent.map {
                if (it.recipientAccountIdB64 == accepterAccountIdB64 && it.recipientKeyB64 == null) {
                    it.copy(recipientKeyB64 = keyB64)
                } else {
                    it
                }
            }
            continue
        }
        nextSent = nextSent.filterNot { it.recipientAccountIdB64 == accepterAccountIdB64 }
        run {
            val known = nextAccepted[index]
            if (known.request.fromAccountId == null) {
                nextAccepted = nextAccepted.toMutableList().also {
                    it[index] = known.copy(
                        request = known.request.copy(fromAccountId = ack.accepterAccountId),
                    )
                }
                changed = true
            }
        }
    }
    return AppliedContactAcks(nextAccepted, nextSent, changed)
}

/** What a Sent record knew about the person it went to. */
public data class FoundName(
    /** The person's signature key, standard base64. */
    val signatureKeyB64: String,
    /** The name the row showed; `null` when it showed the username alone. */
    val displayName: String?,
    /** The username the row showed, which the search had verified. */
    val verifiedUsername: String?,
)

/**
 * The names [sent] holds for the people who just answered (an ack, or the
 * Welcome of [answeredKeysB64]), read before the answer drops the records. A
 * new contact has no name until their profile arrives, and this is what stands
 * in for it (`docs/ux/who-can-find-me.md` section 6.1).
 *
 * Matched on the KEY the request was sealed to, not the account id an ack
 * claims: anyone can sign an ack naming another account, and a name found for
 * one key must not land on a different one. A record with no name or no key
 * gives nothing, and neither does a name that is only the key's fingerprint,
 * which is a label and not a name. A found person's username is only ever kept
 * on the record when the row showed it, so it counts as verified; a request
 * sent by typing a username keeps no name and never reaches here. A person who
 * rotated keys between the search and the ack reads as before until their
 * profile arrives.
 */
public fun foundNamesOfAnswers(
    sent: List<SentContactRequest>,
    acks: List<IncomingContactAck>,
    answeredKeysB64: Set<String>,
): List<FoundName> {
    val encoder = java.util.Base64.getEncoder()
    val keys = answeredKeysB64 + acks.map { encoder.encodeToString(it.accepterPubkey) }
    return sent.mapNotNull { record ->
        val key = record.recipientKeyB64?.takeIf { it in keys } ?: return@mapNotNull null
        val name = record.recipientName?.trim()?.takeIf { it.isNotEmpty() } ?: return@mapNotNull null
        val keyBytes = try {
            java.util.Base64.getDecoder().decode(key)
        } catch (_: IllegalArgumentException) {
            return@mapNotNull null
        }
        if (name == me.lakko.app.profile.fingerprintLabel(keyBytes)) return@mapNotNull null
        val username = record.recipientUsername.takeIf { it.isNotEmpty() }
        FoundName(
            signatureKeyB64 = key,
            // A person found by username alone is listed under `@username`:
            // that is the username again, not a name they gave.
            displayName = name.takeUnless { username != null && it == "@$username" },
            verifiedUsername = username,
        )
    }.distinctBy { it.signatureKeyB64 }
}

// =============================================================================
// Codec — the same field shape as the accepted store's record, minus the
// accepted-only fields (`groupIdHex`, `displayName`). Kept separate so the
// accepted store's on-disk format stays untouched.
// =============================================================================

private val inboxJson = Json { ignoreUnknownKeys = true }

@Serializable
private data class StoredPendingRequest(
    val messageId: Long,
    val fromPubkeyB64: String,
    val fromAccountIdB64: String?,
    val fromPseudonymB64: String,
    val validAfterMs: Long,
    val intro: String?,
    val createdAt: String,
    /** ADR-0079 - the dead-link note, kept with the request. */
    /** `docs/ux/who-can-find-me.md` 6.4 - the group the request came in. */
    val viaGroupIdHex: String? = null,
    /** ADR-0089 section 2 — the request's wire, the evidence of a report. */
    val wireB64: String? = null,
)

/** JSON for [pending]; `java.util.Base64` for the byte fields so the codec
 *  runs on the host JVM. */
internal fun encodePendingRequests(pending: List<IncomingContactRequest>): String =
    inboxJson.encodeToString(
        pending.map { r ->
            StoredPendingRequest(
                messageId = r.messageId,
                fromPubkeyB64 = base64Encode(r.fromPubkey),
                fromAccountIdB64 = r.fromAccountId?.let { base64Encode(it) },
                fromPseudonymB64 = base64Encode(r.fromPseudonym),
                validAfterMs = r.validAfterMs.toLong(),
                intro = r.intro,
                createdAt = r.createdAt,
                viaGroupIdHex = r.viaGroupIdHex,
                wireB64 = r.wire?.let { base64Encode(it) },
            )
        },
    )

/** Inverse of [encodePendingRequests]; `null` when [raw] does not decode. */
internal fun decodePendingRequests(raw: String): List<IncomingContactRequest>? =
    try {
        inboxJson.decodeFromString<List<StoredPendingRequest>>(raw).map { s ->
            IncomingContactRequest(
                messageId = s.messageId,
                fromPubkey = base64Decode(s.fromPubkeyB64),
                fromAccountId = s.fromAccountIdB64?.let { base64Decode(it) },
                fromPseudonym = base64Decode(s.fromPseudonymB64),
                validAfterMs = s.validAfterMs.toULong(),
                intro = s.intro,
                createdAt = s.createdAt,
                viaGroupIdHex = s.viaGroupIdHex,
                wire = s.wireB64?.let { base64Decode(it) },
            )
        }
    } catch (_: Exception) {
        null
    }

// =============================================================================
// ADR-0074 part 3 - which mailbox the inbox cursor belongs to
// =============================================================================

/**
 * A mailbox's message ids are its own, and the account's mailbox follows its
 * published key: after a rotation of the account keys the cursor this device
 * kept belongs to the PREVIOUS key's mailbox, and reading the new mailbox
 * from it would skip everything below it. This remembers which key the kept
 * cursor belongs to and keeps a cursor per later mailbox.
 */
public class InboxMailboxStore(private val storage: me.lakko.app.auth.MarkerStorage) {
    /** The key (standard base64) whose mailbox the account's kept cursor
     *  reads, or `null` before the first poll recorded it. */
    public fun ownerKey(accountIdB64: String): String? = try {
        storage.get(OWNER_PREFIX + accountIdB64)
    } catch (_: Throwable) {
        null
    }

    public fun setOwnerKey(accountIdB64: String, keyB64: String) {
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(OWNER_PREFIX + accountIdB64, keyB64) }
        } catch (_: Throwable) {
            // Recorded at the next poll.
        }
    }

    /** The cursor of a LATER mailbox of the account (0 when never read). */
    public fun cursor(accountIdB64: String, keyB64: String): Long = try {
        storage.get(CURSOR_PREFIX + accountIdB64 + ":" + keyB64)?.toLongOrNull() ?: 0L
    } catch (_: Throwable) {
        0L
    }

    /** Move a later mailbox's cursor forward (never backwards). */
    public fun advance(accountIdB64: String, keyB64: String, cursor: Long) {
        if (cursor <= cursor(accountIdB64, keyB64)) return
        try {
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(CURSOR_PREFIX + accountIdB64 + ":" + keyB64, cursor.toString()) }
        } catch (_: Throwable) {
            // Read again from the old cursor next time.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-inbox-mailbox"
        private const val OWNER_PREFIX: String = "owner-v1:"
        private const val CURSOR_PREFIX: String = "cursor-v1:"

        /** Drop every row of [accountIdB64] (the wipe). */
        public fun deleteAccount(context: android.content.Context, accountIdB64: String) {
            val prefs = context.applicationContext
                .getSharedPreferences(PREFS_NAME, android.content.Context.MODE_PRIVATE)
            val doomed = prefs.all.keys.filter {
                it == OWNER_PREFIX + accountIdB64 || it.startsWith("$CURSOR_PREFIX$accountIdB64:")
            }
            if (doomed.isEmpty()) return
            val edit = prefs.edit()
            for (key in doomed) edit.remove(key)
            edit.commit()
        }
    }
}

/** Where one poll of the account's mailbox reads from. */
public data class InboxRead(
    /** The cursor to read after. */
    val after: Long,
    /** The kept (pre-rotation) cursor is this mailbox's own. */
    val usesKeptCursor: Boolean,
    /** The key whose mailbox the kept cursor belongs to, when it is NOT the
     *  one being read: the previous identity's mailbox continues from the
     *  kept cursor. */
    val keptCursorOwnerB64: String?,
)

/**
 * The pure rule: the kept cursor is used for the mailbox it was made in (a
 * device that never saw another key simply records the current one); any
 * other mailbox of the account has its own cursor, starting at 0.
 */
public fun inboxReadFor(ownerKeyB64: String?, currentKeyB64: String, keptCursor: Long, laterCursor: Long): InboxRead =
    if (ownerKeyB64 == null || ownerKeyB64 == currentKeyB64) {
        InboxRead(after = keptCursor, usesKeptCursor = true, keptCursorOwnerB64 = null)
    } else {
        InboxRead(after = laterCursor, usesKeptCursor = false, keptCursorOwnerB64 = ownerKeyB64)
    }
