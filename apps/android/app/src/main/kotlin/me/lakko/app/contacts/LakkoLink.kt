// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import me.lakko.app.calls.originOf
import me.lakko.app.discovery.InviteLinkArgument

/**
 * A Lakko `/i/`, `/g/` or `/c/` link read off a URL: the one place that turns
 * a path and its `#<link secret>` fragment into what [InviteIntake] holds.
 * Both an opened app link (`MainActivity`) and a QR code scanned on the
 * Contacts screen go through it, so the two routes cannot drift.
 */
public sealed interface LakkoLink {
    /** The ledger id after the kind prefix. */
    public val slug: String

    /** The decoded `#<link secret>` fragment, or `null` when the URL had none
     *  (the landing then refuses the link as damaged). */
    public val fragment: String?

    /** `/i/` — a contact invite. */
    public data class Contact(override val slug: String, override val fragment: String?) : LakkoLink

    /** `/g/` — a group join ticket. */
    public data class Group(override val slug: String, override val fragment: String?) : LakkoLink

    /** `/c/` — a call link. */
    public data class Call(override val slug: String, override val fragment: String?) : LakkoLink
}

/** The hosts the manifest's app-link filter accepts. */
public val LAKKO_LINK_HOSTS: Set<String> = setOf("lakko.me", "test.lakko.me")

/**
 * The link a decoded URL [path] names, or `null` when the path is not
 * `/i/<slug>`, `/g/<slug>` or `/c/<slug>` with a non-empty slug. A blank
 * [fragment] counts as none.
 */
public fun lakkoLinkFromPath(path: String, fragment: String?): LakkoLink? {
    val kind = when {
        path.startsWith("/i/") -> 'i'
        path.startsWith("/g/") -> 'g'
        path.startsWith("/c/") -> 'c'
        else -> return null
    }
    val slug = path.substring(3).trim().removeSuffix("/")
    if (slug.isEmpty()) return null
    val secret = fragment?.takeIf { it.isNotBlank() }
    return when (kind) {
        'i' -> LakkoLink.Contact(slug, secret)
        'g' -> LakkoLink.Group(slug, secret)
        else -> LakkoLink.Call(slug, secret)
    }
}

/**
 * The link a scanned QR string carries, or `null` when it is not an
 * `http(s)` URL on one of [LAKKO_LINK_HOSTS] naming a Lakko link. The
 * operating system only hands the app URLs its manifest filter matched, so a
 * scanned string is held to the same scheme and hosts here.
 */
public fun lakkoLinkFromScanned(raw: String): LakkoLink? {
    val uri = runCatching { java.net.URI(raw.trim()) }.getOrNull() ?: return null
    val scheme = uri.scheme?.lowercase() ?: return null
    if (scheme != "https" && scheme != "http") return null
    val host = uri.host?.lowercase() ?: return null
    if (host !in LAKKO_LINK_HOSTS) return null
    val path = uri.path ?: return null
    return lakkoLinkFromPath(path, uri.fragment)
}

private val OWN_LINK_PATH = Regex("^/[icg]/[A-Za-z0-9_-]{8}$")
private val OWN_LINK_SECRET = Regex("^[A-Za-z0-9_-]{16}$")

/**
 * The Lakko link [url] is at [ownOrigin], or `null`: same scheme, host and
 * port, no user info, no query, a `/i/`, `/g/` or `/c/` ledger id and its
 * secret, nothing else. The Kotlin twin of the web `parseLakkoLink`. Unlike
 * [lakkoLinkFromScanned], which accepts every host the app claims, this names
 * the one deployment this build talks to, because a link that passes opens
 * inside the app.
 */
public fun lakkoLinkAt(url: String, ownOrigin: String): LakkoLink? {
    val parsed = originOf(url.trim()) ?: return null
    val own = originOf(ownOrigin.trim()) ?: return null
    if (parsed.origin != own.origin) return null
    val uri = parsed.uri
    if (uri.rawUserInfo != null || uri.rawQuery != null) return null
    val path = uri.rawPath ?: return null
    val secret = uri.rawFragment ?: return null
    if (!OWN_LINK_PATH.matches(path) || !OWN_LINK_SECRET.matches(secret)) return null
    return lakkoLinkFromPath(path, secret)
}

/** Hand [link] to the slot its kind's landing screen reads. */
public fun InviteIntake.publishLink(link: LakkoLink) {
    when (link) {
        is LakkoLink.Contact -> publish(InviteLinkArgument(link.slug, link.fragment).encode())
        is LakkoLink.Group -> publishGroupJoin(link.slug, link.fragment)
        is LakkoLink.Call -> publishCallJoin(InviteLinkArgument(link.slug, link.fragment).encode())
    }
}
