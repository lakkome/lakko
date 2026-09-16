// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

/**
 * Pure presentation rules for the Contacts tab and the contact detail —
 * docs/ux/client-redesign.md section 5.3. No Compose, no repository: every
 * function here takes plain values so the rules are unit-tested on the host.
 */

/** One contact row after name resolution. [key] is the stable row identity
 *  (the signature key, base64url); [name] is the resolved primary label;
 *  [username] the verified username without `@`, or `null`. */
public data class ContactRowModel(
    val key: String,
    val name: String,
    val username: String?,
    /** Whether the resolved name is the neutral placeholder — such a row sorts
     *  under `#` rather than under `M`, and matches no search term. */
    val placeholder: Boolean = false,
)

/** A letter section of the alphabetical list: the header and its rows. */
public data class ContactSection(val letter: String, val rows: List<ContactRowModel>)

/** The header for rows whose name does not start with a letter. */
public const val OTHER_SECTION: String = "#"

/** The sort key of a row: the name, case-folded, or the username when the name
 *  is the placeholder so an unnamed row still sorts by something the user
 *  recognises. */
internal fun contactSortKey(row: ContactRowModel): String {
    val base = if (row.placeholder) (row.username ?: "") else row.name
    return base.trim().trimStart('@').lowercase()
}

/** The section letter of a row: the first letter of its sort key, uppercased,
 *  or [OTHER_SECTION] when that is a digit, a symbol or nothing at all. */
public fun sectionLetterOf(row: ContactRowModel): String {
    val first = contactSortKey(row).firstOrNull() ?: return OTHER_SECTION
    return if (first.isLetter()) first.uppercaseChar().toString() else OTHER_SECTION
}

/**
 * Group [rows] into alphabetical sections. Letters sort A..Z (locale-neutral),
 * [OTHER_SECTION] last; rows inside a section sort by [contactSortKey], then by
 * [ContactRowModel.key] so two identical names keep a stable order.
 */
public fun contactSections(rows: List<ContactRowModel>): List<ContactSection> {
    val sorted = rows.sortedWith(compareBy({ contactSortKey(it) }, { it.key }))
    val grouped = sorted.groupBy { sectionLetterOf(it) }
    val letters = grouped.keys.sortedWith(
        compareBy<String>({ it == OTHER_SECTION }, { it }),
    )
    return letters.map { ContactSection(it, grouped.getValue(it)) }
}

/**
 * Filter [rows] by a search [query]: a case-insensitive substring match on the
 * name or the username (a leading `@` in the query is ignored). A blank query
 * keeps every row.
 */
public fun filterContacts(rows: List<ContactRowModel>, query: String): List<ContactRowModel> {
    val q = query.trim().trimStart('@').lowercase()
    if (q.isEmpty()) return rows
    return rows.filter { row ->
        (!row.placeholder && row.name.lowercase().contains(q)) ||
            (row.username?.lowercase()?.contains(q) == true)
    }
}

/**
 * Order incoming requests newest first. [messageId] is the delivery-service
 * message id, monotone per mailbox, so it is the arrival order without parsing
 * a timestamp.
 */
public fun <T> sortRequestsNewestFirst(requests: List<T>, messageId: (T) -> Long): List<T> =
    requests.sortedByDescending(messageId)

/**
 * Split a safety number into its five-digit groups. The core renders the
 * number as decimal digits already grouped by five and separated by spaces;
 * this strips anything that is not a digit and re-chunks, so the display does
 * not depend on the separator the core chose.
 */
public fun safetyNumberGroups(raw: String): List<String> {
    val digits = raw.filter { it.isDigit() }
    if (digits.isEmpty()) return emptyList()
    return digits.chunked(5)
}

/** Lay the groups out in rows of [perRow] for the large display. */
public fun safetyNumberRows(groups: List<String>, perRow: Int = 3): List<List<String>> {
    require(perRow > 0) { "perRow must be positive" }
    if (groups.isEmpty()) return emptyList()
    return groups.chunked(perRow)
}

/** The base64url (no padding) form of a signature key, used as the contact
 *  detail route argument. Only URL-safe characters, so it needs no encoding. */
public fun contactRouteKey(signatureKey: ByteArray): String =
    me.lakko.app.auth.base64UrlEncodeNoPad(signatureKey)

// =============================================================================
// Invite links sheet
// =============================================================================

/** Wire-format `max_uses` sentinel for an unlimited invite. */
internal const val INVITE_UNLIMITED_USES: Long = 0xFFFF_FFFFL

/** One row of the "Invite links" sheet. */
public data class InviteLinkRow(
    /** The `/i/<slug>` segment; the row identity. */
    val slug: String,
    /** The full shareable URL. */
    val url: String,
    /** Owner label, or empty. */
    val label: String,
    /** When this device created the link, or `null` when only the server
     *  knows the link (created on another device or in an earlier session). */
    val createdAtMs: Long?,
    /** UNIX millis; `0` = never expires. */
    val expiryMs: Long,
    /** Uses left, or `null` when unknown (unlimited, or a session-only row
     *  whose consume count the server has not reported yet). */
    val usesLeft: Long?,
    /** `true` for a row this device generated this session — its raw
     *  `blob_id` is in memory, so revocation goes through the session path. */
    val fromSession: Boolean,
)

/** Uses left from the wire `max_uses` sentinel and the server-counted
 *  consumes: `null` for unlimited, never negative. */
public fun invitesUsesLeft(maxUses: Long, usedCount: Long): Long? = when (maxUses) {
    INVITE_UNLIMITED_USES -> null
    0L -> (1L - usedCount).coerceAtLeast(0L)
    else -> (maxUses - usedCount).coerceAtLeast(0L)
}

/**
 * Merge the session-created rows with the server's list, keyed by slug: a slug
 * in both keeps the session row (it has the creation time) and takes the
 * server's uses-left; session rows come first, newest first, then the
 * server-only rows soonest-to-expire first.
 */
public fun mergeInviteLinkRows(
    session: List<InviteLinkRow>,
    server: List<InviteLinkRow>,
): List<InviteLinkRow> {
    val serverBySlug = server.associateBy { it.slug }
    val merged = session
        .sortedByDescending { it.createdAtMs ?: 0L }
        .map { row -> serverBySlug[row.slug]?.let { row.copy(usesLeft = it.usesLeft) } ?: row }
    val sessionSlugs = session.map { it.slug }.toHashSet()
    val rest = server
        .filter { it.slug !in sessionSlugs }
        .sortedWith(compareBy({ it.expiryMs == 0L }, { it.expiryMs }))
    return merged + rest
}

/** "Expires in 6d" / "Expires in 3h" / "Expired" / "Never expires". */
public fun describeInviteExpiry(expiryMs: Long, nowMs: Long): String {
    if (expiryMs == 0L) return "Never expires"
    if (expiryMs <= nowMs) return "Expired"
    val deltaMs = expiryMs - nowMs
    val days = deltaMs / (24L * 60 * 60 * 1000)
    if (days >= 1) return "Expires in ${days}d"
    val hours = deltaMs / (60L * 60 * 1000)
    if (hours >= 1) return "Expires in ${hours}h"
    val minutes = (deltaMs / (60L * 1000)).coerceAtLeast(1L)
    return "Expires in ${minutes}m"
}

/** "1 use left" / "3 uses left" / `null` when unknown. */
public fun describeUsesLeft(usesLeft: Long?): String? = when (usesLeft) {
    null -> null
    1L -> "1 use left"
    else -> "$usesLeft uses left"
}
