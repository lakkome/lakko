// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import java.net.URI
import java.net.URISyntaxException
import me.lakko.app.discovery.InviteLinkArgument

/**
 * A call invitation sent as a message (`docs/ux/call-links.md` section 3) —
 * the Kotlin twin of the web `apps/web/src/calls/call-invite-link.ts`.
 *
 * `Invite a contact` sends the call's link into the conversation with that
 * person. The message is the link and nothing else, so every client can tell
 * it from ordinary text without a new wire field: a message whose whole text
 * is one Lakko call link is drawn as an invitation with a `Join` button,
 * never as a line of characters to copy.
 *
 * The check is strict on purpose. Only a link to THIS deployment counts: the
 * `Join` button opens the call inside the app, and a link that merely looks
 * like one but names another host must stay plain text, not gain a button
 * that suggests it is ours.
 *
 * Pure: the caller passes the origin this build mints its own call links
 * under ([CALL_INVITE_DEFAULT_ORIGIN]).
 */

/** Where `Join` goes: the slug and the link secret of the invitation. */
public data class CallInviteTarget(val slug: String, val secret: String) {
    /** `<slug>#<secret>`, the one-string form the app's `/c/` link handling
     *  takes (a tapped link and a pasted one arrive the same way). */
    public val argument: String get() = InviteLinkArgument(slug, secret).encode()
}

private val SLUG = Regex("^[A-Za-z0-9_-]{1,512}$")
private val SECRET = Regex("^[A-Za-z0-9_-]{1,128}$")

/**
 * The invitation a message text is, or `null` when it is ordinary text.
 *
 * [text] must be exactly one link (surrounding whitespace aside), on
 * [ownOrigin], of the shape `/c/<slug>#<secret>`: same scheme, host and
 * port, no user info, no query, nothing else in the path.
 */
public fun callInviteOf(text: String, ownOrigin: String): CallInviteTarget? {
    val trimmed = text.trim()
    if (trimmed.isEmpty() || trimmed.any { it.isWhitespace() }) return null
    val url = originOf(trimmed) ?: return null
    val own = originOf(ownOrigin.trim()) ?: return null
    if (url.origin != own.origin) return null
    val uri = url.uri
    if (uri.rawUserInfo != null || uri.rawQuery != null) return null
    // ["", "c", "<slug>"]
    val parts = (uri.rawPath ?: return null).split('/')
    if (parts.size != 3 || parts[0].isNotEmpty() || parts[1] != "c") return null
    val slug = parts[2]
    val secret = uri.rawFragment.orEmpty()
    if (!SLUG.matches(slug) || !SECRET.matches(secret)) return null
    return CallInviteTarget(slug = slug, secret = secret)
}

private class ParsedOrigin(val uri: URI, val origin: Triple<String, String, Int>)

/** The scheme, host and effective port of an `http(s)` URL, or `null` for
 *  anything else (another scheme, no host, not a URL at all). */
private fun originOf(text: String): ParsedOrigin? {
    val uri = try {
        URI(text)
    } catch (_: URISyntaxException) {
        return null
    }
    val scheme = uri.scheme?.lowercase() ?: return null
    val defaultPort = when (scheme) {
        "https" -> 443
        "http" -> 80
        else -> return null
    }
    val host = uri.host?.lowercase()?.takeIf { it.isNotEmpty() } ?: return null
    val port = if (uri.port == -1) defaultPort else uri.port
    return ParsedOrigin(uri, Triple(scheme, host, port))
}
