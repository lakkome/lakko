// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import uniffi.lakko_ffi_uniffi.ChannelInviteSigner

/**
 * docs/ux/second-device.md section 4a (follow-up of 2026-11-13) — the
 * account's subscriber key of a channel it owns or co-publishes is a real
 * reader key: the account's other devices read the channel under it. The
 * device that publishes the channel (holds its PublisherGroup) registers it
 * at the channels service once, and every rotation it makes reseals to it
 * ([ChannelReseal.resealKeys], whatever the admission mark says).
 *
 * A public channel takes the plain subscribe. A private one needs an
 * admission ticket and an invite proof: this device puts a one-use invite
 * key of its own (it is a publisher) with no expiry, seals the ticket under
 * the channel's current admission key and subscribes. The invite key is kept,
 * so the row's ticket stays valid for every let-in pass.
 */
public object ChannelAccountReaderKey {
    private fun key(account: String, channelId: String): String =
        "account-reader-registered|$account|${channelId.lowercase()}"

    /** Whether [publicKeyHex] was registered for [channelId] from this device. */
    public fun isRegistered(context: Context, account: String, channelId: String, publicKeyHex: String): Boolean =
        registeredKeyHex(context, account, channelId)?.equals(publicKeyHex, ignoreCase = true) == true

    /** The account reader key (hex) this device registered for [channelId],
     *  or `null`: the let-in pass never removes its row. */
    public fun registeredKeyHex(context: Context, account: String, channelId: String): String? =
        try {
            registeredKeyHex(ChannelKeyStore.encryptedPrefs(context), account, channelId)
        } catch (_: Throwable) {
            null
        }

    /** [registeredKeyHex] over the preferences the record lives in. */
    internal fun registeredKeyHex(prefs: android.content.SharedPreferences, account: String, channelId: String): String? =
        prefs.getString(key(account, channelId), null)

    private fun markRegistered(context: Context, account: String, channelId: String, publicKeyHex: String) {
        me.lakko.app.account.AccountStoreGate.write(account) {
            markRegistered(ChannelKeyStore.encryptedPrefs(context), account, channelId, publicKeyHex)
        }
    }

    /** Record [publicKeyHex] as registered for [channelId]. */
    internal fun markRegistered(prefs: android.content.SharedPreferences, account: String, channelId: String, publicKeyHex: String) {
        prefs.edit().putString(key(account, channelId), publicKeyHex.lowercase()).apply()
    }

    /** Register [keys] (the account's key of [channelId]) at the service and
     *  record it. `false` when it did not land. */
    public suspend fun register(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channelId: String,
        keys: ChannelSubscriberKeys,
    ): Boolean {
        val account = state.boundAccount ?: return false
        val pubHex = ChannelsComposer.bytesToHex(keys.publicKey)
        val channel = state.channels.value.firstOrNull { it.id == channelId }
        val private = channelVisibilityOf(context, account, channel, channelId) == false
        val done = if (private) {
            registerPrivate(context, state, client, channelId, keys)
        } else {
            try {
                client.subscribe(channelId, pubHex)
                true
            } catch (e: ChannelsApiException) {
                // Visibility not known here: the service says it is private.
                if (e.code == "sealed-ticket-required") registerPrivate(context, state, client, channelId, keys) else throw e
            }
        }
        if (done) markRegistered(context, account, channelId, pubHex)
        return done
    }

    private suspend fun registerPrivate(
        context: Context,
        state: ChannelsUiState,
        client: ChannelsClient,
        channelId: String,
        keys: ChannelSubscriberKeys,
    ): Boolean {
        val account = state.boundAccount ?: return false
        val publisher = state.publisher ?: return false
        // The owner mints the first admission seed if the channel has none
        // (the rows of today stay readers, as a first link does); a
        // co-publisher waits for the owner's.
        val (version, admissionSeed) = if (state.joinedGroupIdOf(channelId) == null) {
            if (ChannelAdmissionKeys.all(context, account, channelId).also { l -> l.forEach { it.second.fill(0) } }.isEmpty()) {
                ChannelSubscriberRows.markCurrentRowsAsReaders(context, state, client, channelId)
            }
            ChannelAdmissionKeys.currentOrMint(context, account, channelId)
        } else {
            ChannelAdmissionKeys.current(context, account, channelId) ?: return false
        }
        val inviteSeed = uniffi.lakko_ffi_uniffi.generateChannelInviteSeed()
        try {
            val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
            val admissionPublic = uniffi.lakko_ffi_uniffi.channelAdmissionPublicKey(admissionSeed)
            ChannelInviteSigner.fromInviteSeed(inviteSeed).use { signer ->
                val inviteKeyHex = ChannelsComposer.bytesToHex(signer.verifyingKey())
                // One use, no expiry: the door this subscribe walks through.
                ChannelPublisherSigners.derive(publisher, channelId).use {
                    client.putInviteKey(channelId, inviteKeyHex, 0L, 0L, it)
                }
                val proof = signer.signProof(channelBytes, keys.publicKey)
                val ticket = signer.sealTicket(admissionPublic, version, channelBytes, keys.publicKey)
                // The invite key is KEPT (no expiry): the row's ticket stays
                // valid, so the let-in pass of the device holding the mark
                // admits the row instead of removing it (as web).
                client.subscribeWithInvite(channelId, ChannelsComposer.bytesToHex(keys.publicKey), ticket, proof)
            }
            return true
        } finally {
            inviteSeed.fill(0)
            admissionSeed.fill(0)
        }
    }
}
