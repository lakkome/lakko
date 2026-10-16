// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import me.lakko.app.BuildConfig

/**
 * push-gw's X25519 wake key (ADR-0078 section 3).
 *
 * Every `x-lakko-wake` header is a wake sealed to this key, so the Delivery
 * Service forwards a blob it cannot read. Two sources, the `DeviceAttestKey`
 * pattern:
 *
 *   * a **build-time pin**, `BuildConfig.LAKKO_PUSH_WAKE_KEY` (base64url of 32
 *     bytes): when set, the key is never fetched;
 *   * otherwise `GET /push/wake-key`, fetched once per process and cached.
 *
 * A wrong key costs a wake (push-gw cannot open it), never confidentiality.
 * When the key cannot be had, a publish goes out without a wake and
 * recipients catch up on their next poll.
 */
public object PushWakeKey {

    @Volatile
    private var cached: ByteArray? = null

    /** The pinned key, or `null` when none is configured (or it is malformed). */
    internal fun pinned(raw: String = BuildConfig.LAKKO_PUSH_WAKE_KEY): ByteArray? {
        val trimmed = raw.trim()
        if (trimmed.isEmpty()) return null
        return runCatching { java.util.Base64.getUrlDecoder().decode(trimmed.trimEnd('=')) }
            .getOrNull()?.takeIf { it.size == PushClient.KEY_LEN }
    }

    /** push-gw's wake key, or `null` when it cannot be had right now. A
     *  failed fetch is not cached, so the next publish tries again. */
    public suspend fun key(fetch: suspend () -> ByteArray = { PushClient().use { it.wakeKey() } }): ByteArray? {
        pinned()?.let { return it }
        cached?.let { return it }
        return try {
            fetch().also { cached = it }
        } catch (cause: kotlinx.coroutines.CancellationException) {
            throw cause
        } catch (_: Throwable) {
            runCatching { android.util.Log.w("lakko-push", "push wake key unavailable; publishing without a wake") }
            null
        }
    }
}
