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
 *  4. a neutral placeholder ([PLACEHOLDER_MEMBER]); a request card says
 *     [PLACEHOLDER_SOMEONE] instead.
 *
 * The key fingerprint is NEVER a name (docs/ux/client-redesign.md section 2,
 * principle 4). It stays available on [ResolvedName.fingerprint] for the
 * safety-number and debug surfaces only.
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
    /**
     * The sender's self-stated 16-byte auth account id as lowercase hex, or
     * `null` (a profile that carried none, or a row stored before the field
     * existed). A claim, like the id on a contact acknowledgement: what binds
     * it is a username certificate (ADR-0060 section 5), which names an
     * account id. (A group-call tile is no longer named through it: since
     * ADR-0078 section 2 each peer announces itself, sealed and signed.)
     */
    val accountIdHex: String? = null,
    /**
     * ADR-0060 §5 — the discovery-signed certificate of [username] as
     * lowercase hex, or `null`. A viewer shows the username only once the
     * certificate verifies for the OPRF of the username and [accountIdHex],
     * and that account's published key is the sender's.
     */
    val usernameCertificateHex: String? = null,
    /** ADR-0084 — the person's profile photo references, or `null`. */
    val avatar: AvatarRefs? = null,
    /**
     * ADR-0092 section 7 — `false` when the person does not let others share
     * their contact card (their "who can find me" is `Nobody`). A row stored
     * before the field, or a person who never sent a profile, is shareable.
     */
    val cardShareable: Boolean = true,
) {
    /**
     * Whether this is only what a search knew about the person
     * ([ProfileDirectory.onFoundPersonAnswered]): stamp 0 and nothing a client
     * composes for itself, which always states its account id. Read from the
     * fields rather than kept as a flag, so it stays true of the claim a pairing
     * state bundle carried to a new device.
     */
    public val isFoundName: Boolean
        get() = updatedAtMs == 0uL && accountIdHex == null && usernameCertificateHex == null && avatar == null

    /**
     * What a pairing state bundle carries of this profile, `null` for nothing.
     * A found username stands on this device's own verdict, which does not
     * travel: with no certificate the new device could never verify it.
     */
    public val forStateBundle: PeerProfile?
        get() = when {
            !isFoundName || username == null -> this
            displayName != null -> copy(username = null)
            else -> null
        }
}

/** The auth account id length, bytes. */
public const val ACCOUNT_ID_BYTES: Int = 16

/** [PeerProfile.accountIdHex] for the raw id a decoded profile carried:
 *  lowercase hex when it is exactly [ACCOUNT_ID_BYTES] long, else `null`. */
public fun accountIdHexOrNull(accountId: ByteArray?): String? =
    accountId?.takeIf { it.size == ACCOUNT_ID_BYTES }
        ?.joinToString("") { "%02x".format(it.toInt() and 0xff) }

/** A decoded profile envelope as the profile store takes it
 *  ([ProfileDirectory.onProfileReceived]). */
public fun peerProfileOf(profile: uniffi.lakko_ffi_uniffi.FfiProfile): PeerProfile =
    PeerProfile(
        displayName = profile.displayName,
        username = profile.username,
        updatedAtMs = profile.updatedAtMs,
        accountIdHex = accountIdHexOrNull(profile.accountId),
        usernameCertificateHex = profile.usernameCertificate?.let { me.lakko.app.reach.reachHex(it) },
        avatar = avatarRefsOf(profile.avatar),
        cardShareable = profile.cardShareable,
    )

/**
 * The roster / sender label for a person nothing is known about, in the
 * interface language at the moment it is read (`chat.members.unnamed`,
 * ADR-0061). Chat bubbles, member rows and call tiles show it.
 */
public val PLACEHOLDER_MEMBER: String
    get() = me.lakko.app.l10n.Localization.strings.value["chat.members.unnamed"]

/** The request-card label for a person nothing is known about (`contacts.request.someone`). */
public val PLACEHOLDER_SOMEONE: String
    get() = me.lakko.app.l10n.Localization.strings.value["contacts.request.someone"]

/** Everything a surface needs to label one person. */
public data class ResolvedName(
    /** The label to show: rename, profile name, `@username`, or
     *  [PLACEHOLDER_MEMBER] when nothing is known. */
    val primary: String,
    /** The verified username (no `@`), or `null` when none is verified. */
    val verifiedUsername: String?,
    /** `true` when [primary] is the neutral placeholder — nothing is known
     *  about this person beyond their key. */
    val isPlaceholder: Boolean,
    /** Contact detail only: the person's own profile name when a local rename
     *  overrides it ("Their name: X"). */
    val theirName: String?,
    /** Contact detail only: a username was claimed and verification FAILED
     *  ("Username not verified"). Never set while verification is pending. */
    val usernameNotVerified: Boolean,
    /** The key fingerprint (`a1b2 c3d4`). Never shown as a name; for the
     *  safety-number and debug surfaces. */
    val fingerprint: String,
    /** ADR-0098 — the person's account was deleted: [primary] is
     *  [DELETED_ACCOUNT_NAME], with no username, no rename and no photo. */
    val deleted: Boolean = false,
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
    /** ADR-0098 section 3 — `mayShowUsername` for this person now. `false`
     *  shows no username at all, verified or not. */
    showUsername: Boolean = true,
): ResolvedName {
    val rename = localRename?.trim()?.takeIf { it.isNotEmpty() }
    val profileName = profile?.displayName?.trim()?.takeIf { it.isNotEmpty() }
    val claimed = profile?.username?.takeIf { it.isNotEmpty() && showUsername }
    val verification = claimed?.let(verificationOf)
    val verified = claimed?.takeIf { verification == UsernameVerification.VERIFIED }
    val fingerprint = fingerprintLabel(signatureKey)
    val primary = rename ?: profileName ?: verified?.let { "@$it" } ?: PLACEHOLDER_MEMBER
    return ResolvedName(
        primary = primary,
        verifiedUsername = verified,
        isPlaceholder = rename == null && profileName == null && verified == null,
        theirName = if (rename != null && profileName != null && profileName != rename) {
            profileName
        } else {
            null
        },
        usernameNotVerified = claimed != null && verification == UsernameVerification.FAILED,
        fingerprint = fingerprint,
    )
}

/**
 * ADR-0098 — the label of a person whose account was deleted, in the interface
 * language at the moment it is read (`person.deleted.name`).
 */
public val DELETED_ACCOUNT_NAME: String
    get() = me.lakko.app.l10n.Localization.strings.value["person.deleted.name"]

/**
 * ADR-0098 — the resolution of a deleted person: [DELETED_ACCOUNT_NAME]
 * replaces every name (a local rename too), no username is shown, and the
 * card shows nothing of what the person said about themselves.
 */
public fun deletedName(signatureKey: ByteArray): ResolvedName =
    ResolvedName(
        primary = DELETED_ACCOUNT_NAME,
        verifiedUsername = null,
        isPlaceholder = false,
        theirName = null,
        usernameNotVerified = false,
        fingerprint = fingerprintLabel(signatureKey),
        deleted = true,
    )

/**
 * The name an avatar's initials are taken from: the display name alone, never
 * a list label that appends ` · @username` (seen live 2026-10-29: a member row
 * for "VG2 · @vg2" drew "V·").
 */
public fun avatarName(name: ResolvedName): String = name.primary

/** A two-line person row: the name, and the verified `@username` under it. */
public data class PersonRowText(val title: String, val subtitle: String?)

/**
 * A group's member row and the ownership-transfer picker row
 * (`docs/ux/conversation-info.md` 2.2, `docs/ux/profile-names.md` §3): the name
 * as the title, the verified `@username` once as the second line, never both on
 * the first line (seen live 2026-11-22: `G53Person · @w6g53x2201`). A person
 * known only by username is titled `@username`, with no second line.
 */
public fun personRowText(name: ResolvedName): PersonRowText {
    val line = name.verifiedUsername?.let { "@$it" }?.takeIf { it != name.primary }
    return PersonRowText(title = name.primary, subtitle = line)
}

/** The label for the sender of a request: the list label, or
 *  [PLACEHOLDER_SOMEONE] when nothing is known about them. */
public fun requestSenderLabel(name: ResolvedName): String =
    if (name.isPlaceholder) PLACEHOLDER_SOMEONE else name.listLabel

/** The incoming request card title (UX §5): `Alice Petrova · @alice wants to
 *  connect`, `@alice wants to connect`, or `Someone wants to connect`. */
public fun requestCardTitle(name: ResolvedName): String = "${requestSenderLabel(name)} wants to connect"

/** The keys already reported by [reportUnnamedPerson] in this process. */
private val reportedUnnamed: MutableSet<String> = java.util.concurrent.ConcurrentHashMap.newKeySet()

/**
 * A person is drawn with the placeholder in the app. Every sender is a
 * contact, a group member or a channel member whose name is stored on this
 * device, so this is a defect: logged once per person per process, as one
 * plain line with no identifier.
 */
public fun reportUnnamedPerson(personKeyB64: String) {
    if (!reportedUnnamed.add(personKeyB64)) return
    try {
        android.util.Log.e("LakkoNames", "a person is shown without a name in the app")
    } catch (_: RuntimeException) {
        // Host unit tests: android.util.Log is a stub.
    }
}
