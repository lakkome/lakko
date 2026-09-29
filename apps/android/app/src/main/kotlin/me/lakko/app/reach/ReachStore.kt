// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0058 — this account's reach bookkeeping, one JSON document per account.
 *
 * Held in EncryptedSharedPreferences because the document carries SECRETS: the
 * reach grants other people handed this device (whoever holds one can write to
 * their mailbox). The owner's own seeds are never stored: every one of them is
 * derived again from the account seed and a version counter.
 */

/** A slot's version, keyed `"<kind>:<scopeHex>"`. */
@Serializable
internal data class StoredVersion(val kind: String, val scopeHex: String, val version: Int)

/** A key this device registered on the owner's mailbox. */
@Serializable
internal data class StoredRegistered(
    val pkHex: String,
    val kind: String,
    val scopeHex: String,
    val version: Int,
    val expiresAtMs: Long? = null,
)

/** A live invite link's key. */
@Serializable
internal data class StoredLink(
    val linkIdHex: String,
    val expiresAtMs: Long? = null,
    /** The ledger id the link was uploaded under (hex), once known, so a
     *  revoke by ledger id retires the right key. */
    val blobIdHex: String? = null,
)

/** An outgoing request's reply key: its scope, expiry, and who it went to
 *  (account signature key, hex) so an answer from them retires it. */
@Serializable
internal data class StoredReply(
    val scopeHex: String,
    val expiresAtMs: Long,
    val toKeyHex: String? = null,
)

/** A grant someone handed this device. SECRET. */
@Serializable
internal data class StoredHeld(
    /** The owner of the mailbox the grant opens (account signature key, hex). */
    val ownerKeyHex: String,
    val kind: String,
    /** Where it was received: group / channel id for those classes, else "". */
    val scopeHex: String,
    /** The 37-byte grant wire (`encodeReachGrant`), hex. */
    val grantHex: String,
)

/** A committer delegation envelope received in a group (ADR-0058 §6). */
@Serializable
internal data class StoredDelegation(
    val groupHex: String,
    /** The sender's account key, resolved at receipt (C-68), hex. */
    val senderKeyHex: String,
    val addedAccountKeyHex: String,
    /** The envelope plaintext, hex. */
    val envelopeHex: String,
)

/** The whole per-account document. */
@Serializable
internal data class ReachDoc(
    val versions: List<StoredVersion> = emptyList(),
    val registered: List<StoredRegistered> = emptyList(),
    val links: List<StoredLink> = emptyList(),
    val replies: List<StoredReply> = emptyList(),
    val held: List<StoredHeld> = emptyList(),
    val delegations: List<StoredDelegation> = emptyList(),
    /** The visibility setting envelope (`composeVisibilityEnvelope`), hex. */
    val visibilityHex: String? = null,
    /** The `open` flag the server was last told, or `null` when never told. */
    val registeredOpen: Boolean? = null,
    /** Every conversation group this account is in, hex. */
    val joinedGroups: List<String> = emptyList(),
    /** Every channel this account is subscribed to or publishes, hex. */
    val channels: List<String> = emptyList(),
    /** Whether the name terms were last published as Everyone. */
    val publishedPublic: Boolean? = null,
    /** The username certificate discovery signed for this account, hex. */
    val usernameCertHex: String? = null,
    /** The display name the Everyone name terms were published for. */
    val publishedName: String? = null,
)

/** Where the documents live. An interface so the manager is testable. */
internal interface ReachDocStore {
    fun load(accountIdB64: String): ReachDoc
    fun save(accountIdB64: String, doc: ReachDoc)
    fun delete(accountIdB64: String)
}

/** The production store: one EncryptedSharedPreferences file. */
internal class EncryptedReachDocStore(context: Context) : ReachDocStore {
    private val appContext = context.applicationContext
    private val json = Json {
        ignoreUnknownKeys = true
        explicitNulls = false
    }

    private val prefs by lazy {
        val master = MasterKey.Builder(appContext)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            appContext,
            PREFS_NAME,
            master,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun load(accountIdB64: String): ReachDoc {
        val raw = prefs.getString(KEY_PREFIX + accountIdB64, null) ?: return ReachDoc()
        return try {
            json.decodeFromString(ReachDoc.serializer(), raw)
        } catch (_: Throwable) {
            ReachDoc()
        }
    }

    override fun save(accountIdB64: String, doc: ReachDoc) {
        prefs.edit().putString(KEY_PREFIX + accountIdB64, json.encodeToString(ReachDoc.serializer(), doc)).apply()
    }

    override fun delete(accountIdB64: String) {
        prefs.edit().remove(KEY_PREFIX + accountIdB64).apply()
    }

    private companion object {
        const val PREFS_NAME = "lakko-reach"
        const val KEY_PREFIX = "doc.v1."
    }
}
