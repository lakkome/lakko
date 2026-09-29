// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * "Join by link" — turning what a person pasted into a `/c/` slug.
 *
 * `docs/ux/navigation.md` section 5.2. A call link reaches somebody through a
 * chat, a message, a mail; when they tap it the deep link handles it, and when
 * they copy it instead they paste it here. What arrives is therefore whatever
 * the clipboard held: the full URL, the URL with a trailing slash, a URL with a
 * query or a fragment the sharing app added, or the bare slug.
 *
 * Pure, so every one of those shapes is a unit test rather than a thing to try
 * on a device. Nothing here validates the slug — that is
 * [parseCallInviteSlug]'s job, on the landing screen, where a bad slug has a
 * place to be reported. This only decides what the person meant.
 */

/**
 * The `/c/` slug in [input], or `null` when there is none to find.
 *
 * A bare slug is accepted as itself, because a person who copied only the tail
 * of a link meant the link. The one thing that is never guessed at is a URL
 * carrying some OTHER path: `.../i/<slug>` is a contact invite and `.../g/` a
 * group ticket, and silently joining a call from either would act on something
 * the person did not ask for.
 */
public fun callInviteSlugFromInput(input: String): String? {
    var text = input.trim()
    if (text.isEmpty()) return null
    // Sealed links (2026-09-28): the fragment is the link secret and opens the
    // capability, so it is KEPT and travels with the slug as `<slug>#<secret>`;
    // the landing refuses a missing or malformed one as damaged. A query a
    // sharing app appended is dropped.
    val fragment = text.substringAfter('#', missingDelimiterValue = "").trim().ifEmpty { null }
    text = text.substringBefore('#').substringBefore('?').trimEnd('/')
    if (text.isEmpty()) return null
    val marker = CALL_INVITE_URL_PREFIX
    val at = text.lastIndexOf(marker)
    val slug = if (at >= 0) {
        text.substring(at + marker.length).takeIf { it.isNotEmpty() } ?: return null
    } else {
        // No `/c/` anywhere. A bare slug has no path separator at all; anything
        // that looks like a URL or another path is refused rather than guessed at.
        if (text.contains('/') || text.contains(':')) return null
        text
    }
    return me.lakko.app.discovery.InviteLinkArgument(slug, fragment).encode()
}
