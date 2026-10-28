// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.util.Log
import me.lakko.app.ds.DsReachAuth
import uniffi.lakko_ffi_uniffi.FfiMailboxKind
import uniffi.lakko_ffi_uniffi.FfiReachGrant
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.MailboxKey

/**
 * ADR-0078 phase 1 - sealed mailbox rows.
 *
 * Every row stored in a person's mailbox is a sealed box to that person's
 * mailbox key, with the kind INSIDE the seal (the clear one-byte kind this
 * client used to prepend is gone). A mailbox key is derived from the account
 * seed, every device of the account derives the same one, and its address is
 * a hash of the key: nothing about the address is a function of the identity
 * key any more. A writer learns the key only from a 97-byte mailbox binding
 * (the key signed by the owner's account identity key) and verifies the
 * binding against the account key it holds pinned BEFORE it writes; a failed
 * check writes nothing.
 *
 * Everything that touches the native core sits behind [MailboxCrypto] and
 * [OwnMailbox], so the write and read rules run in a host JVM test.
 */

/** What a mailbox row carries (inside the seal). */
public enum class MailboxRowKind {
    /** A signed `ContactRequest`. */
    CONTACT_REQUEST,

    /** An MLS Welcome. */
    WELCOME,

    /** A signed `ContactAck`. */
    CONTACT_ACK,

    /** A Welcome rendezvous (the channel co-publisher join). */
    WELCOME_RENDEZVOUS,

    /** A signed link refusal (ADR-0079 amendment of 2026-10-19). */
    LINK_REFUSAL,
    ;

    /** The core's kind. */
    public fun toFfi(): FfiMailboxKind = when (this) {
        CONTACT_REQUEST -> FfiMailboxKind.CONTACT_REQUEST
        WELCOME -> FfiMailboxKind.WELCOME
        CONTACT_ACK -> FfiMailboxKind.CONTACT_ACK
        WELCOME_RENDEZVOUS -> FfiMailboxKind.WELCOME_RENDEZVOUS
        LINK_REFUSAL -> FfiMailboxKind.LINK_REFUSAL
    }

    public companion object {
        /** The core's kind as this one. */
        public fun of(kind: FfiMailboxKind): MailboxRowKind = when (kind) {
            FfiMailboxKind.CONTACT_REQUEST -> CONTACT_REQUEST
            FfiMailboxKind.WELCOME -> WELCOME
            FfiMailboxKind.CONTACT_ACK -> CONTACT_ACK
            FfiMailboxKind.WELCOME_RENDEZVOUS -> WELCOME_RENDEZVOUS
            FfiMailboxKind.LINK_REFUSAL -> LINK_REFUSAL
        }
    }
}

/** One opened row of the own mailbox: its kind and the inner wire. */
public data class OpenedMailboxRow(val kind: MailboxRowKind, val body: ByteArray) {
    override fun equals(other: Any?): Boolean =
        other is OpenedMailboxRow && kind == other.kind && body.contentEquals(other.body)

    override fun hashCode(): Int = kind.hashCode() * 31 + body.contentHashCode()
}

/** A mailbox binding did not verify against the account key held for its
 *  owner, or was malformed: nothing is written to that mailbox. The message
 *  carries no identifier. */
public class MailboxBindingRefused(cause: Throwable? = null) :
    RuntimeException("mailbox binding refused", cause)

/** No mailbox key is known for the person a write is meant for (no grant,
 *  no binding): nothing is written. The message carries no identifier. */
public class MailboxUnknown : RuntimeException("no mailbox known for the recipient")

/** The native mailbox operations a writer needs. */
public interface MailboxCrypto {
    /** Seal [body] as one row of [kind] to the 32-byte [mailboxPk]. */
    public fun seal(mailboxPk: ByteArray, kind: MailboxRowKind, body: ByteArray): ByteArray

    /** The 32-byte address of the mailbox named by [mailboxPk]. */
    public fun address(mailboxPk: ByteArray): ByteArray

    /** The mailbox key [binding] names, after verifying it against
     *  [accountPk]; throws [MailboxBindingRefused] when it does not verify. */
    public fun verifyBinding(binding: ByteArray, accountPk: ByteArray): ByteArray

    /** The mailbox key [grant] writes to, after verifying its binding against
     *  [accountPk]; throws [MailboxBindingRefused] when it does not verify. */
    public fun grantMailboxPk(grant: FfiReachGrant, accountPk: ByteArray): ByteArray
}

/** [MailboxCrypto] over the UniFFI core. */
public object NativeMailboxCrypto : MailboxCrypto {
    override fun seal(mailboxPk: ByteArray, kind: MailboxRowKind, body: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.sealMailboxRow(mailboxPk, kind.toFfi(), body)

    override fun address(mailboxPk: ByteArray): ByteArray =
        uniffi.lakko_ffi_uniffi.mailboxAddress(mailboxPk)

    override fun verifyBinding(binding: ByteArray, accountPk: ByteArray): ByteArray =
        try {
            uniffi.lakko_ffi_uniffi.verifyMailboxBinding(binding, accountPk)
        } catch (cause: Throwable) {
            throw MailboxBindingRefused(cause)
        }

    override fun grantMailboxPk(grant: FfiReachGrant, accountPk: ByteArray): ByteArray =
        try {
            uniffi.lakko_ffi_uniffi.reachGrantMailboxPkVerified(grant, accountPk)
        } catch (cause: Throwable) {
            throw MailboxBindingRefused(cause)
        }
}

/**
 * Where one write goes: the VERIFIED mailbox key it is sealed to, and what
 * authorises it ([auth] `null` = a proofless write, which spends one
 * `mailbox-write` anonymous token).
 */
public class MailboxDest(public val mailboxPk: ByteArray, public val auth: DsReachAuth?)

/** A sealed row, ready to post: the address it goes to and the row. */
public class SealedMailboxWrite(public val address: ByteArray, public val row: ByteArray)

/** Seal [body] as one row of [kind] for [dest]: the row and its address. */
public fun MailboxCrypto.sealFor(dest: MailboxDest, kind: MailboxRowKind, body: ByteArray): SealedMailboxWrite =
    SealedMailboxWrite(address = address(dest.mailboxPk), row = seal(dest.mailboxPk, kind, body))

/**
 * The mailbox key a grant held for a person writes to, or `null` when its
 * binding does not verify against [pinnedAccountPk] (the write it was for is
 * refused). Logged without identifiers.
 */
public fun MailboxCrypto.grantMailboxPkOrNull(grant: FfiReachGrant, pinnedAccountPk: ByteArray): ByteArray? =
    try {
        grantMailboxPk(grant, pinnedAccountPk)
    } catch (_: Throwable) {
        logRefusedBinding("grant")
        null
    }

/** The mailbox key [binding] names, or `null` when it does not verify
 *  against [pinnedAccountPk]. Logged without identifiers. */
public fun MailboxCrypto.bindingMailboxPkOrNull(binding: ByteArray, pinnedAccountPk: ByteArray): ByteArray? =
    try {
        verifyBinding(binding, pinnedAccountPk)
    } catch (_: Throwable) {
        logRefusedBinding("binding")
        null
    }

/**
 * A write under a held [grant], or `null` when its binding does not verify
 * against [pinnedAccountPk]: the grant's verified mailbox key and the
 * authorisation [authOf] builds from its seed. [authOf] is not called for a
 * refused grant.
 */
public fun verifiedGrantDest(
    grant: FfiReachGrant,
    pinnedAccountPk: ByteArray,
    crypto: MailboxCrypto,
    authOf: (seed: ByteArray) -> DsReachAuth,
): MailboxDest? {
    val pk = crypto.grantMailboxPkOrNull(grant, pinnedAccountPk) ?: return null
    return MailboxDest(pk, authOf(grant.seed))
}

/**
 * Whether a grant received for the person known by [accountPk] (a profile
 * grant from a resolved sender, a join attribution's reply grant from the
 * joiner's resolved account) names that person's mailbox: its binding
 * verifies against [accountPk]. A grant that does not is not kept. Logged
 * without identifiers. A `/c/` link grant is verified in the core instead
 * (`CallInviteCapability.verifiedLinkGrantMailboxPk`).
 */
public fun grantBindsTo(grant: FfiReachGrant, accountPk: ByteArray, crypto: MailboxCrypto): Boolean =
    crypto.grantMailboxPkOrNull(grant, accountPk) != null

/** The mailbox key of the first of [grants] whose binding verifies against
 *  [pinnedAccountPk], or `null`. */
public fun firstVerifiedMailboxPk(
    grants: List<FfiReachGrant>,
    pinnedAccountPk: ByteArray,
    crypto: MailboxCrypto,
): ByteArray? {
    for (grant in grants) {
        crypto.grantMailboxPkOrNull(grant, pinnedAccountPk)?.let { return it }
    }
    return null
}

private fun logRefusedBinding(what: String) {
    try {
        Log.w(MAILBOX_LOG_TAG, "ADR-0078: a mailbox $what did not verify; nothing written")
    } catch (_: Throwable) {
        // Host JVM tests have no android.util.Log.
    }
}

private const val MAILBOX_LOG_TAG = "Mailbox"

/** The account's own mailbox: its address and the opener of its rows. */
public interface OwnMailbox {
    /** The 32-byte address the mailbox is stored under. */
    public val address: ByteArray

    /** Open one row; `null` when it does not open (it is dropped). */
    public fun open(row: ByteArray): OpenedMailboxRow?
}

/** [OwnMailbox] over the account's [MailboxKey]. */
public class NativeOwnMailbox(private val key: MailboxKey) : OwnMailbox {
    override val address: ByteArray by lazy { key.address() }

    override fun open(row: ByteArray): OpenedMailboxRow? =
        try {
            val opened = uniffi.lakko_ffi_uniffi.openMailboxRow(key, row)
            OpenedMailboxRow(MailboxRowKind.of(opened.kind), opened.body)
        } catch (_: Throwable) {
            null
        }

    public companion object {
        /** The own mailbox of the account whose identity is [accountIdentity]. */
        public fun of(accountIdentity: Identity): NativeOwnMailbox =
            NativeOwnMailbox(MailboxKey.fromAccountIdentity(accountIdentity))
    }
}

/**
 * ADR-0078 - the identities whose mailboxes this session claims and reads:
 * the conversation account identity (while its mailbox is read) and the
 * published one, never a device identity (device mailboxes are removed).
 * Duplicates by reference are dropped.
 */
public fun <I : Any> ownMailboxIdentities(conversation: I?, published: I?): List<I> =
    listOfNotNull(conversation, published).distinct()

/** The Welcome an opened own-mailbox row carries (a WELCOME or a
 *  WELCOME_RENDEZVOUS row), or `null` for any other kind or a row that did
 *  not open. */
public fun welcomeBodyOf(row: OpenedMailboxRow?): ByteArray? = when (row?.kind) {
    MailboxRowKind.WELCOME, MailboxRowKind.WELCOME_RENDEZVOUS -> row.body
    else -> null
}

/**
 * Where a request to a person a SEARCH found goes (ADR-0078): under the grant
 * the search found them by ([grantWire]), else proofless to the key the
 * binding of their public record names ([mailboxBinding]), each verified
 * against [recipientKey] (their current account key). `null` when the search
 * handed neither (the caller falls back to what this device holds). A grant
 * or binding that is handed but does not verify throws
 * [MailboxBindingRefused]: the write is refused, never redirected.
 */
public fun foundRequestDest(
    grantWire: ByteArray?,
    mailboxBinding: ByteArray?,
    recipientKey: ByteArray,
    crypto: MailboxCrypto,
    authOf: (seed: ByteArray) -> DsReachAuth,
    decodeGrant: (ByteArray) -> FfiReachGrant,
): MailboxDest? {
    if (grantWire != null) {
        val grant = try {
            decodeGrant(grantWire)
        } catch (cause: Throwable) {
            throw MailboxBindingRefused(cause)
        }
        return verifiedGrantDest(grant, recipientKey, crypto, authOf) ?: throw MailboxBindingRefused()
    }
    if (mailboxBinding != null) {
        val pk = crypto.bindingMailboxPkOrNull(mailboxBinding, recipientKey) ?: throw MailboxBindingRefused()
        return MailboxDest(pk, null)
    }
    return null
}
