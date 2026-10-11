// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import me.lakko.app.contacts.ProvenLink
import me.lakko.app.contacts.currentKeyOf
import me.lakko.app.contacts.keysOfPerson
import java.util.Base64

/*
 * ADR-0066 section 3.5a - the mailbox after a rotation of the account keys.
 *
 * A mailbox's address and the right to read it both derive from the account
 * key, so a rotated account moves to the mailbox of its NEW key at once. The
 * conversation leaves that exist still name the old key (Stage A moves none
 * of them), so every place that turns an account key into a mailbox - and
 * picks the grant or delegation that opens it - first maps the key through
 * the proven-predecessor resolver (`contacts/ProvenPredecessors.kt`):
 * predecessor -> current, this account's own key included.
 *
 * Only a PROVEN step is followed. A key change nothing proved (a reset, a
 * stripped or refused proof) creates no link, so such a key resolves to
 * itself and is addressed as the stranger it is.
 *
 * Everything here is pure. Keys are lowercase hex here (the reach store's
 * form) and standard base64 in a [ProvenLink].
 */

/** How long a predecessor's mailbox is still read after this device adopted
 *  the successor key: the DS journal retention (chosen, ADR-0066 3.5a
 *  rule 4). */
public const val PREDECESSOR_MAILBOX_READ_MS: Long = 30L * 24 * 60 * 60 * 1000

private fun hexToB64(hex: String): String? = reachUnhex(hex)?.let { Base64.getEncoder().encodeToString(it) }

private fun b64ToHex(b64: String): String? =
    try {
        reachHex(Base64.getDecoder().decode(b64))
    } catch (_: IllegalArgumentException) {
        null
    }

/**
 * Rule 1 - the account key whose mailbox is ADDRESSED for a person known by
 * [accountKeyHex]: the key the proven steps lead to, or [accountKeyHex]
 * itself when nothing proved a change (or the input is not hex).
 */
public fun addressedKeyHex(accountKeyHex: String, links: List<ProvenLink>): String {
    val key = accountKeyHex.lowercase()
    if (links.isEmpty()) return key
    val b64 = hexToB64(key) ?: return key
    return b64ToHex(currentKeyOf(b64, links)) ?: key
}

/** [addressedKeyHex] over raw keys. Always a fresh array. */
public fun addressedKey(accountKey: ByteArray, links: List<ProvenLink>): ByteArray =
    reachUnhex(addressedKeyHex(reachHex(accountKey), links)) ?: accountKey.copyOf()

/**
 * Rule 2 - every key one person is filed under, the CURRENT key first and
 * then each proven predecessor, newest to earliest. A grant is stored under
 * the first and looked up under all of them, so one that was filed under a
 * key before that key became a proven predecessor is still found.
 */
public fun personKeysHex(accountKeyHex: String, links: List<ProvenLink>): List<String> {
    val key = accountKeyHex.lowercase()
    if (links.isEmpty()) return listOf(key)
    val b64 = hexToB64(key) ?: return listOf(key)
    val chain = keysOfPerson(b64, links).mapNotNull(::b64ToHex).asReversed()
    return if (chain.isEmpty()) listOf(key) else chain
}

/**
 * The grants held for one person out of [held], in lookup order: those filed
 * under the current key first, then under each predecessor ([personKeys] is
 * [personKeysHex]'s answer). Within one key the stored order is kept.
 */
internal fun heldForPerson(held: List<StoredHeld>, personKeys: List<String>): List<StoredHeld> =
    personKeys.flatMap { key -> held.filter { it.ownerKeyHex == key } }

/**
 * Rule 4 - whether a predecessor's mailbox is still READ: for
 * [PREDECESSOR_MAILBOX_READ_MS] after this device adopted the successor
 * ([adoptedAtMs]), then never again. A clock that went backwards reads (mail
 * already sent must not be lost to it); nothing is ever written or registered
 * there either way.
 */
public fun predecessorMailboxIsRead(adoptedAtMs: Long, nowMs: Long): Boolean =
    nowMs - adoptedAtMs < PREDECESSOR_MAILBOX_READ_MS

/** What re-sending the profile into one conversation ended with. */
public enum class ProfileResend {
    /** The envelope went out. */
    SENT,

    /** Nothing to send there (no profile stored, membership ended, the
     *  conversation is gone): not a failure. */
    NOTHING_TO_SEND,

    /** It could not be sent; the re-send stays owed. */
    FAILED,
}

/**
 * Rule 3 - re-send the profile into every conversation of [targets], each
 * attempted even when an earlier one failed. `true` only when none FAILED:
 * anything else keeps the re-send owed. [onSent] is told each conversation
 * that took it, so a retry does not send there again.
 */
public suspend fun resendProfileToAll(
    targets: Collection<String>,
    onSent: (String) -> Unit = {},
    send: suspend (String) -> ProfileResend,
): Boolean {
    var allDone = true
    for (target in targets) {
        val outcome = try {
            send(target)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            ProfileResend.FAILED
        }
        when (outcome) {
            ProfileResend.SENT -> onSent(target)
            ProfileResend.NOTHING_TO_SEND -> Unit
            ProfileResend.FAILED -> allDone = false
        }
    }
    return allDone
}

// ---------------------------------------------------------------------
// Rule 6 - a refused write re-checks the address once
// ---------------------------------------------------------------------

/**
 * Rule 6 - one write into a person's mailbox, re-checked once when refused.
 *
 * [write] derives the mailbox and picks the grant itself, each time it is
 * called, so a second call sees whatever [recheck] learned. A refusal
 * ([isRefusal], the server's `401`) runs [recheck] (the person's identity
 * bundle through the fail-closed gate, which records a proven predecessor);
 * when that answers `true` the write is tried ONCE more. A recheck that
 * could not run (`false`, or a throw) re-throws the first refusal, and a
 * second refusal propagates: never a loop.
 */
public suspend fun <T> writeRecheckingAddress(
    write: suspend () -> T,
    isRefusal: (Throwable) -> Boolean,
    recheck: suspend () -> Boolean,
): T {
    val refusal = try {
        return write()
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (cause: Throwable) {
        if (!isRefusal(cause)) throw cause
        cause
    }
    val checked = try {
        recheck()
    } catch (cancel: kotlinx.coroutines.CancellationException) {
        throw cancel
    } catch (_: Throwable) {
        false
    }
    if (!checked) throw refusal
    return write()
}

/**
 * Rule 6 - [held] after the person filed under [ownerHex] handed over
 * [grantHex] for ([kind], [scopeKey]). A grant that REPLACES another of the
 * same class and scope keeps the replaced one beside it
 * ([StoredHeld.previousGrantHex]): the new one may open a mailbox this
 * device does not address yet (the person rotated and the re-sent profile
 * arrived before the proof was observed), and until the address is known
 * current ([withoutPreviousGrants]) the old one is what the old mailbox
 * takes. The same grant again changes nothing (the list is returned as is).
 */
internal fun heldWithGrant(
    held: List<StoredHeld>,
    ownerHex: String,
    kind: String,
    scopeKey: String,
    grantHex: String,
): List<StoredHeld> {
    fun same(h: StoredHeld) = h.ownerKeyHex == ownerHex && h.kind == kind && h.scopeHex == scopeKey
    val existing = held.firstOrNull(::same)
    if (existing != null && existing.grantHex == grantHex) return held
    return held.filterNot(::same) + StoredHeld(ownerHex, kind, scopeKey, grantHex, existing?.grantHex)
}

/** Rule 6 - the address of the person filed under [personKeys] is known
 *  current: the grants kept beside newer ones are dropped. The list itself
 *  when nothing changes. */
internal fun withoutPreviousGrants(held: List<StoredHeld>, personKeys: List<String>): List<StoredHeld> {
    if (held.none { it.previousGrantHex != null && it.ownerKeyHex in personKeys }) return held
    return held.map { if (it.ownerKeyHex in personKeys) it.copy(previousGrantHex = null) else it }
}

/** The grant wires (hex) one stored entry offers, the current one first. */
internal fun grantWiresOf(entry: StoredHeld): List<String> =
    listOfNotNull(entry.grantHex, entry.previousGrantHex?.takeIf { it != entry.grantHex })

// ---------------------------------------------------------------------
// Committer delegations after a rotation of this account's own keys
// ---------------------------------------------------------------------

/**
 * The first delegation [envelopes] hold for one of this account's own keys
 * ([delegateKeys], the current key first, then each own predecessor this
 * device still holds the seed of): a proposer names the committer by the
 * key IT knows, which is the predecessor until it observed the rotation.
 * [headerFor] is the core's `committerDelegationFor` (`null`: this envelope
 * names another key; a throw: a malformed envelope, which authorises
 * nothing). Returns the header and the INDEX of the key it names, whose
 * seed signs the write.
 */
public fun <E> pickOwnDelegation(
    envelopes: List<E>,
    delegateKeys: List<ByteArray>,
    headerFor: (E, ByteArray) -> String?,
): Pair<String, Int>? {
    for (envelope in envelopes) {
        for ((index, key) in delegateKeys.withIndex()) {
            val header = try {
                headerFor(envelope, key)
            } catch (_: Throwable) {
                break
            }
            if (header != null) return header to index
        }
    }
    return null
}

// ---------------------------------------------------------------------
// Rule 5 - what was granted before the rotation is spent
// ---------------------------------------------------------------------

/** What re-sending one unanswered contact request from the new key ended
 *  with. */
public enum class RequestResend {
    /** A fresh request went out; the stored entry is replaced. */
    SENT,

    /** The person can no longer be asked this way (their mailbox refused
     *  the write, their keys were reset): the spent entry is dropped. */
    GONE,

    /** It could not be tried (no network): the entry stays, the step owed. */
    FAILED,
}

/** The pending list after a re-send pass, and whether the step is done. */
public class ResentRequests<T>(public val pending: List<T>, public val done: Boolean)

/**
 * Rule 5 - re-send every unanswered outgoing request of [pending] from the
 * new key, each attempted even when an earlier one failed. A sent one is
 * replaced by what [send] returned, a gone one is dropped, a failed one
 * stays as it was and keeps the step owed.
 */
public suspend fun <T> resendPendingRequests(
    pending: List<T>,
    send: suspend (T) -> Pair<RequestResend, T?>,
): ResentRequests<T> {
    val out = ArrayList<T>(pending.size)
    var done = true
    for (entry in pending) {
        val (outcome, replacement) = try {
            send(entry)
        } catch (cancel: kotlinx.coroutines.CancellationException) {
            throw cancel
        } catch (_: Throwable) {
            RequestResend.FAILED to null
        }
        when (outcome) {
            RequestResend.SENT -> out += replacement ?: entry
            RequestResend.GONE -> Unit
            RequestResend.FAILED -> {
                out += entry
                done = false
            }
        }
    }
    return ResentRequests(out, done)
}

/** Rule 5 - the spent-link list after the reach keys moved: what was already
 *  spent, then the links live until now, each ledger id once. Bounded: the
 *  oldest entries go first. */
internal fun spentLinksAfterMove(spent: List<StoredLink>, live: List<StoredLink>): List<StoredLink> {
    val out = LinkedHashMap<String, StoredLink>()
    for (link in spent + live) out[link.linkIdHex] = link
    return out.values.toList().takeLast(MAX_SPENT_LINKS)
}

/** Chosen, not measured: far past the links one device has live at once. */
internal const val MAX_SPENT_LINKS: Int = 256

/** How often a refused write to one person re-checks their address through
 *  the gate (chosen, not measured): an owed Welcome is retried at every
 *  poll, and the bundle is not fetched at every poll. */
public const val ADDRESS_RECHECK_INTERVAL_MS: Long = 10L * 60 * 1000

/** Rule 6 - whether the address last re-checked at [lastMs] (`null`: never)
 *  is re-checked again at [nowMs]. A clock that went backwards re-checks. */
public fun addressRecheckDue(lastMs: Long?, nowMs: Long): Boolean =
    lastMs == null || nowMs < lastMs || nowMs - lastMs >= ADDRESS_RECHECK_INTERVAL_MS
