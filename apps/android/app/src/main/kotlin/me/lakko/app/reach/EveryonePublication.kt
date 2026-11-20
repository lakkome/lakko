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
 *  record with the username in it; the mailbox binding is, so a rotation of
 *  the account keys replaces a record naming the old mailbox. */
internal fun everyonePublishKey(
    displayName: String?,
    username: String?,
    certificate: ByteArray?,
    mailboxBinding: ByteArray? = null,
): String {
    val cert = certificate?.joinToString("") { "%02x".format(it) } ?: ""
    val binding = mailboxBinding?.joinToString("") { "%02x".format(it) } ?: ""
    return "${displayName ?: ""}|${username ?: ""}|$cert|$binding"
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

/**
 * [everyoneAction] for one refresh. The first refresh of a session in Everyone
 * sends even when the note says the server already holds it: the note is this
 * device's memory, not the server's state, and a person who chose Everyone and
 * cannot be found has no way to tell.
 */
internal fun everyoneActionForRefresh(
    wantPublic: Boolean,
    published: Boolean?,
    publishedKey: String?,
    currentKey: String,
    firstOfSession: Boolean,
): EveryoneAction {
    val action = everyoneAction(wantPublic, published, publishedKey, currentKey)
    return if (action == EveryoneAction.NONE && wantPublic && firstOfSession) EveryoneAction.PUT else action
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
    /** ADR-0078 - the account's 97-byte mailbox binding: the record carries
     *  it so a searcher can address the open mailbox; `null` publishes
     *  nothing. */
    val mailboxBinding: ByteArray? = null,
)

/** The native and network steps, passed in. */
internal class EveryoneOps(
    val nameSearchTerms: (String) -> List<String>,
    val oprfNameTerm: suspend (String) -> OprfResult,
    val oprfUsername: suspend (String) -> OprfResult,
    val composePublicRecord: (FfiIntroductionPayload, List<ByteArray>, ULong) -> ByteArray,
    val putVisibility: suspend (public: Boolean, lookupKeys: List<ByteArray>, record: ByteArray?) -> Unit,
    /** The own name's terms at once (`POST /discovery/oprf/batch`, one
     *  register token), in order; `null` evaluates them one by one. */
    val oprfOwnTerms: (suspend (List<me.lakko.app.discovery.OwnTerm>) -> List<OprfResult>)? = null,
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
    val binding = who.mailboxBinding
    if (identityPk == null || accountId == null || binding == null || (terms.isEmpty() && username == null)) {
        return EveryoneOutcome.NOTHING_TO_PUBLISH
    }
    val recordTermOutputs = mutableListOf<ByteArray>()
    val lookupKeys = mutableListOf<ByteArray>()
    val batch = ops.oprfOwnTerms
    if (batch != null) {
        // Publishing the own name: every term in one batch, one token.
        val own = listOfNotNull(username?.let { me.lakko.app.discovery.OwnTerm.Username(it) }) +
            terms.map { me.lakko.app.discovery.OwnTerm.NameTerm(it) }
        val results = batch(own)
        val offset = if (username != null) 1 else 0
        if (username != null) recordTermOutputs += results[0].output
        for (i in terms.indices) {
            lookupKeys += results[offset + i].lookupKey
            recordTermOutputs += results[offset + i].output
        }
    } else {
        if (username != null) recordTermOutputs += ops.oprfUsername(username).output
        for (term in terms) {
            val oprf = ops.oprfNameTerm(term)
            lookupKeys += oprf.lookupKey
            recordTermOutputs += oprf.output
        }
    }
    val payload = FfiIntroductionPayload(
        identityPk = identityPk,
        accountId = accountId,
        displayName = name,
        username = username,
        usernameCertificate = if (username != null) who.certificate else null,
        fof = null,
        mailboxBinding = binding,
    )
    val record = ops.composePublicRecord(payload, recordTermOutputs, nowMs.coerceAtLeast(0L).toULong())
    ops.putVisibility(true, lookupKeys, record)
    return EveryoneOutcome.PUBLISHED
}

private const val ACCOUNT_ID_LEN = 16

/** ADR-0098 step 5 — the discovery code for an account moderators removed
 *  from search. */
public const val HIDDEN_FROM_SEARCH_CODE: String = "hidden-from-search"

/** Whether [cause] (or a cause under it) is discovery refusing the Everyone
 *  record with `403 hidden-from-search`. */
public fun isHiddenFromSearch(cause: Throwable?): Boolean {
    var c: Throwable? = cause
    val seen = HashSet<Throwable>()
    while (c != null && seen.add(c)) {
        if (c is me.lakko.app.discovery.DiscoveryHttpError && c.status == 403 && c.code == HIDDEN_FROM_SEARCH_CODE) {
            return true
        }
        c = c.cause
    }
    return false
}

/**
 * ADR-0098 step 5 — whether Everyone can be chosen on Who can find me: not
 * while moderators keep the account out of search. A setting already on
 * Everyone stays as the person chose it.
 */
public fun everyoneSelectable(hiddenFromSearch: Boolean): Boolean = !hiddenFromSearch
