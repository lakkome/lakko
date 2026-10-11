// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0060 — this account's reach bookkeeping, one JSON document per account.
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
    /** ADR-0066 section 3.5a rule 6 - the grant [grantHex] replaced, kept
     *  beside it until the person's address is known current. */
    val previousGrantHex: String? = null,
)

/** A committer delegation envelope received in a group (ADR-0060 §6). */
@Serializable
internal data class StoredDelegation(
    val groupHex: String,
    /** The sender's account key, resolved at receipt (C-68), hex. */
    val senderKeyHex: String,
    val addedAccountKeyHex: String,
    /** The envelope plaintext, hex. */
    val envelopeHex: String,
)

/** ADR-0060 §4 — a contact's relayed introductions bundle, kept whole. */
@Serializable
internal data class StoredIntroBundle(
    /** The Introductions envelope as received, hex. */
    val bundleHex: String,
    /** Its stamp, the `bundleIsNewer` order key. */
    val updatedAtMs: Long,
)

/** ADR-0060 §4 — this account's own introduction record and the inputs it
 *  was built from ([introductionOfferKey]). */
@Serializable
internal data class StoredOwnOffer(val key: String, val recordHex: String)

/** The whole per-account document. */
@Serializable
internal data class ReachDoc(
    val versions: List<StoredVersion> = emptyList(),
    val registered: List<StoredRegistered> = emptyList(),
    val links: List<StoredLink> = emptyList(),
    val replies: List<StoredReply> = emptyList(),
    /** ADR-0066 section 3.5a rule 5 - the links this device minted before
     *  the account's keys moved: their keys name the old mailbox and are not
     *  registered on the new one; the rotating device revokes those with a
     *  ledger row and then forgets them. */
    val spentLinks: List<StoredLink> = emptyList(),
    val held: List<StoredHeld> = emptyList(),
    val delegations: List<StoredDelegation> = emptyList(),
    /** The visibility setting envelope (`composeVisibilityEnvelope`), hex. */
    val visibilityHex: String? = null,
    /** The `open` flag the server was last told, or `null` when never told. */
    val registeredOpen: Boolean? = null,
    /** ADR-0066 section 3.5 - the account key (hex) whose mailbox
     *  [registered] and [registeredOpen] describe, once a rotation moved the
     *  reach keys; `null` before any rotation (the key the session first
     *  bound). */
    val registeredForKeyHex: String? = null,
    /** ADR-0066 section 3.5a rule 3 - the account key (hex) the profile
     *  still has to be re-sent under to every conversation, set when the
     *  reach keys moved to it and cleared once every conversation took the
     *  envelope; `null` when nothing is owed. */
    val profileResendOwedForKeyHex: String? = null,
    /** Every conversation group this account is in, hex. */
    val joinedGroups: List<String> = emptyList(),
    /** Every channel this account is subscribed to or publishes, hex. */
    val channels: List<String> = emptyList(),
    /** Whether the name terms were last published as Everyone. */
    val publishedPublic: Boolean? = null,
    /** The username certificate discovery signed for this account, hex. */
    val usernameCertHex: String? = null,
    /** What the Everyone publication was last sent for
     *  ([everyonePublishKey]: name, username and certificate). */
    val publishedKey: String? = null,
    /** The ReachState envelope (`composeReachStateEnvelope`) in force, hex:
     *  composed here on a rotation, or accepted verbatim from another of the
     *  account's devices. Carries its stamp, so a re-send keeps it. */
    val reachStateHex: String? = null,
    /** ADR-0060 §4 — the introduction record each contact offered for
     *  their own 2.1 publication: account key hex -> record hex. */
    val introOffers: Map<String, String> = emptyMap(),
    /** ADR-0060 §4 — the bundle each contact relayed: account key hex. */
    val introBundles: Map<String, StoredIntroBundle> = emptyMap(),
    /** ADR-0060 §4/§5 — the OPRF OUTPUT of each term already evaluated
     *  (`"n:<term>"` / `"u:<username>"` -> hex). Never sent anywhere. */
    val termOutputs: Map<String, String> = emptyMap(),
    /** ADR-0060 §4 — this account's current introduction record. */
    val ownOffer: StoredOwnOffer? = null,
    /** One-to-one group hex -> the offer key last sent there. */
    val offerSentTo: Map<String, String> = emptyMap(),
    /** One-to-one group hex -> the relayed-bundle digest last sent there. */
    val bundleSentTo: Map<String, String> = emptyMap(),
    /** ADR-0060 §1 ("Versions after a restore", amended 2026-10-01) — the
     *  [VersionRecovery] name of this document, `null` before it was decided
     *  on the first reconcile. */
    val versionRecovery: String? = null,
    /** The reach public keys (hex) the mailbox held when the recovery read
     *  succeeded: what a slot's version is recovered from. Public keys only. */
    val recoveryPkHex: List<String> = emptyList(),
    /** The slots ([reachSlotId]) whose version was already recovered from
     *  [recoveryPkHex], so a slot is scanned once. */
    val recoveredSlots: List<String> = emptyList(),
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
