// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import uniffi.lakko_ffi_uniffi.FfiIntroductionPayload
import uniffi.lakko_ffi_uniffi.OprfResult

/*
 * ADR-0060 section 5 — the Everyone publication: on entering Everyone and on
 * every change of the name, the username or its certificate, the client sends
 * discovery the LOOKUP KEY of every name search term and one public record
 * sealed under the OPRF OUTPUTS of those terms plus the username. The outputs
 * never leave the device: a copy of discovery's tables must open no record.
 *
 * Kept free of Android and of native calls (every FFI function is passed in),
 * so the decision and the request shape run on the host JVM.
 */

/** What the publication is keyed on: a change of any part republishes. The
 *  certificate is in the key so a certificate arriving later republishes the
 *  record with the username in it. */
internal fun everyonePublishKey(displayName: String?, username: String?, certificate: ByteArray?): String {
    val cert = certificate?.joinToString("") { "%02x".format(it) } ?: ""
    return "${displayName ?: ""}|${username ?: ""}|$cert"
}

/** What a refresh has to do. */
internal enum class EveryoneAction {
    /** The server already holds what the setting asks for. */
    NONE,

    /** Outside Everyone and never published: only record that. */
    NOTE_WITHDRAWN,

    /** Send `PUT /discovery/visibility`. */
    PUT,
}

/** Decide from what the server was last told ([published], [publishedKey])
 *  and what is wanted now. */
internal fun everyoneAction(
    wantPublic: Boolean,
    published: Boolean?,
    publishedKey: String?,
    currentKey: String,
): EveryoneAction = when {
    wantPublic -> if (published == true && publishedKey == currentKey) EveryoneAction.NONE else EveryoneAction.PUT
    published == false -> EveryoneAction.NONE
    published == null -> EveryoneAction.NOTE_WITHDRAWN
    else -> EveryoneAction.PUT
}

/** The person as the publication states them. */
internal class EveryoneIdentity(
    val displayName: String?,
    /** The canonical username; published only with a [certificate]. */
    val username: String?,
    val certificate: ByteArray?,
    /** The account identity public key (32 bytes). */
    val identityPk: ByteArray?,
    /** The auth account id (16 bytes). */
    val accountId: ByteArray?,
)

/** The native and network steps, passed in. */
internal class EveryoneOps(
    val nameSearchTerms: (String) -> List<String>,
    val oprfNameTerm: suspend (String) -> OprfResult,
    val oprfUsername: suspend (String) -> OprfResult,
    val composePublicRecord: (FfiIntroductionPayload, List<ByteArray>, ULong) -> ByteArray,
    val putVisibility: suspend (public: Boolean, lookupKeys: List<ByteArray>, record: ByteArray?) -> Unit,
)

/** How a publication ended. */
internal enum class EveryoneOutcome {
    /** Entered Everyone with the terms and the record. */
    PUBLISHED,

    /** Left Everyone. */
    WITHDRAWN,

    /** In Everyone, but there is nothing to publish (no name and no
     *  certified username) or no identity: nothing was sent, since the
     *  server refuses Everyone without a record. */
    NOTHING_TO_PUBLISH,
}

/**
 * Run the publication. Throws what [EveryoneOps] throws (the caller logs it).
 *
 * The record carries no `fof` grant: the mailbox is open in Everyone, and a
 * grant given to the world would outlive leaving it.
 */
internal suspend fun publishEveryone(
    wantPublic: Boolean,
    who: EveryoneIdentity,
    ops: EveryoneOps,
    nowMs: Long,
): EveryoneOutcome {
    if (!wantPublic) {
        ops.putVisibility(false, emptyList(), null)
        return EveryoneOutcome.WITHDRAWN
    }
    val name = who.displayName?.takeIf { it.isNotBlank() }
    val username = who.username?.takeIf { it.isNotEmpty() && who.certificate != null }
    val identityPk = who.identityPk
    val accountId = who.accountId?.takeIf { it.size == ACCOUNT_ID_LEN }
    val terms = name?.let(ops.nameSearchTerms).orEmpty()
    if (identityPk == null || accountId == null || (terms.isEmpty() && username == null)) {
        return EveryoneOutcome.NOTHING_TO_PUBLISH
    }
    val recordTermOutputs = mutableListOf<ByteArray>()
    if (username != null) recordTermOutputs += ops.oprfUsername(username).output
    val lookupKeys = mutableListOf<ByteArray>()
    for (term in terms) {
        val oprf = ops.oprfNameTerm(term)
        lookupKeys += oprf.lookupKey
        recordTermOutputs += oprf.output
    }
    val payload = FfiIntroductionPayload(
        identityPk = identityPk,
        accountId = accountId,
        displayName = name,
        username = username,
        usernameCertificate = if (username != null) who.certificate else null,
        fof = null,
    )
    val record = ops.composePublicRecord(payload, recordTermOutputs, nowMs.coerceAtLeast(0L).toULong())
    ops.putVisibility(true, lookupKeys, record)
    return EveryoneOutcome.PUBLISHED
}

private const val ACCOUNT_ID_LEN = 16
