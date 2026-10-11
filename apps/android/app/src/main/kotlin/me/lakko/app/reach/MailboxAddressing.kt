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
