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
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode

/*
 * ADR-0074 section 3.5a rule 5 - this device's unanswered outgoing contact
 * requests, kept across a process death.
 *
 * After a rotation of the account keys each of them is sent again from the
 * new key (its reply grant named the old mailbox). The re-send needs the
 * recipient and the intro text, and it may run long after the request was
 * first sent, so the list is durable: one file per account under
 * `noBackupFilesDir`, sealed with the device key (the intro is the person's
 * own words to someone who has not answered yet).
 *
 * An entry leaves the list when the request is answered and the whole file
 * goes with the account wipe. This client has no "withdraw" for a sent
 * request.
 */

/** Where the unanswered outgoing requests of an account live. */
public interface SentRequestsStore {
    /** The stored list; empty when absent or unreadable. */
    public fun load(accountIdB64: String): List<SentContactRequest>

    /** Replace the stored list; an empty list removes the file. Returns
     *  whether it was written. */
    public fun save(accountIdB64: String, sent: List<SentContactRequest>): Boolean

    public fun delete(accountIdB64: String)
}

@Serializable
private data class StoredSentRequest(
    val recipientUsername: String,
    val recipientAccountIdB64: String,
    val messageId: Long,
    val createdAt: String,
    val introB64: String = "",
    val sentAtMs: Long = 0L,
)

private val sentJson = Json { ignoreUnknownKeys = true }
private val SENT_LIST = ListSerializer(StoredSentRequest.serializer())

/** Chosen, not measured: far past the requests one person has unanswered. */
internal const val MAX_STORED_SENT_REQUESTS: Int = 256

/** The stored form of [sent] (UTF-8 JSON). The newest
 *  [MAX_STORED_SENT_REQUESTS] are kept. */
internal fun encodeSentRequests(sent: List<SentContactRequest>): ByteArray =
    sentJson.encodeToString(
        SENT_LIST,
        sent.takeLast(MAX_STORED_SENT_REQUESTS).map {
            StoredSentRequest(
                recipientUsername = it.recipientUsername,
                recipientAccountIdB64 = it.recipientAccountIdB64,
                messageId = it.messageId,
                createdAt = it.createdAt,
                introB64 = base64Encode(it.introMessage),
                sentAtMs = it.sentAtMs,
            )
        },
    ).toByteArray(Charsets.UTF_8)

/** The list [bytes] hold, or `null` when they are not one. */
internal fun decodeSentRequests(bytes: ByteArray): List<SentContactRequest>? =
    try {
        sentJson.decodeFromString(SENT_LIST, bytes.toString(Charsets.UTF_8)).map {
            SentContactRequest(
                recipientUsername = it.recipientUsername,
                recipientAccountIdB64 = it.recipientAccountIdB64,
                messageId = it.messageId,
                createdAt = it.createdAt,
                introMessage = base64Decode(it.introB64),
                sentAtMs = it.sentAtMs,
            )
        }
    } catch (_: Exception) {
        null
    }

/**
 * The Sent list an account shows once its stored requests are loaded: the
 * stored entries, then the ones already in memory that the store does not
 * hold (a request sent before the account was bound). One entry per
 * Delivery Service row.
 */
internal fun mergedSentRequests(
    stored: List<SentContactRequest>,
    inMemory: List<SentContactRequest>,
): List<SentContactRequest> = (stored + inMemory).distinctBy { it.messageId }

/** [SentRequestsStore] over sealed blobs named by the account id in hex. */
public class SealedSentRequestsStore(private val storage: SealedBlobStorage) : SentRequestsStore {
    override fun load(accountIdB64: String): List<SentContactRequest> {
        val name = SentRequestsFiles.nameOf(accountIdB64) ?: return emptyList()
        val plain = try {
            storage.read(name)
        } catch (_: Throwable) {
            null
        } ?: return emptyList()
        return decodeSentRequests(plain).orEmpty()
    }

    override fun save(accountIdB64: String, sent: List<SentContactRequest>): Boolean {
        val name = SentRequestsFiles.nameOf(accountIdB64) ?: return false
        return try {
            if (sent.isEmpty()) storage.delete(name) else storage.write(name, encodeSentRequests(sent))
            true
        } catch (_: Throwable) {
            false
        }
    }

    override fun delete(accountIdB64: String) {
        SentRequestsFiles.nameOf(accountIdB64)?.let {
            try {
                storage.delete(it)
            } catch (_: Throwable) {
                // The account wipe deletes the file by name.
            }
        }
    }
}

/** The directory and file names of the sealed Sent lists. */
public object SentRequestsFiles {
    public const val DIR: String = "lakko-sent-requests"
    public const val SUFFIX: String = ".sent"

    /** `<account id in hex>.sent`, or `null` for an id that is not base64. */
    public fun nameOf(accountIdB64: String): String? =
        KeyRotationFiles.hexOf(AcceptedContactsStore.canonicalAccountB64(accountIdB64))?.let { it + SUFFIX }

    public fun onDevice(context: Context): SentRequestsStore =
        SealedSentRequestsStore(DeviceSealedBlobStorage(context.applicationContext, DIR))

    /** Delete [accountIdB64]'s file without opening it (the account wipe's
     *  step; needs no Keystore). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val name = nameOf(accountIdB64) ?: return
        File(File(context.noBackupFilesDir, DIR), name).delete()
    }
}

/** In-memory store (tests). */
public class InMemorySentRequestsStore : SentRequestsStore {
    private val rows = HashMap<String, ByteArray>()

    override fun load(accountIdB64: String): List<SentContactRequest> =
        synchronized(this) { rows[accountIdB64]?.let(::decodeSentRequests).orEmpty() }

    override fun save(accountIdB64: String, sent: List<SentContactRequest>): Boolean = synchronized(this) {
        if (sent.isEmpty()) rows.remove(accountIdB64) else rows[accountIdB64] = encodeSentRequests(sent)
        true
    }

    override fun delete(accountIdB64: String): Unit = synchronized(this) { rows.remove(accountIdB64) }
}
