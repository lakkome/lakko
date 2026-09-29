// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths

/**
 * The group invite page (ADR-0028 v2, `docs/ux/group-invite-page.md`) — the
 * pure half: the link secret in the URL fragment, the route argument that
 * carries the fragment with the slug, and the card copy derived from a preview.
 *
 * Since the sealed-link amendment of 2026-09-28 the WHOLE capability is
 * encrypted under a key derived from a random 12-byte **link secret**, and a
 * v2 capability additionally carries a sealed preview (group name, inviter
 * name, member count at mint time) under the same secret. The secret rides in
 * the URL **fragment** — `https://lakko.me/g/<8-char id>#<16-char secret>` —
 * because the fragment is the one part of a URL a browser never sends and an
 * app never uploads, so the discovery ledger and every access log stay
 * ciphertext-only.
 *
 * A link without its fragment, with a malformed one, whose sealed capability
 * does not open under it, or whose present preview does not open, is DAMAGED:
 * the page refuses it with one sentence and shows nothing else. A capability
 * minted without a preview still opens, and the page then shows no preview.
 * The widths come from the core ([InviteLinkLengths.core]) and are a parameter
 * so a host JVM test can pass them in.
 */

/**
 * Append the link secret to a finished `/g/<slug>` URL as its fragment. Both
 * the pointer and the inline form take the same suffix; `Copy link` copies it
 * whole.
 *
 * @throws IllegalArgumentException when [linkSecret] is not the core's secret
 *   width (12 bytes) — a mint handed the wrong bytes must not produce a link
 *   every opener then refuses as damaged.
 */
public fun appendGroupInviteLinkKey(
    url: String,
    linkSecret: ByteArray,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): String = me.lakko.app.discovery.appendInviteLinkSecret(url, linkSecret, lengths)

/** What the fragment of a tapped `/g/` link carries. */
public sealed interface GroupInviteFragment {
    /** No fragment (or a blank one): the link is damaged (sealed links). */
    public data object Absent : GroupInviteFragment

    /** A fragment that is not exactly the 16-character / 12-byte secret — the
     *  pre-amendment 43-character key included. The link is refused. */
    public data object Malformed : GroupInviteFragment

    /** The 12-byte link secret. */
    public class Secret(public val bytes: ByteArray) : GroupInviteFragment {
        override fun equals(other: Any?): Boolean = other is Secret && bytes.contentEquals(other.bytes)
        override fun hashCode(): Int = bytes.contentHashCode()
    }
}

/**
 * Read the link secret back out of a URL fragment (the text after `#`,
 * without the `#`). Absent for a missing or blank fragment; Malformed for one
 * that is not exactly [InviteLinkLengths.linkSecretUrlLen] base64url
 * characters decoding to [InviteLinkLengths.linkSecretLen] bytes.
 */
public fun parseGroupInviteFragment(
    fragment: String?,
    lengths: InviteLinkLengths = InviteLinkLengths.core,
): GroupInviteFragment {
    val text = fragment?.trim().orEmpty()
    if (text.isEmpty()) return GroupInviteFragment.Absent
    if (text.length != lengths.linkSecretUrlLen) return GroupInviteFragment.Malformed
    val bytes = try {
        base64UrlDecodeNoPad(text)
    } catch (_: IllegalArgumentException) {
        return GroupInviteFragment.Malformed
    }
    return if (bytes.size == lengths.linkSecretLen) {
        GroupInviteFragment.Secret(bytes)
    } else {
        GroupInviteFragment.Malformed
    }
}

/** The link secret of a parsed fragment, or `null` when the link is damaged
 *  (no fragment, or a malformed one). */
public fun GroupInviteFragment.secretOrNull(): ByteArray? = (this as? GroupInviteFragment.Secret)?.bytes

/** What the page makes of the opened capability's preview. */
public sealed interface GroupInvitePreviewOutcome {
    /** Show the page; [preview] is `null` for a capability minted without one. */
    public data class Shown(val preview: GroupInvitePreviewUi?) : GroupInvitePreviewOutcome

    /** The preview is present and does not open under the link's secret:
     *  refuse the link, never show it without a preview. */
    public data object Damaged : GroupInvitePreviewOutcome
}

/**
 * Decide the page's preview, fail closed, AFTER the capability itself opened
 * under [secret]. [hasPreview] is the capability's `hasPreview()`; [open]
 * opens it (`openPreview(secret)`), returning `null` when there is none and
 * throwing when it does not open. A present preview that does not open (or
 * answers nothing) is [GroupInvitePreviewOutcome.Damaged].
 */
public fun resolveGroupInvitePreview(
    hasPreview: Boolean,
    secret: ByteArray,
    open: (ByteArray) -> GroupInvitePreviewUi?,
): GroupInvitePreviewOutcome {
    if (!hasPreview) return GroupInvitePreviewOutcome.Shown(null)
    val opened = try {
        open(secret)
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (_: Exception) {
        null
    }
    return if (opened != null) GroupInvitePreviewOutcome.Shown(opened) else GroupInvitePreviewOutcome.Damaged
}

/** Thrown by [ChatRepository.inspectGroupInvite] when the link is damaged:
 *  the fragment is missing or malformed, the sealed capability does not open
 *  under it, or its preview does not. */
public class GroupInviteLinkDamaged : Exception("group invite link is damaged")

/**
 * The `/g/` slug of a full invite link, with any `#fragment` stripped — the
 * revoke path parses this, not the whole tail. `null` when [link] has no `/g/`
 * segment or an empty slug.
 */
public fun groupInviteSlugOf(link: String): String? {
    val idx = link.lastIndexOf(GROUP_INVITE_URL_PREFIX)
    if (idx < 0) return null
    val slug = link.substring(idx + GROUP_INVITE_URL_PREFIX.length)
        .substringBefore('#')
        .trim()
        .removeSuffix("/")
    return slug.ifEmpty { null }
}

/** The fragment of a full invite link (text after the first `#`), or `null`. */
public fun groupInviteFragmentOf(link: String): String? {
    val idx = link.indexOf('#')
    if (idx < 0) return null
    return link.substring(idx + 1).ifEmpty { null }
}

// =============================================================
// Route argument
// =============================================================

/**
 * What a tapped `/g/` deep-link brings into the app: the slug, and the
 * fragment when the link still had one (a chat app that strips fragments hands
 * us `null`, and the page then refuses the link as damaged).
 */
public data class GroupJoinLink(
    val slug: String,
    val fragment: String?,
)

/**
 * One navigation argument for `Routes.GROUP_JOIN` carrying both parts:
 * `<slug>` or `<slug>#<fragment>`. base64url has no `#`, so the split is
 * unambiguous; the router URL-encodes the whole value, so the `#` never reaches
 * the navigation graph as a real fragment.
 */
public fun composeGroupJoinArgument(link: GroupJoinLink): String =
    me.lakko.app.discovery.InviteLinkArgument(link.slug, link.fragment).encode()

/** Reverse of [composeGroupJoinArgument]. An empty fragment reads as absent. */
public fun parseGroupJoinArgument(argument: String): GroupJoinLink {
    val parsed = me.lakko.app.discovery.InviteLinkArgument.decode(argument)
    return GroupJoinLink(slug = parsed.slug, fragment = parsed.fragment)
}

// =============================================================
// Card copy
// =============================================================

/**
 * A decrypted preview, as the page consumes it: what the inviter's device
 * knew at mint time. Blank names are absent (the core already refuses them,
 * this is defence for the page).
 */
public data class GroupInvitePreviewUi(
    val groupName: String?,
    val inviterName: String?,
    val memberCount: Int,
)

/**
 * The card's text, spec section 3. [headline] is always present; [groupLine]
 * and [membersLine] only when the preview supplies them. The fingerprint foot
 * is [invitedByLine].
 */
public data class GroupInvitePageLines(
    val headline: String,
    val groupLine: String?,
    val membersLine: String?,
)

/** Text of the muted foot under the button: `Invited by <fingerprint>`. */
public fun invitedByLine(fingerprint: String): String = "Invited by $fingerprint"

/**
 * Derive the card copy from a preview (or its absence).
 *
 * - `<Inviter> invites you to` when the inviter's name is known, else
 *   `You are invited to`.
 * - The group name on its own line; a missing name folds `a group` into the
 *   headline and omits the line.
 * - `1 member` / `N members`, the roster at mint time; omitted with no
 *   preview.
 *
 * No preview reads `You are invited to a group` and nothing else: nothing
 * invents a name, and the page does not say why the preview is missing.
 */
public fun groupInvitePageLines(preview: GroupInvitePreviewUi?): GroupInvitePageLines {
    val inviter = preview?.inviterName?.trim()?.takeIf { it.isNotEmpty() }
    val groupName = preview?.groupName?.trim()?.takeIf { it.isNotEmpty() }
    val subject = if (inviter != null) "$inviter invites you to" else "You are invited to"
    val headline = if (groupName != null) subject else "$subject a group"
    val membersLine = preview?.let { memberCountLine(it.memberCount) }
    return GroupInvitePageLines(
        headline = headline,
        groupLine = groupName,
        membersLine = membersLine,
    )
}

/** `1 member` / `N members`. */
public fun memberCountLine(count: Int): String =
    if (count == 1) "1 member" else "$count members"

/** Copy for a link that does not decode, verify, or has expired. */
public const val GROUP_INVITE_INVALID_COPY: String = "This invite link is invalid or has expired."

/** Copy for a damaged link (fail closed); the same sentence on every kind. */
public const val GROUP_INVITE_DAMAGED_COPY: String = me.lakko.app.discovery.INVITE_LINK_DAMAGED_COPY

/** The one control of the card. */
public const val GROUP_INVITE_REQUEST_TO_JOIN: String = "Request to join"

/**
 * What [ChatRepository.inspectGroupInvite] hands the page: the issuer's key
 * fingerprint (the one thing on the page the inviter cannot fake), whether the
 * link is still within its expiry, the opened preview or `null`, and the
 * opened capability's plaintext wire.
 */
public class GroupInviteCard(
    public val fingerprint: String,
    public val valid: Boolean,
    public val preview: GroupInvitePreviewUi?,
    /** The opened capability's plaintext wire bytes: what the join request,
     *  the consume dedup tag and the join attribution are computed over. */
    public val capabilityWire: ByteArray = ByteArray(0),
)

/** Why a `/g/` link did not open, as the page shows it. */
public enum class GroupInviteOpenFailure {
    /** Does not decode, verify, resolve, or has expired. */
    Invalid,

    /** Its fragment is missing or malformed, or the link or its preview
     *  does not open under it. */
    Damaged,

    /** The discovery service answered `429`: try again later, the link may
     *  well be fine. */
    RateLimited,
}

/** Classify a failure to open a `/g/` link (the page's three refusal states). */
public fun groupInviteOpenFailure(error: Throwable): GroupInviteOpenFailure {
    var cause: Throwable? = error
    val seen = HashSet<Throwable>()
    while (cause != null && seen.add(cause)) {
        if (cause is GroupInviteLinkDamaged || cause is me.lakko.app.discovery.InviteLinkDamaged) {
            return GroupInviteOpenFailure.Damaged
        }
        cause = cause.cause
    }
    if (me.lakko.app.discovery.isDiscoveryRateLimited(error)) return GroupInviteOpenFailure.RateLimited
    return GroupInviteOpenFailure.Invalid
}
