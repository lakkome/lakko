// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import uniffi.lakko_ffi_uniffi.FfiProfile

/*
 * ADR-0048 section 4a — the own name between the account's own devices. The
 * own profile envelope rides the DeviceGroup `ServiceEnvelope` carrier
 * (ADR-0060 section 7) beside the Visibility setting and the ReachState.
 *
 * The rules, host-testable (the composer is passed in):
 *   - sent with the name's STORED stamp ([OwnProfile.nameUpdatedAtMs]), never
 *     a fresh one: a re-send (the hand-over to an admitted device) must not
 *     arrive newer than a later change made elsewhere;
 *   - the display name only: no username (the receiver keeps its session's),
 *     no username certificate and no grants; the account id is stated so a
 *     receiver can refuse one that is not its own;
 *   - an account that never saved a name (stamp 0) sends nothing;
 *   - a receiver keeps the name only when its stamp is STRICTLY greater than
 *     the one held, and keeps that stamp as is. It never republishes it to
 *     its groups: the device that made the change already has.
 */

/** The own profile as it goes to the account's other devices, or `null`
 *  when no name was ever saved (nothing to send). */
public fun ownProfileForDevices(profile: OwnProfile?, accountId: ByteArray): FfiProfile? {
    if (profile == null || profile.nameUpdatedAtMs <= 0L) return null
    return FfiProfile(
        displayName = profile.displayName,
        username = null,
        updatedAtMs = profile.nameUpdatedAtMs.toULong(),
        accountId = accountId.takeIf { it.size == ACCOUNT_ID_BYTES }?.copyOf(),
        usernameCertificate = null,
        grants = emptyList(),
        // ADR-0084 — the photo travels with the name, under the same stamp.
        avatar = profile.avatar?.toFfi(),
        // ADR-0092 section 7 — the receiving device keeps its own flag (it
        // follows the synced setting); stated truthfully all the same.
        cardShareable = profile.cardShareable,
    )
}

/** The composed envelope of [ownProfileForDevices] ([compose]:
 *  `composeProfileEnvelope`), or `null` when there is nothing to send or the
 *  core refuses it (never sent in a weakened form). */
public fun ownProfileDeviceEnvelope(
    profile: OwnProfile?,
    accountId: ByteArray,
    compose: (FfiProfile) -> ByteArray,
): ByteArray? {
    val wire = ownProfileForDevices(profile, accountId) ?: return null
    return try {
        compose(wire)
    } catch (_: Throwable) {
        null
    }
}

/**
 * The own profile envelope a backup carries: what the account's devices send
 * each other plus the username, with the name's stored stamp, or `null` when
 * no name or photo was ever saved. A restore on a device with nothing has no
 * other source for either.
 */
public fun ownProfileBackupEnvelope(
    profile: OwnProfile?,
    accountId: ByteArray,
    compose: (FfiProfile) -> ByteArray,
): ByteArray? {
    val wire = ownProfileForDevices(profile, accountId) ?: return null
    return try {
        compose(wire.copy(username = profile?.username?.takeIf { it.isNotEmpty() }))
    } catch (_: Throwable) {
        null
    }
}

/**
 * What a restore keeps of the backup's own profile: [adoptOwnProfileFromDevice]
 * (nothing when this device holds a name as new, or the envelope names another
 * account), with the backup's username when this device states none yet.
 */
public fun adoptOwnProfileFromBackup(
    stored: OwnProfile?,
    incoming: FfiProfile,
    ownAccountId: ByteArray,
): OwnProfile? =
    adoptOwnProfileFromDevice(stored, incoming, ownAccountId)?.let { adopted ->
        if (adopted.username == null) adopted.copy(username = incoming.username) else adopted
    }

/**
 * What this device keeps of an own profile [incoming] from another of the
 * account's devices: the profile to store, or `null` when nothing changes —
 * a profile naming another account, or a stamp not strictly greater than
 * the name's stamp held here ([stored], `null` when none: compared as 0).
 *
 * The username stays the one stored (the session's); [OwnProfile.updatedAtMs]
 * never goes back, so a group re-send of this profile is never older than
 * one this device already published.
 */
public fun adoptOwnProfileFromDevice(
    stored: OwnProfile?,
    incoming: FfiProfile,
    ownAccountId: ByteArray,
): OwnProfile? {
    val statedAccount = incoming.accountId
    if (statedAccount != null && !statedAccount.contentEquals(ownAccountId)) return null
    val stamp = incoming.updatedAtMs
    if (stamp > Long.MAX_VALUE.toULong()) return null
    val incomingMs = stamp.toLong()
    if (incomingMs <= (stored?.nameUpdatedAtMs ?: 0L)) return null
    return OwnProfile(
        displayName = incoming.displayName,
        username = stored?.username,
        updatedAtMs = maxOf(stored?.updatedAtMs ?: 0L, incomingMs),
        nameUpdatedAtMs = incomingMs,
        avatar = avatarRefsOf(incoming.avatar),
        // Each device derives the flag from the synced setting itself.
        cardShareable = stored?.cardShareable ?: true,
    )
}

/**
 * The Settings > Profile field after the stored name changed to [incoming]
 * (ADR-0048 section 4a): it follows while it still shows [mirrored], the
 * stored name it last showed, and keeps what the person typed otherwise.
 */
internal fun followSavedName(draft: String, mirrored: String, incoming: String): String =
    if (draft == mirrored) incoming else draft
