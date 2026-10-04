// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.UiText
import me.lakko.app.push.GroupPushRouting

/**
 * ADR-0065 phase 2 / `docs/ux/identity-recovery.md` 16.5 — the group-closed
 * envelope on the receive side: the verdict and the wire peek.
 *
 * The envelope (`lakko-contacts::group_closed`, `lakko/group-closed/v1`) is
 * the group-name template: a bounded, domain-separated application message
 * carrying the closing stamp and the new group's id, or no id when the group
 * was closed without a successor. It is accepted at receipt ONLY from an owner
 * or admin resolved through the live role table under the per-group lock
 * (C-68, C-62 sub-invariant 5), and ordered by a strictly-greater stamp: the
 * second closed notice to land wins the `Open the new group` target (16.10).
 *
 * Deliberately the same shape as [groupNameVerdict], for the same reason: the
 * decision is readable and testable on the JVM without the native library, and
 * the repository runs it against CURRENT state in the poll's own critical
 * section.
 */

/** The decoded envelope as the store keeps it. */
public data class StoredGroupClosed(
    val closedAtMs: ULong,
    /** The successor group id, hex, or `null` when closed without one. */
    val successorGroupIdHex: String?,
)

/** The receive-side decision for an incoming group-closed envelope. */
public enum class GroupClosedVerdict(public val accepts: Boolean) {
    ACCEPT_IDENTICAL(true),
    ACCEPT_OWNER(true),
    ACCEPT_ADMIN(true),

    /** A notice from a plain member, or a key not in the role table. */
    REJECT_NOT_AUTHORIZED(false),

    /** No role table (a table-less group has no admin to close it), an
     *  unreadable table, or an unresolvable sender. Fail closed. */
    REJECT_UNVERIFIABLE(false),

    /** Authorised, but not newer than what is stored. */
    REJECT_STALE(false),
}

/**
 * Decide whether [incoming] replaces [stored] for one group.
 *
 * @param senderSignatureKey the sender's account key resolved from the MLS
 *   leaf at receipt, raw bytes — `null` when unresolvable.
 * @param owner the live role table (owner + admins), no table, or unreadable.
 * @param isNewer the canonical order — the FFI `groupClosedIsNewer`.
 */
public fun groupClosedVerdict(
    senderSignatureKey: ByteArray?,
    owner: GroupPushRouting.OwnerLookup,
    incoming: StoredGroupClosed,
    stored: StoredGroupClosed?,
    isNewer: (incomingMs: ULong, storedMs: ULong) -> Boolean,
): GroupClosedVerdict {
    if (stored != null && stored == incoming) return GroupClosedVerdict.ACCEPT_IDENTICAL
    val authorised = when (owner) {
        is GroupPushRouting.OwnerLookup.Unavailable -> GroupClosedVerdict.REJECT_UNVERIFIABLE
        // 16.5 is reached only in a role-table group (the owned-group branch);
        // a notice in a table-less group names an authority that does not exist.
        is GroupPushRouting.OwnerLookup.NoRoleTable -> GroupClosedVerdict.REJECT_UNVERIFIABLE
        is GroupPushRouting.OwnerLookup.Known -> when {
            senderSignatureKey == null -> GroupClosedVerdict.REJECT_UNVERIFIABLE
            senderSignatureKey.contentEquals(owner.owner) -> GroupClosedVerdict.ACCEPT_OWNER
            owner.admins.any { it.contentEquals(senderSignatureKey) } -> GroupClosedVerdict.ACCEPT_ADMIN
            else -> GroupClosedVerdict.REJECT_NOT_AUTHORIZED
        }
    }
    if (!authorised.accepts) return authorised
    if (stored == null) return authorised
    return if (isNewer(incoming.closedAtMs, stored.closedAtMs)) authorised else GroupClosedVerdict.REJECT_STALE
}

/**
 * The frozen group-closed envelope domain separator, `lakko/group-closed/v1`
 * followed by one NUL byte (22 bytes). Built programmatically: a raw NUL in a
 * source file makes git treat it as binary.
 */
internal val GROUP_CLOSED_ENVELOPE_DOMAIN: ByteArray =
    "lakko/group-closed/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the group-closed domain
 * separator? A complete classification, like the group-name peek: the
 * separator ends in a NUL byte, so no chat envelope and no typed legacy
 * message can open with it. `internal` so the background push filter reuses
 * this exact test. Pure — host-testable.
 */
internal fun looksLikeGroupClosedEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < GROUP_CLOSED_ENVELOPE_DOMAIN.size) return false
    for (i in GROUP_CLOSED_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != GROUP_CLOSED_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

/** 16.5 — the closed group's shapes. */
public object GroupClosedCopy {
    public fun terminalHeadline(closerName: String): UiText =
        UiText("group.closed.system", texts = mapOf("name" to closerName))

    public val COMPOSER: UiText = UiText("group.closed.composer")
    public val OPEN_NEW: UiText = UiText("group.closed.open_new")
    public val LIST_ROW: UiText = UiText("group.closed.row")

    public fun panel(newGroupName: String): UiText =
        UiText("group.closed.panel", texts = mapOf("name" to newGroupName))
}

/**
 * 16.5 — `Open the new group` is shown only once this device has joined the
 * successor; a device not invited sees the bar alone. The id is the admin's
 * claim; joining is what makes it an opening.
 */
public fun openNewGroupTarget(successorGroupIdHex: String?, joinedGroupIdsHex: Set<String>): String? {
    val hex = successorGroupIdHex?.lowercase() ?: return null
    return if (hex in joinedGroupIdsHex.map { it.lowercase() }) hex else null
}
