// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.updates

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.request.get
import io.ktor.client.statement.readBytes
import io.ktor.http.isSuccess
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.transport.AntiCensorshipEngine
import uniffi.lakko_ffi_uniffi.releaseUpdateStatus
import uniffi.lakko_ffi_uniffi.verifyReleaseManifest

/**
 * ADR-0038 S4 — the "update available" notice controller (Android). The Kotlin
 * twin of the iOS `ReleaseUpdateChecker`.
 *
 * Mirror of the structure `AntiCensorshipManager` / `DeviceAttestKey` establish: a
 * process-scoped observable that, on launch (throttled to at most once per 24h),
 * fetches a small signed release manifest, verifies its Ed25519 signature against
 * the build-time-pinned publisher key, computes the update status against this
 * build's release integer, and publishes an [UpdateBannerDecision] the
 * `UpdateAvailableBanner` renders.
 *
 * **Fail-safe-toward-silence (ADR-0038 §3, UX §2).** This is an availability /
 * anti-phishing control, NOT an E2EE gate — its "fail" is *show no nag*, distinct
 * from invariant #4's fail-closed-refuse. So:
 *   * an unpinned build (no `LAKKO_RELEASE_KEY`) is fully dormant — no fetch;
 *   * ANY fetch / verify failure resolves to "up to date" (no banner), so a spoofed
 *     or garbage manifest can never drive an update nag;
 *   * the transport is not the security boundary — the fetch rides the
 *     anti-censorship tunnel when armed ([AntiCensorshipEngine], so a censored
 *     network still reaches the manifest) exactly like every other native REST
 *     call; the Ed25519 signature is the trust anchor either way.
 *
 * Persisted state is a version integer + a timestamp — no PII, no key material, so
 * plain `SharedPreferences` (invariant #1 not engaged).
 */
public class ReleaseUpdateChecker(
    appContext: Context,
    private val scope: CoroutineScope,
) {
    private val prefs =
        appContext.getSharedPreferences("lakko.release", Context.MODE_PRIVATE)

    // ADR-0026 — route the manifest fetch through the obfuscated tunnel when armed
    // (the ambient config is read per-request; disarmed = direct OkHttp).
    private val client = HttpClient(AntiCensorshipEngine(OkHttp.create {}))

    private val _decision = MutableStateFlow<UpdateBannerDecision>(UpdateBannerDecision.Hidden)

    /** The resolved banner visibility — recomputed after each check and on dismiss. */
    public val decision: StateFlow<UpdateBannerDecision> = _decision.asStateFlow()

    private var dismissedThisSession = false

    /**
     * The `latest_release` the current decision was computed against — the key a
     * dismiss is stored under.
     */
    private var offeredLatest: ULong = 0uL

    /**
     * Fetch + verify (throttled) then publish the banner decision. Called once at
     * app launch; safe to call repeatedly (idempotent, cheap when not due). Never
     * throws — it launches its own coroutine on [scope] and swallows every failure
     * (fail-safe-toward-silence).
     */
    public fun checkOnLaunch() {
        // Dormant unless the publisher key is pinned into this build — default
        // empty means the feature is off (fail-safe-toward-silence). No fetch.
        val key = ReleasePin.verifyingKey
        if (key == null) {
            _decision.value = UpdateBannerDecision.Hidden
            return
        }
        val url = ReleasePin.manifestUrl
        val nowMillis = System.currentTimeMillis()
        scope.launch {
            if (isCheckDue(nowMillis)) {
                fetchAndVerify(url, key, nowMillis)
            }
            recompute(nowMillis)
        }
    }

    /**
     * The user dismissed the banner: gone for the rest of this run, and the
     * dismissed target + timestamp persist so a later cold launch re-nags per the
     * [updateBannerDecision] cadence (24h, or immediately on a newer release).
     */
    public fun dismiss() {
        dismissedThisSession = true
        prefs.edit()
            .putString(KEY_DISMISSED_RELEASE, offeredLatest.toString())
            .putLong(KEY_DISMISSED_AT, System.currentTimeMillis())
            .apply()
        _decision.value = UpdateBannerDecision.Hidden
    }

    /**
     * Open the configured Play deep-link. No-op when no store URL is set (the button
     * is hidden in that case, so this is defensive). Launched from the banner's
     * Activity [Context].
     */
    public fun openStore(context: Context) {
        val raw = ReleasePin.playStoreUrl ?: return
        try {
            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(raw)))
        } catch (e: ActivityNotFoundException) {
            Log.w(TAG, "no activity to handle the store URL", e)
        }
    }

    // ---- internals ----

    private fun isCheckDue(nowMillis: Long): Boolean {
        if (!prefs.contains(KEY_LAST_CHECK_AT)) return true
        val last = prefs.getLong(KEY_LAST_CHECK_AT, 0L)
        return nowMillis - last >= CHECK_THROTTLE_MILLIS
    }

    private suspend fun fetchAndVerify(url: String, key: ByteArray, nowMillis: Long) {
        try {
            val response = withContext(Dispatchers.IO) { client.get(url) }
            if (!response.status.isSuccess()) {
                // Not a signed verdict — do NOT bump the throttle (retry next launch)
                // and do NOT touch the cache. Resolve to up-to-date.
                Log.i(TAG, "manifest fetch HTTP ${response.status.value} - treating as up to date")
                return
            }
            val bytes = response.readBytes()
            if (bytes.isEmpty() || bytes.size > MAX_MANIFEST_BYTES) {
                // A body far from the fixed 89 bytes is a hostile / misconfigured
                // origin — fail toward up-to-date rather than hand it to the parser.
                Log.i(TAG, "manifest body ${bytes.size} bytes out of range - treating as up to date")
                return
            }
            // Verify signature + freshness in the audited core. Freshness DISABLED
            // per ADR-0038 (Long.MAX_VALUE) — a stale manifest can only *suppress* a
            // notice, never forge one (the LOW replay residual). ANY failure throws.
            val verified = withContext(Dispatchers.Default) {
                verifyReleaseManifest(bytes, key, nowMillis, Long.MAX_VALUE)
            }
            // Cache the signature-verified numbers + record the successful check, so a
            // relaunch inside the 24h window drives the banner from cache, no re-fetch.
            prefs.edit()
                .putString(KEY_CACHED_LATEST, verified.latestRelease.toString())
                .putString(KEY_CACHED_MIN, verified.minSupportedRelease.toString())
                .putLong(KEY_LAST_CHECK_AT, nowMillis)
                .apply()
        } catch (c: CancellationException) {
            // Respect coroutine cancellation (structured concurrency) — never
            // swallow it into the fail-safe path.
            throw c
        } catch (t: Throwable) {
            // Fail-safe: ANY failure resolves to up-to-date. Keep the last
            // signature-verified cache (an authentic prior verdict is not a spoof)
            // and do NOT bump the throttle, so a transient outage retries next launch
            // rather than pinning silence for 24h.
            Log.w(TAG, "manifest fetch/verify failed - treating as up to date", t)
        }
    }

    private fun recompute(nowMillis: Long) {
        val latest = prefs.getString(KEY_CACHED_LATEST, null)?.toULongOrNull()
        val minSupported = prefs.getString(KEY_CACHED_MIN, null)?.toULongOrNull()
        if (latest == null || minSupported == null) {
            // Never verified a manifest → nothing authenticated to show.
            offeredLatest = 0uL
            _decision.value = UpdateBannerDecision.Hidden
            return
        }
        val status = releaseUpdateStatus(ReleasePin.currentRelease, latest, minSupported)
        offeredLatest = latest
        val inputs = UpdateBannerInputs(
            status = status,
            latestRelease = latest,
            storeUrlPresent = ReleasePin.playStoreUrl != null,
            devOverride = ReleasePin.devOverride,
            dismissedThisSession = dismissedThisSession,
            dismissedRelease = prefs.getString(KEY_DISMISSED_RELEASE, null)?.toULongOrNull(),
            dismissedAtMillis = if (prefs.contains(KEY_DISMISSED_AT)) {
                prefs.getLong(KEY_DISMISSED_AT, 0L)
            } else {
                null
            },
            nowMillis = nowMillis,
            reNagAfterMillis = RE_NAG_AFTER_MILLIS,
        )
        _decision.value = updateBannerDecision(inputs)
    }

    private companion object {
        private const val TAG = "ReleaseUpdate"

        /** Fetch throttle + cold-launch re-nag window (24h in millis). */
        private const val CHECK_THROTTLE_MILLIS = 24L * 60 * 60 * 1000
        private const val RE_NAG_AFTER_MILLIS = 24L * 60 * 60 * 1000

        /**
         * A defensive upper bound on the fetched manifest size. The manifest is a
         * fixed 89 bytes; a body far larger than that is a hostile / misconfigured
         * origin. `verifyReleaseManifest` would reject any non-89-byte input anyway;
         * this just caps the work before that.
         */
        private const val MAX_MANIFEST_BYTES = 4096

        // Persisted, non-secret UI state (a version integer + a timestamp).
        private const val KEY_LAST_CHECK_AT = "lastCheckAtMillis"
        private const val KEY_CACHED_LATEST = "cachedLatestRelease"
        private const val KEY_CACHED_MIN = "cachedMinSupportedRelease"
        private const val KEY_DISMISSED_RELEASE = "dismissedRelease"
        private const val KEY_DISMISSED_AT = "dismissedAtMillis"
    }
}
