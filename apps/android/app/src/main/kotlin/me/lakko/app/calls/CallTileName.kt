// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import me.lakko.app.profile.ProfileDirectory
import me.lakko.app.profile.ResolvedName

/**
 * The name on a remote call tile — `docs/ux/calls.md`, "Every tile carries the
 * person's name". The Kotlin twin of the web `calls/tile-name.ts`.
 *
 * A tile always carries the person's name. One rule for both call kinds, in
 * this order:
 *
 *  1. what the one resolver (`ProfileDirectory.resolve`) calls the person
 *     behind the tile: the viewer's rename, the person's profile name, their
 *     verified `@username`. The placeholder it returns for a person it knows
 *     nothing about is not a name and is skipped;
 *  2. the display name the person announced inside the call (link calls), for
 *     the moment before the profile store has filed it;
 *  3. the username the person claims while it is not verified, as plain text
 *     without `@`. The `@` form is shown only for a verified username
 *     (ADR-0048); a bare word claims nothing a typed display name could not.
 *
 * `null` only while nothing has reached the device from that person; the tile
 * then carries an empty name and the call screen draws its unnamed state
 * (`call.participant.unnamed`, no number). That state is meant to last
 * moments: everyone has a name or a username to announce, and the link call
 * keeps the announcements that arrive before it can read them ([EarlyRelays]).
 *
 * Never a piece of an account id or of a key. The SFU hands every client an
 * `account_pseudonym` (the first characters of the account id); a tile
 * labelled with it reads like something the viewer ought to recognise and
 * names nobody, and it is what a person without a profile name was shown as
 * until 2026-10-02.
 *
 * Pure and Android-free, so the rule is unit-tested on the host JVM.
 */

/** The name the sources give, or `null` while the person is still unnamed.
 *  Each argument is `null` (or blank) when that source has nothing. */
public fun callTileName(resolved: String?, announcedName: String?, claimedUsername: String?): String? {
    for (candidate in arrayOf(resolved, announcedName, claimedUsername)) {
        val name = candidate?.trim().orEmpty()
        if (name.isNotEmpty()) return name
    }
    return null
}

/** A resolved person's name for a tile: `null` when the resolver knows nothing
 *  about them beyond their key. */
public fun personNameOrNull(resolved: ResolvedName): String? =
    if (resolved.isPlaceholder) null else resolved.primary

/**
 * A person's name on a call tile: the one resolver every screen names people
 * through, then the username their stored profile claims, as plain text, while
 * it is not verified.
 */
public fun callNameOfPerson(directory: ProfileDirectory, signatureKeyB64: String): String? =
    callTileName(
        resolved = personNameOrNull(directory.resolve(signatureKeyB64)),
        announcedName = null,
        claimedUsername = directory.claimedUsernameOf(signatureKeyB64),
    )

/**
 * Whether the account a sealed profile claims is the account the SFU admitted
 * the relaying peer as.
 *
 * A profile in a link call comes from a stranger, and a username certificate
 * binds a username to an ACCOUNT ID, not to the key that sent the profile. The
 * ADR-0021 peer tag is derived from the account id in the peer's
 * call-admission token, so a profile is filed in the profile store, and its
 * username may ever read as verified, only when the claimed account id gives
 * that peer's tag. A participant therefore cannot present another account's id
 * together with that account's certificate.
 *
 * @param tagOf the ADR-0021 derivation for this call, `sfuPeerTag(callId, ·)`.
 */
public fun profileClaimMatchesPeer(
    claimedAccountIdHex: String?,
    peerTag: String?,
    tagOf: (accountIdHex: String) -> String,
): Boolean {
    if (claimedAccountIdHex == null || peerTag.isNullOrEmpty()) return false
    return try {
        tagOf(claimedAccountIdHex) == peerTag
    } catch (_: Throwable) {
        false
    }
}

/** How many relays a link call keeps while it cannot read them yet. */
public const val EARLY_RELAY_LIMIT: Int = 64

/**
 * The relays that reached a joiner before its Welcome.
 *
 * A member's sealed profile and the committer's Welcome come from different
 * senders, so nothing orders them: the profile can arrive first, when there is
 * no group to open it with. Dropped, that member's tile stayed unnamed until
 * someone else was admitted. Kept here, bounded, and handed back once the
 * group exists.
 */
public class EarlyRelays<T> {
    private val held = ArrayDeque<T>()

    /** Keep one; past the limit the oldest is dropped. */
    public fun keep(relay: T) {
        held.addLast(relay)
        if (held.size > EARLY_RELAY_LIMIT) held.removeFirst()
    }

    /** Everything kept, oldest first; the queue is empty afterwards. */
    public fun drain(): List<T> {
        val out = held.toList()
        held.clear()
        return out
    }
}
