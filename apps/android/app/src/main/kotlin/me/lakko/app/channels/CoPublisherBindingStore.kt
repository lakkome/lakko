// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context

/**
 * ADR-0059 section 1 / ADR-0087 phase B — the co-publisher keys this device
 * has verified, kept on disk per account so an owner device can withdraw a
 * removed publisher's appointment (`DELETE .../copublishers/{key}`) after a
 * restart, after the knock that carried the binding expired, or when another
 * device of the account appointed them.
 *
 * A binding is learnt from two places, both verified by the core's decode
 * (the account key's signature) and checked by the caller against the account
 * the binding names: the joiner's knock, and the binding a co-publisher posts
 * into the PublisherGroup journal once after it joins (every publisher device
 * reads the journal; only the account the MLS sender resolves to is taken).
 * The key is public (it is appointed on the channels service), so nothing here
 * is a secret; it is still removed with the account.
 */
public object CoPublisherBindingStore {
    private const val PREFS = "me.lakko.channels.copublisher-bindings"

    @Volatile
    private var appContext: Context? = null

    /** Called once by the publisher loop. */
    public fun init(context: Context) {
        appContext = context.applicationContext
    }

    private fun prefs(context: Context? = appContext) =
        context?.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun account(): String? = me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64

    private fun hex(b: ByteArray): String = ChannelsComposer.bytesToHex(b)

    private fun slot(acct: String, groupId: ByteArray, accountKey: ByteArray): String =
        "b|$acct|${hex(groupId)}|${hex(accountKey)}"

    /** File [copublisherKey] as the key the account [accountKey] appointed in
     *  the PublisherGroup [groupId]. */
    public fun put(groupId: ByteArray, accountKey: ByteArray, copublisherKey: ByteArray) {
        val acct = account() ?: return
        prefs()?.edit()?.putString(slot(acct, groupId, accountKey), hex(copublisherKey))?.apply()
    }

    /** The co-publisher key filed for [accountKey] in [groupId], or `null`. */
    public fun get(groupId: ByteArray, accountKey: ByteArray): ByteArray? {
        val acct = account() ?: return null
        val v = prefs()?.getString(slot(acct, groupId, accountKey), null) ?: return null
        if (v.length != 64) return null
        return ByteArray(32) { i -> (v.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null).toByte() }
    }

    /** Whether this device already posted its own binding for [channelId]
     *  at [epoch]. It is posted again after each epoch change, since a
     *  device of the owner reads only the epochs it holds. */
    public fun posted(channelId: String, epoch: ULong): Boolean {
        val acct = account() ?: return true
        val p = prefs() ?: return true
        return p.getLong("p|$acct|$channelId", -1L) == epoch.toLong()
    }

    /** Record that this device posted its own binding for [channelId] at
     *  [epoch]. */
    public fun markPosted(channelId: String, epoch: ULong) {
        val acct = account() ?: return
        prefs()?.edit()?.putLong("p|$acct|$channelId", epoch.toLong())?.apply()
    }

    /** Remove everything of [accountIdB64]. */
    public fun forgetAccount(context: Context, accountIdB64: String) {
        val p = prefs(context) ?: return
        val edit = p.edit()
        for (k in p.all.keys) {
            if (k.startsWith("b|$accountIdB64|") || k.startsWith("p|$accountIdB64|")) edit.remove(k)
        }
        edit.commit()
    }
}
