// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * A message whose whole text is one `/g/` link and that carries no invitation
 * card (ADR-0087 section 14; an older message, sent before cards existed, or a
 * link pasted as text). The twin of the web's
 * `apps/web/src/chat/channel-invite-preview.ts` and iOS `BareGroupLink.swift`.
 *
 * Seen live 2026-11-13: the Chats row of such a message showed the link with
 * its secret. The rule: the link is opened through the ledger once per run;
 * until that finishes, and when the ledger does not answer or the link opens
 * to a group, the row says the link WITHOUT what follows `#`; a link that is
 * gone says `invite.link.invalid`; a channel's says the invitation. The bubble
 * says the same words while the link is not drawn as a card, and the link
 * itself only once it opened to a group (a live link, tapped to join).
 */
public sealed interface BareLinkOpening {
    /** A channel's invitation (a subscriber's or a publisher's). */
    public data class Channel(val summary: ChannelInviteSummary) : BareLinkOpening

    /** The link opened and is a group's: a live link. */
    public data object NotChannel : BareLinkOpening

    /** The ledger has no row for it (used up, revoked, expired) or the link
     *  is damaged: nothing it says can be shown. */
    public data object Dead : BareLinkOpening

    /** The ledger did not answer. */
    public data object Offline : BareLinkOpening
}

private val TOKEN = Regex("^[A-Za-z0-9_-]+$")
private const val SLUG_MAX = 512
private const val SECRET_MAX = 128

/**
 * The link [text] is when it is exactly one `/g/<slug>#<secret>` link on
 * [ownOrigin] (surrounding white space aside, no query, no user info), else
 * `null`. [ownOrigin] `null` takes any origin (a notification, which knows
 * no ledger and only hides the secret).
 */
internal fun bareGroupLinkOf(text: String, ownOrigin: String?): String? {
    val trimmed = text.trim()
    if (trimmed.isEmpty() || trimmed.any { it.isWhitespace() }) return null
    val uri = try {
        java.net.URI(trimmed)
    } catch (_: Exception) {
        return null
    }
    val scheme = uri.scheme ?: return null
    val host = uri.host ?: return null
    if (uri.rawUserInfo != null || uri.rawQuery != null) return null
    if (ownOrigin != null) {
        val own = try {
            java.net.URI(ownOrigin.trim().trimEnd('/'))
        } catch (_: Exception) {
            return null
        }
        if (!scheme.equals(own.scheme, ignoreCase = true) ||
            !host.equals(own.host, ignoreCase = true) ||
            uri.port != own.port
        ) {
            return null
        }
    }
    val path = uri.rawPath ?: return null
    val secret = uri.rawFragment ?: return null
    if (!path.startsWith("/g/")) return null
    val slug = path.removePrefix("/g/")
    if (slug.length > SLUG_MAX || !TOKEN.matches(slug)) return null
    if (secret.length > SECRET_MAX || !TOKEN.matches(secret)) return null
    return trimmed
}

/** [link] without its secret: what follows `#` is never drawn. */
internal fun linkWithoutSecret(link: String): String = link.substringBefore('#')

/**
 * The Chats row's line for a message [text], or `null` when it is not a bare
 * `/g/` link of [ownOrigin]. Pure: [opening] is the caller's.
 */
internal fun bareGroupLinkRowLine(
    text: String,
    opening: BareLinkOpening?,
    ownOrigin: String,
    strings: me.lakko.app.l10n.Strings = me.lakko.app.l10n.Localization.strings.value,
): String? {
    val link = bareGroupLinkOf(text, ownOrigin) ?: return null
    return when (opening) {
        is BareLinkOpening.Channel -> channelInvitePreviewText(opening.summary, strings)
        BareLinkOpening.Dead -> strings["invite.link.invalid"]
        BareLinkOpening.NotChannel, BareLinkOpening.Offline, null -> linkWithoutSecret(link)
    }
}

/**
 * The words a bubble draws in place of the bare link [link], or `null` when
 * it draws the link itself (it opened to a group's: a live link). A channel's
 * link is drawn as its card, which hides the text once it resolves; until
 * then the link without its secret.
 */
internal fun bareGroupLinkBubbleWords(
    link: String,
    opening: BareLinkOpening?,
    strings: me.lakko.app.l10n.Strings = me.lakko.app.l10n.Localization.strings.value,
): String? = when (opening) {
    BareLinkOpening.NotChannel -> null
    BareLinkOpening.Dead -> strings["invite.link.invalid"]
    is BareLinkOpening.Channel, BareLinkOpening.Offline, null -> linkWithoutSecret(link)
}

/**
 * What stands for a bare `/g/` link message's text in a snapshot taken of it
 * (a reply's quote, the pinned bar, a saved message): the row's line, never
 * the link's secret; `null` for any other text. The web's
 * `rowInvitationLine`. Starts the link's opening when it has not begun.
 */
internal fun bareGroupLinkSnapshotLine(
    text: String,
    ownOrigin: String = me.lakko.app.config.ApiEndpoints.webOrigin,
    openings: Map<String, BareLinkOpening> = BareGroupLinks.openings.value,
    strings: me.lakko.app.l10n.Strings = me.lakko.app.l10n.Localization.strings.value,
): String? {
    val link = bareGroupLinkOf(text, ownOrigin) ?: return null
    BareGroupLinks.start(link)
    return bareGroupLinkRowLine(text, openings[link], ownOrigin, strings)
}

/**
 * The ledger openings of bare `/g/` links, once per link per run, shared by
 * the Chats rows and the bubbles so a redraw never asks again.
 */
public object BareGroupLinks {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val started = HashSet<String>()
    private val _openings = MutableStateFlow<Map<String, BareLinkOpening>>(emptyMap())

    /** Every finished opening, by link. */
    public val openings: StateFlow<Map<String, BareLinkOpening>> = _openings.asStateFlow()

    /** Start opening [link] through the session's ledger, once. Nothing
     *  happens outside a session (no actions to open it with). */
    public fun start(link: String) {
        val actions = ChannelInviteActionsHolder.actions ?: return
        val group = groupLinkOf(link) ?: return
        synchronized(started) {
            if (!started.add(link)) return
        }
        scope.launch {
            // No answer from the ledger is shown (the link without its
            // secret) and asked again later, never kept as the answer.
            var attempt = 0
            while (true) {
                val outcome = try {
                    (ChannelInviteActionsHolder.actions ?: actions).openBareLink(group)
                } catch (e: kotlinx.coroutines.CancellationException) {
                    synchronized(started) { started.remove(link) }
                    throw e
                } catch (_: Throwable) {
                    BareLinkOpening.Offline
                }
                _openings.update { it + (link to outcome) }
                if (outcome != BareLinkOpening.Offline) break
                kotlinx.coroutines.delay(inspectRetryDelayMs(attempt))
                attempt += 1
                // Signed out meanwhile: stop; a later draw starts again.
                if (ChannelInviteActionsHolder.actions == null) {
                    synchronized(started) { started.remove(link) }
                    break
                }
            }
        }
    }

    private fun groupLinkOf(link: String): me.lakko.app.contacts.LakkoLink.Group? {
        val hash = link.indexOf('#')
        if (hash < 0) return null
        val slug = link.substring(0, hash).substringAfter("/g/", "")
        if (slug.isEmpty()) return null
        return me.lakko.app.contacts.LakkoLink.Group(slug, link.substring(hash + 1))
    }
}
