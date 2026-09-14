// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

/**
 * ADR-0048 §5 / UX `docs/ux/profile-names.md` §3 — the ONE display resolver.
 *
 * Every surface that shows a person (contacts, request cards, chat headers,
 * author labels, typing, member lists, call rosters, notifications) asks this
 * function, so the precedence cannot drift between screens:
 *
 *  1. a name the viewer gave the contact themselves (local rename);
 *  2. the person's own profile name;
 *  3. the person's VERIFIED `@username`;
 *  4. the key fingerprint, WITHOUT `@` (`a1b2 c3d4`).
 *
 * ## A username is shown only when verified
 *
 * An unverified claim is not rendered at all — not greyed, not with a question
 * mark. A displayed `@pavel` that is not Pavel is the impersonation ADR-0048
 * exists to prevent, so the claimed username contributes NOTHING to
 * [ResolvedName.primary] or [ResolvedName.verifiedUsername] until the
 * verification result for exactly that `(key, username)` pair is VERIFIED.
 *
 * ## The fingerprint never carries `@`
 *
 * The old label (`@` + a hex prefix) read like a username and was not one.
 */

/** Verification result of a claimed username, cached per `(key, username)`. */
public enum class UsernameVerification {
    /** Discovery resolved the username to this very signature key. */
    VERIFIED,

    /** Discovery resolved it to a different key, or it is not registered, or
     *  it fails the username rule. Never shown for this key. */
    FAILED,
}

/** A person's received profile claim, bound to their signature key. */
public data class PeerProfile(
    /** Self-stated display name, or `null` for no name. */
    val displayName: String?,
    /** Claimed username (canonical form), or `null`. A CLAIM until verified. */
    val username: String?,
    /** Sender-clock stamp; the recency order key (ADR-0048 §1). */
    val updatedAtMs: ULong,
)

/** Everything a surface needs to label one person. */
public data class ResolvedName(
    /** The label to show: rename, profile name, `@username`, or fingerprint. */
    val primary: String,
    /** The verified username (no `@`), or `null` when none is verified. */
    val verifiedUsername: String?,
    /** `true` when [primary] is the fingerprint (render it as a key). */
    val isFingerprint: Boolean,
    /** Contact detail only: the person's own profile name when a local rename
     *  overrides it ("Their name: X"). */
    val theirName: String?,
    /** Contact detail only: a username was claimed and verification FAILED
     *  ("Username not verified"). Never set while verification is pending. */
    val usernameNotVerified: Boolean,
    /** The fingerprint, always available for detail views. */
    val fingerprint: String,
) {
    /**
     * The list-row label: `Name · @username` when a username is verified and
     * the primary is not already that username, else just [primary]. Chat
     * bubbles and call tiles use [primary] alone (UX §3).
     */
    public val listLabel: String
        get() {
            val user = verifiedUsername ?: return primary
            val at = "@$user"
            return if (primary == at) primary else "$primary · $at"
        }
}

/** The key fingerprint without `@`: the first four bytes as `a1b2 c3d4`. */
public fun fingerprintLabel(signatureKey: ByteArray): String {
    val hex = signatureKey.take(4).joinToString("") { "%02x".format(it.toInt() and 0xff) }
    return hex.chunked(4).joinToString(" ")
}

/**
 * Resolve one person's name.
 *
 * @param localRename the viewer's own name for the contact, or `null`.
 * @param profile the person's latest accepted profile claim, or `null`.
 * @param verificationOf the cached verification result for a
 *   `(this key, username)` pair, or `null` when not (yet) known.
 * @param signatureKey the person's signature public key (fingerprint source).
 */
public fun resolveName(
    localRename: String?,
    profile: PeerProfile?,
    verificationOf: (String) -> UsernameVerification?,
    signatureKey: ByteArray,
): ResolvedName {
    val rename = localRename?.trim()?.takeIf { it.isNotEmpty() }
    val profileName = profile?.displayName?.trim()?.takeIf { it.isNotEmpty() }
    val claimed = profile?.username?.takeIf { it.isNotEmpty() }
    val verification = claimed?.let(verificationOf)
    val verified = claimed?.takeIf { verification == UsernameVerification.VERIFIED }
    val fingerprint = fingerprintLabel(signatureKey)
    val primary = rename ?: profileName ?: verified?.let { "@$it" } ?: fingerprint
    return ResolvedName(
        primary = primary,
        verifiedUsername = verified,
        isFingerprint = rename == null && profileName == null && verified == null,
        theirName = if (rename != null && profileName != null && profileName != rename) {
            profileName
        } else {
            null
        },
        usernameNotVerified = claimed != null && verification == UsernameVerification.FAILED,
        fingerprint = fingerprint,
    )
}

/** The incoming request card title (UX §5): `Alice Petrova · @alice wants to
 *  connect`, or `@alice` / the fingerprint when that is all there is. */
public fun requestCardTitle(name: ResolvedName): String = "${name.listLabel} wants to connect"
