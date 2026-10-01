// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import android.content.Context
import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.engine.okhttp.OkHttp
import io.ktor.client.request.get
import io.ktor.client.statement.readBytes
import io.ktor.http.isSuccess
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.BuildConfig
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.transport.AntiCensorshipEngine
import me.lakko.app.updates.ReleasePin
import uniffi.lakko_ffi_uniffi.L10nBundle
import uniffi.lakko_ffi_uniffi.l10nAcceptBundle

/**
 * ADR-0061 S6 — the build-time pin for signed translation updates, the
 * `ReleasePin` pattern. [verifyingKey] `null` (unset, blank, malformed, not 32
 * bytes) makes the feature dormant: no fetch, no stored bundle is used.
 */
public object TranslationPin {
    public const val DEFAULT_URL: String = "https://lakko.me/.well-known/lakko-translations"

    public val verifyingKey: ByteArray?
        get() {
            val raw = BuildConfig.LAKKO_TRANSLATION_KEY.trim()
            if (raw.isEmpty()) return null
            return try {
                base64UrlDecodeNoPad(raw).takeIf { it.size == 32 }
            } catch (_: Throwable) {
                null
            }
        }

    public val bundleUrl: String
        get() = BuildConfig.LAKKO_TRANSLATION_BUNDLE_URL.trim().ifEmpty { DEFAULT_URL }

    /** The release integer bundles are checked against (`min_release`), the release notice's. */
    public val currentRelease: ULong get() = ReleasePin.currentRelease

    /** The ADR-0061 size bound, checked before the body reaches the parser (the core checks again). */
    public const val MAX_BUNDLE_BYTES: Int = 4 * 1024 * 1024
}

/** The core's acceptance check over the pinned key, as a [BundleVerifier]. */
internal fun coreBundleVerifier(key: ByteArray, builtInVersion: ULong): BundleVerifier<L10nBundle> =
    BundleVerifier { bytes, lastAccepted ->
        try {
            val b = l10nAcceptBundle(bytes, key, TranslationPin.currentRelease, builtInVersion, lastAccepted)
            AcceptedBundle(b, b.version(), b.signedAtMillis())
        } catch (e: Exception) {
            Log.i(TAG, "translation bundle refused: ${e.javaClass.simpleName}")
            null
        }
    }

/**
 * Fetches the signed translation bundle on the release notice's cadence
 * (launch, at most once per 24 h) through the anti-censorship tunnel when
 * armed, and hands an accepted one to [Localization], which switches the live
 * strings at once. The request is the same for every device: no language,
 * no account, no header beyond what OkHttp sends.
 *
 * Any failure (dormant pin, network, non-2xx, refusal, rollback, a write
 * error) keeps the strings in effect and logs only.
 */
public class TranslationUpdater(
    appContext: Context,
    private val scope: CoroutineScope,
) {
    private val prefs = appContext.getSharedPreferences("lakko.translations", Context.MODE_PRIVATE)
    private val client = HttpClient(AntiCensorshipEngine(OkHttp.create {}))

    public fun checkOnLaunch() {
        val key = TranslationPin.verifyingKey ?: return
        val builtIn = Localization.builtInCatalogVersion() ?: return
        val now = System.currentTimeMillis()
        val last = if (prefs.contains(KEY_LAST_CHECK_AT)) prefs.getLong(KEY_LAST_CHECK_AT, 0L) else null
        if (!isTranslationFetchDue(last, now, CHECK_THROTTLE_MILLIS)) return
        val url = TranslationPin.bundleUrl
        scope.launch {
            try {
                val response = withContext(Dispatchers.IO) { client.get(url) }
                if (!response.status.isSuccess()) {
                    // 404 = nothing published. Not a verdict: retry next launch.
                    Log.i(TAG, "translation bundle fetch HTTP ${response.status.value}")
                    return@launch
                }
                val bytes = withContext(Dispatchers.IO) { response.readBytes() }
                // A served bundle was judged (accepted or not): that is the
                // day's check. In the steady state the served bundle is the
                // one already accepted and the core refuses it as not newer.
                prefs.edit().putLong(KEY_LAST_CHECK_AT, now).apply()
                val outcome = withContext(Dispatchers.Default) {
                    decideOnDownload(
                        bytes,
                        Localization.activeBundleVersion(),
                        TranslationPin.MAX_BUNDLE_BYTES,
                        coreBundleVerifier(key, builtIn),
                    )
                }
                if (outcome is DownloadOutcome.Apply) {
                    val accepted = outcome.accepted
                    val stored = StoredBundle(bytes, StoredBundleMeta(accepted.version, accepted.signedAtMillis))
                    withContext(Dispatchers.IO) { Localization.bundleStorage()?.write(stored) }
                    withContext(Dispatchers.Main) { Localization.installBundle(accepted) }
                    Log.i(TAG, "translation bundle ${accepted.version} applied")
                }
            } catch (c: CancellationException) {
                throw c
            } catch (t: Throwable) {
                Log.w(TAG, "translation bundle update failed: ${t.javaClass.simpleName}")
            }
        }
    }

    private companion object {
        const val CHECK_THROTTLE_MILLIS = 24L * 60 * 60 * 1000
        const val KEY_LAST_CHECK_AT = "lastCheckAtMillis"
    }
}

private const val TAG = "LakkoL10n"
