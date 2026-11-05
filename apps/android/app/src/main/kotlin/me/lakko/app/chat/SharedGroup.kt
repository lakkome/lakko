// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * `docs/ux/who-can-find-me.md` section 6.4, step 2: when no grant this device
 * holds reaches a person, a contact request goes inside a group both people
 * are current members of. Pure, so the host suite runs it without a group
 * handle.
 */

/**
 * The group to send a contact request in: among [candidates], one that
 * [isGroup] (a group, not a one-to-one, this device still a member of) whose
 * [people] include [own] and someone [isPeer]; with several, the one whose
 * newest message is newest ([newestAt], Unix ms; `null` reads as oldest).
 * `null` when there is none. A torn roster ([people] `null`) is no match.
 */
internal fun pickSharedGroup(
    candidates: Collection<String>,
    isGroup: (String) -> Boolean,
    people: (String) -> List<String>?,
    own: String,
    isPeer: (String) -> Boolean,
    newestAt: (String) -> Long?,
): String? = candidates
    .filter { hex ->
        if (!isGroup(hex)) return@filter false
        val roster = people(hex) ?: return@filter false
        own in roster && roster.any { it != own && isPeer(it) }
    }
    .maxWithOrNull(compareBy<String>({ newestAt(it) ?: Long.MIN_VALUE }, { it }))

/** The Unix ms of [thread]'s newest message, or `null` when it holds none or
 *  the stamp does not parse. */
internal fun threadNewestAtMs(thread: ChatThread): Long? {
    val stamp = thread.messages.lastOrNull()?.createdAt ?: return null
    return runCatching { java.time.Instant.parse(stamp).toEpochMilli() }
        .recoverCatching { java.time.OffsetDateTime.parse(stamp).toInstant().toEpochMilli() }
        .getOrNull()
}
