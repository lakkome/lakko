// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import java.io.File
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.json.Json
import me.lakko.app.auth.DeviceSealedBlobStorage
import me.lakko.app.auth.KeyRotationFiles
import me.lakko.app.auth.SealedBlobStorage
import uniffi.lakko_ffi_uniffi.LinkRecord
import uniffi.lakko_ffi_uniffi.LinkRecordKind

/*
 * ADR-0079 section 5 — the person this account added through their invite
 * link, before their app has answered. Android mirror of
 * `apps/web/src/contacts/pending-contacts.ts` and the iOS
 * `Contacts/PendingContacts.swift`.
 *
 * After Add on the invite page the inviter is a contact at once on this side
 * (`docs/ux/contact-invite-link.md` section 4): listed in Contacts and Chats
 * under the name the link states, and written to. What is written is NOT
 * kept with the record (reworked 2026-10-17): it goes into the ONE held-send
 * queue (`chat/HeldSendQueue.kt`, sealed under the device key) under the
 * contact's [PendingContact.queueKey], the same mechanism web and iOS use;
 * when the inviter's Welcome lands, the messages move, in order, into the
 * queue of the new one-to-one, which sends them, and the record goes. The
 * records live in one file per account under `noBackupFilesDir`, sealed with
 * the device key.
 */

/** A person added through their invite link whose app has not answered. */
@Serializable
public data class PendingContact(
    /** The inviter's 16-byte account id, lower-case hex. */
    val accountIdHex: String,
    /** The inviter's account key (Ed25519), base64: what the one-to-one's
     *  roster names them by. */
    val signaturePubkeyB64: String,
    val x25519PubkeyB64: String,
    /** The display name the link states, or `""`. */
    val displayName: String,
    /** The username the link states, or `""`. */
    val username: String,
    /** When Add was pressed (Unix ms): the 7-day line counts from here. */
    val addedAtMs: Long,
    /** The id of the invite link the request answered (16 bytes, lower-case
     *  hex): a link refusal must name it (ADR-0079 amendment of 2026-10-19). */
    val linkIdHex: String,
) {
    /** The held-send queue key of what is written before the chat exists:
     *  the inviter's account id hex (32 characters, never a conversation id,
     *  which is a 64-character group id). */
    val queueKey: String get() = accountIdHex
}

/** The pure rules of the pending contacts (one definition each, tested). */
public object PendingContacts {
    /** After this long without an answer the chat line changes (UX section 4). */
    public const val STALE_MS: Long = 7L * 24 * 60 * 60 * 1000

    /** [list] with [contact] added, or the existing record for the same
     *  person kept with its first Add time (a second Add through a fresh link
     *  does not reset the 7-day line). */
    public fun adding(contact: PendingContact, list: List<PendingContact>): List<PendingContact> {
        val at = list.indexOfFirst { it.accountIdHex == contact.accountIdHex }
        if (at < 0) return list + contact
        val held = list[at]
        return list.toMutableList().also {
            it[at] = contact.copy(addedAtMs = minOf(held.addedAtMs, contact.addedAtMs))
        }
    }

    /**
     * ADR-0079 amendment of 2026-10-19 — the pending contact a link refusal
     * cancels: the one whose stated account key signed it over the link that
     * contact's request answered and this account's key ([verify] is the
     * core's `verifyLinkRefusal`). `null` when none does: a refusal nobody
     * but the inviter could sign, for another link or another person,
     * changes nothing. Mirror of web `pendingContactRefusedBy`.
     */
    public fun refusedBy(
        list: List<PendingContact>,
        refusal: ByteArray,
        ownAccountKey: ByteArray,
        verify: (ByteArray, ByteArray, ByteArray, ByteArray) -> Boolean,
    ): PendingContact? = list.firstOrNull { c ->
        val inviterKey = runCatching { java.util.Base64.getDecoder().decode(c.signaturePubkeyB64) }.getOrNull()
            ?: return@firstOrNull false
        val linkId = LinkClaims.bytes(c.linkIdHex) ?: return@firstOrNull false
        verify(refusal, inviterKey, linkId, ownAccountKey)
    }

    /** Which line the pending chat shows. */
    public enum class Line { WAITING, STALE }

    public fun line(contact: PendingContact, nowMs: Long): Line =
        if (nowMs - contact.addedAtMs >= STALE_MS) Line.STALE else Line.WAITING

    /** The pending contacts the lists show: those with no accepted contact
     *  or one-to-one yet. */
    public fun shown(list: List<PendingContact>, knownPeerKeysB64: Set<String>): List<PendingContact> =
        list.filter { it.signaturePubkeyB64 !in knownPeerKeysB64 }

    /** The pending contact a joined one-to-one belongs to: the one whose
     *  account key is one of its people. */
    public fun forConversation(list: List<PendingContact>, personKeysB64: List<String>): PendingContact? {
        val people = personKeysB64.toSet()
        return list.firstOrNull { it.signaturePubkeyB64 in people }
    }


    /** The name shown: the stated display name, else `@username`, else
     *  `null` (the caller's catalog fallback). */
    public fun name(contact: PendingContact): String? = statedName(contact.displayName, contact.username)

    /** The one rule for the name a link states. */
    public fun statedName(displayName: String?, username: String?): String? {
        val trimmed = displayName?.trim().orEmpty()
        if (trimmed.isNotEmpty()) return trimmed
        return if (username.isNullOrEmpty()) null else "@$username"
    }
}

/**
 * ADR-0079 section 4 — the rules over the synced link-record register that
 * the lists need. The verdict on a request's link answer is the core's
 * (`SyncedState.linkAnswerVerdict`), never re-derived here.
 */
public object LinkRecords {
    /** The number of uses a link allows, or `null` for unlimited (`0` is
     *  single use, `UInt.MAX_VALUE` unlimited, as `lakko-contacts` defines them). */
    public fun effectiveLimit(maxUses: UInt): UInt? = when (maxUses) {
        0u -> 1u
        UInt.MAX_VALUE -> null
        else -> maxUses
    }

    /** Not revoked, not expired, not used up. */
    public fun isLive(record: LinkRecord, nowMs: ULong): Boolean {
        if (record.revoked) return false
        if (record.expiryMs != 0uL && record.expiryMs <= nowMs) return false
        val limit = effectiveLimit(record.maxUses)
        return limit == null || record.uses < limit
    }

    /** The account's contact links the invite-links list shows: live ones
     *  with a ledger row, newest first. */
    public fun listedContactLinks(records: List<LinkRecord>, nowMs: ULong): List<LinkRecord> =
        records
            .filter { it.kind == LinkRecordKind.CONTACT && it.ledgerId.isNotEmpty() && isLive(it, nowMs) }
            .sortedByDescending { it.mintedAtMs }
}

/** Where the pending contacts of an account live. */
public interface PendingContactsStore {
    public fun load(accountIdB64: String): List<PendingContact>

    /** Replace the stored list; an empty list removes the file. */
    public fun save(accountIdB64: String, list: List<PendingContact>): Boolean
}

private val pendingJson = Json { ignoreUnknownKeys = true }
private val PENDING_LIST = ListSerializer(PendingContact.serializer())

internal fun encodePendingContacts(list: List<PendingContact>): ByteArray =
    pendingJson.encodeToString(PENDING_LIST, list).toByteArray(Charsets.UTF_8)

internal fun decodePendingContacts(bytes: ByteArray): List<PendingContact>? =
    try {
        pendingJson.decodeFromString(PENDING_LIST, bytes.toString(Charsets.UTF_8))
    } catch (_: Exception) {
        null
    }

/** [PendingContactsStore] over sealed blobs named by the account id in hex. */
public class SealedPendingContactsStore(private val storage: SealedBlobStorage) : PendingContactsStore {
    override fun load(accountIdB64: String): List<PendingContact> {
        val name = PendingContactsFiles.nameOf(accountIdB64) ?: return emptyList()
        val plain = try {
            storage.read(name)
        } catch (_: Throwable) {
            null
        } ?: return emptyList()
        return decodePendingContacts(plain).orEmpty()
    }

    override fun save(accountIdB64: String, list: List<PendingContact>): Boolean {
        val name = PendingContactsFiles.nameOf(accountIdB64) ?: return false
        return try {
            // Refused once the account's session ended (ADR-0077).
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
                if (list.isEmpty()) storage.delete(name) else storage.write(name, encodePendingContacts(list))
                true
            } ?: false
        } catch (_: Throwable) {
            false
        }
    }
}

/** The directory and file names of the sealed pending-contact lists. */
public object PendingContactsFiles {
    public const val DIR: String = "lakko-pending-contacts"
    public const val SUFFIX: String = ".pending"

    public fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(AcceptedContactsStore.canonicalAccountB64(accountIdB64))?.let { it + SUFFIX }

    public fun onDevice(context: Context): PendingContactsStore =
        SealedPendingContactsStore(DeviceSealedBlobStorage(context.applicationContext, DIR))

    /** Delete [accountIdB64]'s file without opening it (the account wipe). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val name = nameOf(accountIdB64) ?: return
        File(File(context.noBackupFilesDir, DIR), name).delete()
    }
}

/** In-memory store (tests). */
public class InMemoryPendingContactsStore : PendingContactsStore {
    private val rows = HashMap<String, ByteArray>()

    override fun load(accountIdB64: String): List<PendingContact> =
        synchronized(this) { rows[accountIdB64]?.let(::decodePendingContacts).orEmpty() }

    override fun save(accountIdB64: String, list: List<PendingContact>): Boolean = synchronized(this) {
        if (list.isEmpty()) rows.remove(accountIdB64) else rows[accountIdB64] = encodePendingContacts(list)
        true
    }
}

/**
 * The order of **Add** on a contact invite page (ADR-0079 section 5, amended
 * 2026-10-19; `docs/ux/contact-invite-link.md` section 3), with the page's
 * steps passed in so the order is unit-tested. The web
 * (`contacts/invite-add.ts`) and iOS (`InviteAddOrder`) keep the same order:
 *
 *  1. the ledger's consume: a link used up or gone since the page opened is
 *     dead, and NOTHING is kept (no pending contact, no held message);
 *  2. only then the pending contact is written, before the request is sent
 *     so an answer that comes back at once finds it;
 *  3. the request; a send that fails removes the pending contact again and
 *     the failure propagates.
 *
 * Inline, so a step may return from the caller (a refusal, a lost session).
 */
public object InviteAddOrder {
    /** How Add ended. */
    public enum class Outcome { ADDED, DEAD }

    public suspend inline fun run(
        consumeLive: suspend () -> Boolean,
        recordPending: () -> Unit,
        sendRequest: suspend () -> Unit,
        forgetPending: () -> Unit,
    ): Outcome {
        if (!consumeLive()) return Outcome.DEAD
        recordPending()
        try {
            sendRequest()
        } catch (e: Throwable) {
            forgetPending()
            throw e
        }
        return Outcome.ADDED
    }
}
