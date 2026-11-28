// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import me.lakko.app.l10n.UiText

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
    /** ADR-0084 — the person's signature key in either base64 spelling, for
     *  rows whose [key] is not one (an invited contact); `null` = [key]. */
    val personKey: String? = null,
)

/** A letter section of the alphabetical list: the header and its rows. */
public data class ContactSection(val letter: String, val rows: List<ContactRowModel>)

/** What a row of Contacts > Sent says: [title], then [username] when it is more
 *  than the title already said. */
public data class SentRowText(val title: String, val username: String?)

/** A Sent row for the name a person was found under and their username (empty
 *  for a person found by name alone). */
public fun sentRowText(name: String?, username: String): SentRowText {
    val handle = username.takeIf { it.isNotEmpty() }?.let { "@$it" }
    val shown = name?.trim()?.takeIf { it.isNotEmpty() }
    return when {
        shown == null -> SentRowText(handle.orEmpty(), null)
        shown == handle -> SentRowText(shown, null)
        else -> SentRowText(shown, handle)
    }
}

/**
 * The people [sections] lists, as standard base64 account keys. The People part
 * under the list leaves each of them out, whatever their reach, so one person
 * stands once (`docs/ux/who-can-find-me.md` section 6.1). A contact's key comes
 * from [contactKeyB64] by row key; a pending contact's row carries it in
 * [ContactRowModel.personKey].
 */
public fun listedPersonKeys(
    sections: List<ContactSection>,
    contactKeyB64: (rowKey: String) -> String?,
): Set<String> =
    sections.flatMap { it.rows }.mapNotNullTo(HashSet()) { row -> contactKeyB64(row.key) ?: row.personKey }

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

/** The signature key a route key names, or `null` when it is not base64url. */
public fun routeKeyBytes(routeKey: String): ByteArray? = try {
    me.lakko.app.auth.base64UrlDecodeNoPad(routeKey)
} catch (_: IllegalArgumentException) {
    null
}

/**
 * The card of a person with no contact row whose one-to-one remains — a
 * person the viewer removed (docs/ux/conversation-info.md section 2.1,
 * amended 2026-10-24). Built from the conversation, as the one-to-one rule
 * builds a row ([me.lakko.app.contacts.adoptingOneToOnePeer]), but never
 * stored: the card offers `Add to contacts`, which stores it. `null` when the
 * route key is malformed or [chatFor] finds no one-to-one with the person.
 */
public fun removedPeerContact(
    routeKey: String,
    chatFor: (peerKey: ByteArray) -> String?,
): me.lakko.app.contacts.AcceptedContact? {
    val key = routeKeyBytes(routeKey) ?: return null
    val groupIdHex = chatFor(key) ?: return null
    return me.lakko.app.contacts.adoptingOneToOnePeer(
        accepted = emptyList(),
        signatureKey = key,
        accountId = null,
        groupIdHex = groupIdHex,
        nowMs = 0uL,
        createdAt = "",
    )?.singleOrNull()
}

// =============================================================================
// Invite links sheet
// =============================================================================

/** Wire-format `max_uses` sentinel for an unlimited invite. */
internal const val INVITE_UNLIMITED_USES: Long = 0xFFFF_FFFFL

/** UX 5.3 — the line a row shows in place of Copy and Share when this device
 *  does not hold the link's secret. Delete link stays. */
public val INVITE_CREATED_ELSEWHERE_COPY: UiText = UiText("invite.links.made_elsewhere_hint")

/** One row of the "Invite links" sheet. */
public data class InviteLinkRow(
    /** The `/i/<slug>` segment; the row identity. */
    val slug: String,
    /** The full shareable URL with its `#<secret>` fragment, or `null` when
     *  this device does not hold the link's secret: the server never holds it
     *  (sealed links), so such a row (created on another device) can be
     *  revoked but not re-shared here. A link this device created keeps its
     *  secret across restarts, so its server row carries the URL. */
    val url: String?,
    /** Owner label, or empty. */
    val label: String,
    /** When this device created the link, or `null` when only the server
     *  knows the link (created on another device or in an earlier session). */
    val createdAtMs: Long?,
    /** UNIX millis; `0` = never expires. */
    val expiryMs: Long,
    /** The `max_uses` sentinel the link was minted with. ADR-0078 section
     *  10: the list is this device's own records, so how many uses are left
     *  is not known here; the row states the limit. */
    val maxUses: Long,
    /** `true` for a row this device generated this session — its raw
     *  `blob_id` is in memory, so revocation goes through the session path. */
    val fromSession: Boolean,
)

/** The title line of [row]: its label, or `null` for a link without one, so
 *  the row leads with its meta line instead of a generic "Invite link". */
public fun inviteRowTitle(row: InviteLinkRow): String? = row.label.trim().ifEmpty { null }

/** What a row offers: Copy and Share with the full URL, or the
 *  [INVITE_CREATED_ELSEWHERE_COPY] line; Delete link is offered either way. */
public sealed interface InviteRowShare {
    public data class Shareable(val url: String) : InviteRowShare

    public data object CreatedElsewhere : InviteRowShare
}

/** The share half of [row] (UX 5.3). */
public fun inviteRowShare(row: InviteLinkRow): InviteRowShare =
    row.url?.let { InviteRowShare.Shareable(it) } ?: InviteRowShare.CreatedElsewhere

/**
 * Merge the session-created rows with the recorded list, keyed by slug: a
 * slug in both keeps the session row; session rows come first, newest first,
 * then the recorded-only rows soonest-to-expire first.
 */
public fun mergeInviteLinkRows(
    session: List<InviteLinkRow>,
    server: List<InviteLinkRow>,
): List<InviteLinkRow> {
    val serverBySlug = server.associateBy { it.slug }
    val merged = session
        .sortedByDescending { it.createdAtMs ?: 0L }
        .map { row -> serverBySlug[row.slug]?.let { row.copy(createdAtMs = row.createdAtMs ?: it.createdAtMs) } ?: row }
    val sessionSlugs = session.map { it.slug }.toHashSet()
    val rest = server
        .filter { it.slug !in sessionSlugs }
        .sortedWith(compareBy({ it.expiryMs == 0L }, { it.expiryMs }))
    return merged + rest
}

/** "Expires in 6d" / "Expires in 3h" / "Expired" / "Never expires" (ADR-0061: catalog messages). */
public fun describeInviteExpiry(expiryMs: Long, nowMs: Long): UiText {
    if (expiryMs == 0L) return UiText("invite.links.never_expires")
    if (expiryMs <= nowMs) return UiText("invite.links.expired")
    val deltaMs = expiryMs - nowMs
    // Same rule on all three clients: minutes round up (so "30 s" reads
    // "1m"), hours and days round half up (so a 7-day link made an hour
    // ago still reads "7d", not a floored "6d").
    val minuteMs = 60_000L
    val hourMs = 60 * minuteMs
    val dayMs = 24 * hourMs
    val minutes = (deltaMs + minuteMs - 1) / minuteMs
    if (minutes >= 60) {
        val hours = (deltaMs + hourMs / 2) / hourMs
        if (hours < 24) return UiText("invite.links.expires_hours", texts = mapOf("count" to hours.toString()))
        val days = (deltaMs + dayMs / 2) / dayMs
        return UiText("invite.links.expires_days", texts = mapOf("count" to days.toString()))
    }
    return UiText("invite.links.expires_minutes", texts = mapOf("count" to minutes.toString()))
}

/** The limit a link was minted with: "single-use" / "up to 5 uses" /
 *  "unlimited" (the web `describeInviteLimit`, iOS `limitCopy`). */
public fun describeInviteLimit(maxUses: Long): UiText = when {
    maxUses == INVITE_UNLIMITED_USES -> UiText("invite.uses.unlimited")
    maxUses <= 1L -> UiText("invite.uses.single")
    else -> UiText("invite.uses.up_to", counts = mapOf("count" to maxUses.toULong()))
}

/**
 * UX 16.11 (alignment 2026-11-27): the card of a key recorded as started
 * over (its account reset its keys without a seal; the old conversation is
 * closed) never offers `Add to contacts`: that key belongs to an account that
 * no longer exists. The person's new account is added by `@name`.
 */
public fun withoutAddForStartedOver(
    sections: List<uniffi.lakko_ffi_uniffi.InfoSection>,
    keyStartedOver: Boolean,
): List<uniffi.lakko_ffi_uniffi.InfoSection> =
    if (keyStartedOver) sections.filter { it != uniffi.lakko_ffi_uniffi.InfoSection.ADD_CONTACT } else sections
