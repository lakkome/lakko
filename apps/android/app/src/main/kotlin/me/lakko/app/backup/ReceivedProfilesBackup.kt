// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import java.util.Base64
import me.lakko.app.profile.PeerProfile
import me.lakko.app.profile.ProfileOffer
import uniffi.lakko_ffi_uniffi.BackupReceivedProfile

/*
 * ADR-0048 with `lakko-backup` v8 — the names this device learned of other
 * people travel in the backup, so a device restored with no other device
 * alive names its contacts and group members at once instead of waiting for
 * each to republish.
 *
 * Written: every stored profile under a 32-byte person key that is not this
 * account's own, recomposed as the profile envelope with its ORIGINAL stamp
 * (the same envelope the pairing state bundle carries). The account's own
 * contacts first, then everyone else by the profile's stamp, newest first:
 * this device keeps no per-person "last used", and the core drops what does
 * not fit from the end. Restored: each envelope through the store's ordinary
 * receive gate (strictly newer stamp per key), never under an own key.
 */

/** The received profiles a backup carries, in the order they are kept. */
internal fun receivedProfilesForBackup(
    stored: Map<String, PeerProfile>,
    contactKeysB64: Set<String>,
    ownKeysB64: Set<String>,
    ownAccountIdHex: String?,
    compose: (PeerProfile) -> ByteArray?,
): List<BackupReceivedProfile> {
    val dec = Base64.getDecoder()
    val enc = Base64.getEncoder()
    val candidates = ArrayList<Triple<ByteArray, PeerProfile, Boolean>>()
    val seen = HashSet<String>()
    for ((rawKey, profile) in stored) {
        val keyBytes = try {
            dec.decode(rawKey)
        } catch (_: IllegalArgumentException) {
            continue
        }
        if (keyBytes.size != PERSON_KEY_LEN) continue
        val key = enc.encodeToString(keyBytes)
        if (key in ownKeysB64 || !seen.add(key)) continue
        if (ownAccountIdHex != null && profile.accountIdHex == ownAccountIdHex) continue
        val carried = profile.forStateBundle ?: continue
        candidates += Triple(keyBytes, carried, key in contactKeysB64)
    }
    val ordered = candidates.sortedWith(
        compareByDescending<Triple<ByteArray, PeerProfile, Boolean>> { it.third }
            .thenByDescending { it.second.updatedAtMs }
            .thenBy { enc.encodeToString(it.first) },
    )
    return ordered.mapNotNull { (key, profile, _) ->
        compose(profile)?.takeIf { it.isNotEmpty() }?.let { BackupReceivedProfile(personKey = key, envelope = it) }
    }
}

/** The receiving half: each envelope through [offer] (the store's strictly
 *  newer gate) under its person key; never an own key or a profile naming
 *  the own account; an envelope that does not decode is skipped. Returns how
 *  many were stored. */
internal fun applyReceivedProfilesFromBackup(
    entries: List<BackupReceivedProfile>,
    ownKeysB64: Set<String>,
    ownAccountIdHex: String?,
    decode: (ByteArray) -> PeerProfile?,
    offer: (String, PeerProfile) -> ProfileOffer,
): Int {
    val enc = Base64.getEncoder()
    var stored = 0
    for (e in entries) {
        if (e.personKey.size != PERSON_KEY_LEN) continue
        val key = enc.encodeToString(e.personKey)
        if (key in ownKeysB64) continue
        val profile = decode(e.envelope) ?: continue
        if (ownAccountIdHex != null && profile.accountIdHex == ownAccountIdHex) continue
        if (offer(key, profile) == ProfileOffer.STORED) stored += 1
    }
    return stored
}

/** [receivedProfilesForBackup] over this device's stores for [accountIdB64]. */
internal fun receivedProfilesOnDevice(context: Context, accountIdB64: String): List<BackupReceivedProfile> {
    val owner = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
    val stored = try {
        me.lakko.app.profile.SharedPrefsProfilePersistence(context).loadProfiles(owner)
    } catch (_: Throwable) {
        return emptyList()
    }
    if (stored.isEmpty()) return emptyList()
    val contacts = try {
        me.lakko.app.contacts.AcceptedContactsStore.load(context, owner)
            .map { Base64.getEncoder().encodeToString(it.request.fromPubkey) }.toSet()
    } catch (_: Throwable) {
        emptySet()
    }
    return receivedProfilesForBackup(
        stored = stored,
        contactKeysB64 = contacts,
        ownKeysB64 = me.lakko.app.devices.StateBundle.ownKeysOnDevice(context, owner, emptyList()),
        ownAccountIdHex = accountIdHexOf(owner),
        compose = me.lakko.app.devices.StateBundle.FfiEnvelopeCodecs.composeProfile,
    )
}

/** [applyReceivedProfilesFromBackup] into this device's profile store. */
internal fun restoreReceivedProfilesOnDevice(
    context: Context,
    accountIdB64: String,
    entries: List<BackupReceivedProfile>,
): Int {
    if (entries.isEmpty()) return 0
    val owner = me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
    return applyReceivedProfilesFromBackup(
        entries = entries,
        ownKeysB64 = me.lakko.app.devices.StateBundle.ownKeysOnDevice(context, owner, emptyList()),
        ownAccountIdHex = accountIdHexOf(owner),
        decode = me.lakko.app.devices.StateBundle.FfiEnvelopeCodecs.decodeProfile,
        offer = { key, profile ->
            me.lakko.app.profile.ProfileDirectory.get(context).offerForAccount(owner, key, profile)
        },
    )
}

private const val PERSON_KEY_LEN: Int = 32

private fun accountIdHexOf(accountIdB64: String): String? =
    try {
        Base64.getDecoder().decode(accountIdB64).takeIf { it.size == 16 }
            ?.joinToString("") { "%02x".format(it.toInt() and 0xff) }
    } catch (_: IllegalArgumentException) {
        null
    }
