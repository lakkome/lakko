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

    /** Drop both entries for [accountIdB64] (logout / forget device). */
    public fun delete(accountIdB64: String)
}

/** What [ContactsInboxStore] holds for one account. */
public data class ContactsInboxSnapshot(
    val cursor: Long = 0L,
    val pending: List<IncomingContactRequest> = emptyList(),
)

/** The production store over [SharedPreferences]. */
public class SharedPreferencesContactsInboxStore(context: Context) : ContactsInboxStore {

    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String): ContactsInboxSnapshot {
        val cursor = prefs.getLong(cursorKey(accountIdB64), 0L)
        val pending = prefs.getString(pendingKey(accountIdB64), null)
            ?.let { decodePendingRequests(it) }
            ?: emptyList()
        return ContactsInboxSnapshot(cursor = cursor, pending = pending)
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
            .apply()
    }

    private companion object {
        const val PREFS = "lakko-contacts-inbox"
        fun cursorKey(accountIdB64: String) = "cursor.v1:$accountIdB64"
        fun pendingKey(accountIdB64: String) = "pending.v1:$accountIdB64"
    }
}

/** Host-testable store with the same contract, for unit tests and for a
 *  repository built without a `Context`. */
public class InMemoryContactsInboxStore : ContactsInboxStore {
    private val cursors = HashMap<String, Long>()
    private val pendingByAccount = HashMap<String, String>()

    override fun load(accountIdB64: String): ContactsInboxSnapshot = synchronized(this) {
        ContactsInboxSnapshot(
            cursor = cursors[accountIdB64] ?: 0L,
            pending = pendingByAccount[accountIdB64]?.let { decodePendingRequests(it) } ?: emptyList(),
        )
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
 */
public fun mergeFetchedRequests(
    pending: List<IncomingContactRequest>,
    accepted: List<AcceptedContact>,
    fetched: List<IncomingContactRequest>,
): FetchedRequestsMerge {
    val seenIds = pending.mapTo(HashSet()) { it.messageId }
    val acceptedKeys = accepted.mapTo(HashSet()) { it.request.fromPubkey.toList() }
    val kept = ArrayList<IncomingContactRequest>()
    val dropped = ArrayList<IncomingContactRequest>()
    for (req in fetched) {
        val duplicateRow = !seenIds.add(req.messageId)
        val alreadyAccepted = req.fromPubkey.toList() in acceptedKeys
        if (duplicateRow || alreadyAccepted) dropped.add(req) else kept.add(req)
    }
    return FetchedRequestsMerge(pending = if (kept.isEmpty()) pending else pending + kept, dropped = dropped)
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
