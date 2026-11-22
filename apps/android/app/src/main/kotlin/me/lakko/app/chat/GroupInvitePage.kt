// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.InviteLinkLengths
import me.lakko.app.l10n.UiText

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
 * Append the link secret to a finished `/g/<slug>` URL as its fragment:
 * the link takes the suffix; `Copy link` copies it
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
    /** ADR-0087 sections 7 and 15 — a channel's link: its 16-byte id. */
    val channelId: ByteArray? = null,
    /** Section 7.2 — a subscriber invitation (preview kind 3): the
     *  channel's description and its access keys; `null` for a group link
     *  and for a co-publisher link. */
    val subscriber: uniffi.lakko_ffi_uniffi.FfiSubscriberInvite? = null,
) {
    /** A co-publisher link (preview kind 2). */
    val coPublisher: Boolean get() = channelId != null && subscriber == null

    override fun equals(other: Any?): Boolean =
        other is GroupInvitePreviewUi && groupName == other.groupName && inviterName == other.inviterName &&
            memberCount == other.memberCount &&
            (channelId?.contentEquals(other.channelId) ?: (other.channelId == null)) &&
            subscriber == other.subscriber

    override fun hashCode(): Int = (groupName?.hashCode() ?: 0) * 31 + memberCount
}

/**
 * The card's text, spec section 3. [headline] is always present; [groupLine]
 * and [membersLine] only when the preview supplies them. The inviter is
 * named once, in the headline (spec section 3, amended 2026-11-22).
 */
public data class GroupInvitePageLines(
    val headline: UiText,
    /** The group's own name (data, not a catalog message), or `null`. */
    val groupLine: String?,
    val membersLine: UiText?,
)

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
    // ADR-0087 section 15.2 — a co-publisher link (kind 2) invites to publish
    // in a channel, never to a group: the one sentence, no member count (the
    // preview carries none for a channel). Seen live 2026-11-12 on an iPhone:
    // "Icons QA invites you to a group, 0 members".
    if (preview?.coPublisher == true) {
        return GroupInvitePageLines(
            headline = publisherInviteLine(inviter, groupName),
            groupLine = null,
            membersLine = null,
        )
    }
    // One catalog sentence per shape (docs/l10n/batch-4-contacts-channels.md).
    val headline = when {
        inviter != null && groupName != null -> UiText("invite.group.lead_named", texts = mapOf("name" to inviter))
        inviter != null -> UiText("invite.group.lead_named_unnamed", texts = mapOf("name" to inviter))
        groupName != null -> UiText("invite.group.lead_anonymous")
        else -> UiText("invite.group.lead_anonymous_unnamed")
    }
    val membersLine = preview?.let { memberCountLine(it.memberCount) }
    return GroupInvitePageLines(
        headline = headline,
        groupLine = groupName,
        membersLine = membersLine,
    )
}

/** UX 15.2 — "{name} invites you to be a publisher of {channel}." when the
 *  invitation names who sent it, else "You are invited to be a publisher of
 *  {channel}."; a channel the invitation does not name reads "Channel". The
 *  link page and the card in a chat say the same sentence. */
public fun publisherInviteLine(inviterName: String?, channelName: String?): UiText {
    val inviter = inviterName?.trim()?.takeIf { it.isNotEmpty() }
    val channel = channelName?.trim()?.takeIf { it.isNotEmpty() }
    val texts = buildMap {
        inviter?.let { put("name", it) }
        channel?.let { put("channel", it) }
    }
    val parts = if (channel == null) mapOf("channel" to UiText("channels.fallback_name")) else emptyMap()
    return UiText(
        if (inviter != null) "channels.publishers.invite_page_from" else "channels.publishers.invite_page",
        texts = texts,
        parts = parts,
    )
}

/** `1 member` / `N members`. */
public fun memberCountLine(count: Int): UiText =
    UiText("chat.members.count", counts = mapOf("count" to count.coerceAtLeast(0).toULong()))

/** Copy for a link that does not decode, verify, or has expired. */
public val GROUP_INVITE_INVALID_TEXT: UiText = UiText("invite.link.invalid")

/** The ledger could not be asked for a `/g/` or `/c/` link (the network, a
 *  server error): the link may be fine, so the page offers it again. The same
 *  sentence on web and iOS. */
public val INVITE_LINK_OPEN_FAILED_TEXT: UiText = UiText("common.error.offline")

/** The ledger could not be asked for a link: the network failed, or the
 *  server answered with an error other than "no such link". Says nothing
 *  about the link (seen on a local stack 2026-10-23, where the pages read it
 *  as an invalid link). A `429` travels inside as the cause; callers test for
 *  it first. */
public class InviteLinkUnreachable(cause: Throwable) : RuntimeException("the invite link could not be fetched", cause)

/** Copy for a link whose fragment does not open its preview (fail closed). */
public val GROUP_INVITE_DAMAGED_TEXT: UiText = UiText("invite.link.damaged")

/** The one control of the card. */
public val GROUP_INVITE_REQUEST_TO_JOIN_TEXT: UiText = UiText("group.join.request")

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
    /** The group the link points at, lowercase hex: how the page finds a
     *  request this device already sent (UX 6.2) or a group it already holds. */
    public val groupIdHex: String = "",
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

    /** The ledger could not be asked (the network, a server error): the link
     *  may be fine, it is offered again. */
    OpenFailed,
}

/** Classify a failure to open a `/g/` link (the page's four refusal states).
 *  Only a failure to ask the ledger is offered again; everything after the
 *  ledger answered is a link that does not work. */
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
    cause = error
    seen.clear()
    while (cause != null && seen.add(cause)) {
        if (cause is InviteLinkUnreachable) return GroupInviteOpenFailure.OpenFailed
        cause = cause.cause
    }
    return GroupInviteOpenFailure.Invalid
}
