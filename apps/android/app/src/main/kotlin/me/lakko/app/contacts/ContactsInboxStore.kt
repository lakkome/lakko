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
        val key = cursorKey(accountIdB64)
        val advanced = advanceInboxCursor(prefs.getLong(key, 0L), cursor)
        prefs.edit().putLong(key, advanced).apply()
    }

    override fun savePending(accountIdB64: String, pending: List<IncomingContactRequest>) {
        prefs.edit().putString(pendingKey(accountIdB64), encodePendingRequests(pending)).apply()
    }

    override fun delete(accountIdB64: String) {
        prefs.edit()
            .remove(cursorKey(accountIdB64))
            .remove(pendingKey(accountIdB64))
            .remove(waitingKey(accountIdB64))
            .apply()
    }

    private companion object {
        const val PREFS = "lakko-contacts-inbox"
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

    override fun delete(accountIdB64: String): Unit = synchronized(this) {
        cursors.remove(accountIdB64)
        pendingByAccount.remove(accountIdB64)
        waitingByAccount.remove(accountIdB64)
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
        val duplicateRow = !seenIds.add(req.messageId)
        val alreadyAccepted = acceptedKeys.any {
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
    return pending.filterNot { it.fromPubkey.toList() in acceptedKeys }
}

/** The contact list after a batch of accept acknowledgements was applied. */
public data class AppliedContactAcks(
    val accepted: List<AcceptedContact>,
    val sent: List<SentContactRequest>,
    /** `true` when [accepted] differs from the input and must be persisted. */
    val acceptedChanged: Boolean,
)

/**
 * Fold verified `ContactAck`s into the contact list: the person who pressed
 * Accept on one of this device's outgoing requests becomes a contact on this
 * side too, keyed on their signature key, carrying their account id, and the
 * matching sent request is retired.
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
): AppliedContactAcks {
    if (acks.isEmpty()) return AppliedContactAcks(accepted, sent, acceptedChanged = false)
    val encoder = java.util.Base64.getEncoder()
    var nextAccepted = accepted
    var nextSent = sent
    var changed = false
    for (ack in acks) {
        val accepterAccountIdB64 = encoder.encodeToString(ack.accepterAccountId)
        nextSent = nextSent.filterNot { it.recipientAccountIdB64 == accepterAccountIdB64 }
        val index = nextAccepted.indexOfFirst { it.request.fromPubkey.contentEquals(ack.accepterPubkey) }
        if (index >= 0) {
            val known = nextAccepted[index]
            if (known.request.fromAccountId == null) {
                nextAccepted = nextAccepted.toMutableList().also {
                    it[index] = known.copy(
                        request = known.request.copy(fromAccountId = ack.accepterAccountId),
                    )
                }
                changed = true
            }
            continue
        }
        nextAccepted = nextAccepted + AcceptedContact(
            request = IncomingContactRequest(
                messageId = ack.messageId,
                fromPubkey = ack.accepterPubkey,
                fromAccountId = ack.accepterAccountId,
                fromPseudonym = ack.ackToPseudonym,
                validAfterMs = ack.acceptedAtMs,
                intro = null,
                createdAt = ack.createdAt,
            ),
            groupIdHex = null,
        )
        changed = true
    }
    return AppliedContactAcks(nextAccepted, nextSent, changed)
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
            storage.set(OWNER_PREFIX + accountIdB64, keyB64)
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
            storage.set(CURSOR_PREFIX + accountIdB64 + ":" + keyB64, cursor.toString())
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
