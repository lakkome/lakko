// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import uniffi.lakko_ffi_uniffi.derivePushGid
import uniffi.lakko_ffi_uniffi.derivePushWakePublicKey
import uniffi.lakko_ffi_uniffi.sealPushSubscription

/**
 * The sealed push-gw subscriptions of ADR-0078 section 3.
 *
 * One row per `(account, group)` this device is subscribed to: this device's
 * tokens ([PushDeviceTokens]) sealed under the group's unlock key, filed under
 * a random subscription id this device keeps per `push_gid`
 * ([PushRouteStore.ensureSubscriptionId]), with the group's wake public key
 * beside it so push-gw can find the row for a signed wake. The unlock key is
 * derived inside the core and never leaves it; push-gw learns it only inside
 * a sealed wake, for the one group that wake is for.
 *
 * Every network call is the caller's to bound (the routing code runs these
 * under its serializing lock).
 */
public object PushSubscriptions {

    /** The request [send] posts, or `null` when no secret is held, push is
     *  off (no token) or a derivation fails. */
    internal class Request(
        val subId: ByteArray,
        val pushGid: ByteArray,
        val wakePk: ByteArray,
        val sealed: ByteArray,
    )

    internal fun request(context: Context, accountIdB64: String, groupIdHex: String): Request? {
        val secret = PushRoutingSecretStore.entry(context, accountIdB64, groupIdHex)?.secret ?: return null
        val entries = PushDeviceTokens(context).entries()
        if (entries.isEmpty()) return null
        return try {
            val pushGid = derivePushGid(secret)
            val subId = PushRouteStore(context).ensureSubscriptionId(accountIdB64, PushClient.pseudonymHex(pushGid))
            Request(
                subId = subId,
                pushGid = pushGid,
                wakePk = derivePushWakePublicKey(secret),
                sealed = sealPushSubscription(secret, subId, entries),
            )
        } catch (_: Throwable) {
            runCatching { android.util.Log.w("lakko-push", "push subscription sealing failed") }
            null
        }
    }

    /** Send (or replace) the subscription for `(account, group)`. Returns
     *  `false` when there was nothing to send (no secret, no token). Throws
     *  on a push-gw failure. */
    public suspend fun send(context: Context, accountIdB64: String, groupIdHex: String): Boolean {
        val req = request(context, accountIdB64, groupIdHex) ?: return false
        PushClient().use { it.subscribe(req.subId, req.pushGid, req.wakePk, req.sealed) }
        return true
    }

    /** [send] for the group [accountIdB64] filed under [pushGid] (the port
     *  shape the routing code calls). */
    public suspend fun sendForPushGid(context: Context, accountIdB64: String, pushGid: ByteArray) {
        val groupIdHex = PushRouteStore(context).lookup(accountIdB64, PushClient.pseudonymHex(pushGid)) ?: return
        send(context, accountIdB64, groupIdHex)
    }

    /** Remove this device's push-gw row for [pushGid], by its subscription
     *  id. Must run BEFORE the route row (where the id lives) is deleted. */
    public suspend fun retire(context: Context, accountIdB64: String, pushGid: ByteArray) {
        val subId = PushRouteStore(context).subscriptionId(accountIdB64, PushClient.pseudonymHex(pushGid)) ?: return
        PushClient().use { it.unsubscribe(subId) }
    }

    /**
     * Send every subscription of [accountIdB64] again (a token changed, or a
     * sign-in): each one carries the tokens sealed, so a new token reaches
     * push-gw only this way. Each group runs under its routing lock
     * ([GroupPushRouting.resubscribe]), so it never interleaves a rotation.
     * Best-effort per group.
     */
    public suspend fun resubscribeAll(context: Context, accountIdB64: String) {
        for (groupIdHex in PushRouteStore(context).groupIds(accountIdB64)) {
            GroupPushRouting.resubscribe(accountIdB64, groupIdHex) {
                send(context, accountIdB64, groupIdHex)
            }
        }
    }
}
