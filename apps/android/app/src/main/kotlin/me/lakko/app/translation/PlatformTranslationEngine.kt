// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import android.content.Context
import android.icu.util.ULocale
import android.os.Build
import android.os.CancellationSignal
import android.util.Log
import android.view.translation.TranslationCapability
import android.view.translation.TranslationContext
import android.view.translation.TranslationManager
import android.view.translation.TranslationRequest
import android.view.translation.TranslationRequestValue
import android.view.translation.TranslationResponseValue
import android.view.translation.TranslationSpec
import androidx.annotation.RequiresApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.sameLanguage
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import kotlin.coroutines.resume

/**
 * Translation through Android's on-device translation service
 * (`TranslationManager.createOnDeviceTranslator`, API 31), the Android engine
 * of ADR-0082 (amended 2026-10-27: ML Kit is not used, its data disclosure says
 * it reports the configured source and target languages to Google). Only the
 * `OnDevice` factory and the `OnDevice` capability query are called; the
 * deprecated `createTranslator` is never called (the fail-closed source test
 * refuses it). The service belongs to the system (on Pixel devices, Android
 * System Intelligence), so its packs are "Managed by Android".
 */
@RequiresApi(Build.VERSION_CODES.S)
internal class PlatformTranslationEngine(context: Context) : TranslationEngine {
    private val appContext = context.applicationContext
    private val executor: Executor = Executors.newSingleThreadExecutor()

    private fun manager(): TranslationManager? =
        appContext.getSystemService(TranslationManager::class.java)

    private suspend fun capabilities(): Set<TranslationCapability> = withContext(Dispatchers.IO) {
        try {
            manager()?.getOnDeviceTranslationCapabilities(
                TranslationSpec.DATA_FORMAT_TEXT,
                TranslationSpec.DATA_FORMAT_TEXT,
            ) ?: emptySet()
        } catch (_: Throwable) {
            emptySet()
        }
    }

    private fun stateOf(capability: TranslationCapability): TranslationEngineState =
        when (capability.state) {
            TranslationCapability.STATE_ON_DEVICE -> TranslationEngineState.READY
            TranslationCapability.STATE_AVAILABLE_TO_DOWNLOAD,
            TranslationCapability.STATE_DOWNLOADING,
            -> TranslationEngineState.NEEDS_PACK
            else -> TranslationEngineState.UNAVAILABLE
        }

    override suspend fun available(): Boolean =
        capabilities().any { stateOf(it) != TranslationEngineState.UNAVAILABLE }

    /** A locale the service reported, in the tag form the core uses. */
    private fun tagOf(locale: ULocale): String = PlatformLanguageTags.fromPlatform(locale.toLanguageTag())

    /** The service's capabilities for a pair, whatever it calls the languages. */
    private fun matching(all: Set<TranslationCapability>, source: String, target: String): List<TranslationCapability> {
        val s = PlatformLanguageTags.fromPlatform(source)
        val t = PlatformLanguageTags.fromPlatform(target)
        return all.filter { sameLanguage(tagOf(it.sourceSpec.locale), s) && sameLanguage(tagOf(it.targetSpec.locale), t) }
    }

    override suspend fun state(source: String, target: String): TranslationEngineState {
        val match = matching(capabilities(), source, target)
        return when {
            match.any { stateOf(it) == TranslationEngineState.READY } -> TranslationEngineState.READY
            match.any { stateOf(it) == TranslationEngineState.NEEDS_PACK } -> TranslationEngineState.NEEDS_PACK
            else -> TranslationEngineState.UNAVAILABLE
        }
    }

    override suspend fun sources(target: String): List<String> =
        capabilities()
            .filter {
                sameLanguage(tagOf(it.targetSpec.locale), PlatformLanguageTags.fromPlatform(target)) &&
                    stateOf(it) != TranslationEngineState.UNAVAILABLE
            }
            .map { tagOf(it.sourceSpec.locale) }
            .distinct()

    private suspend fun translator(source: String, target: String): android.view.translation.Translator? {
        val m = manager() ?: return null
        // The locales as the service itself names them (`nb` may be `no`,
        // `tl` may be `fil` there), else the tags as given.
        val match = matching(capabilities(), source, target).firstOrNull()
        val sourceLocale = match?.sourceSpec?.locale ?: ULocale.forLanguageTag(source)
        val targetLocale = match?.targetSpec?.locale ?: ULocale.forLanguageTag(target)
        val ctx = TranslationContext.Builder(
            TranslationSpec(sourceLocale, TranslationSpec.DATA_FORMAT_TEXT),
            TranslationSpec(targetLocale, TranslationSpec.DATA_FORMAT_TEXT),
        ).build()
        return withTimeoutOrNull(TIMEOUT_MS) {
            suspendCancellableCoroutine { cont ->
                try {
                    m.createOnDeviceTranslator(ctx, executor) { t -> if (cont.isActive) cont.resume(t) }
                } catch (_: Throwable) {
                    if (cont.isActive) cont.resume(null)
                }
            }
        }
    }

    override suspend fun translate(text: String, source: String, target: String): String? {
        if (state(source, target) != TranslationEngineState.READY) return null
        val t = translator(source, target) ?: return null
        return try {
            val request = TranslationRequest.Builder()
                .setTranslationRequestValues(listOf(TranslationRequestValue.forText(text)))
                .build()
            val signal = CancellationSignal()
            withTimeoutOrNull(TIMEOUT_MS) {
                suspendCancellableCoroutine<String?> { cont ->
                    cont.invokeOnCancellation { signal.cancel() }
                    t.translate(request, signal, executor) { response ->
                        val value = response.translationResponseValues.get(0)
                        val out = if (value != null && value.statusCode == TranslationResponseValue.STATUS_SUCCESS) {
                            value.text?.toString()
                        } else {
                            null
                        }
                        if (cont.isActive) cont.resume(out)
                    }
                }
            }
        } catch (_: Throwable) {
            Log.w(TAG, "translate failed code=exception")
            null
        } finally {
            try {
                t.destroy()
            } catch (_: Throwable) {
                // nothing to release
            }
        }
    }

    /**
     * Creating an on-device translator for a pair that is available to
     * download is how the platform is asked for the pack. Whether every
     * implementation starts a download for it is not documented; the outcome
     * is read back from the capability state, and a pair that does not reach
     * the device in time reads as scheduled. Not run on a device.
     */
    override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome {
        progress(null)
        val t = translator(source, target)
        try {
            t?.destroy()
        } catch (_: Throwable) {
            // nothing to release
        }
        val deadline = System.currentTimeMillis() + DOWNLOAD_WAIT_MS
        while (System.currentTimeMillis() < deadline) {
            when (state(source, target)) {
                TranslationEngineState.READY -> return PackOutcome.DONE
                TranslationEngineState.UNAVAILABLE -> return PackOutcome.FAILED
                TranslationEngineState.NEEDS_PACK -> kotlinx.coroutines.delay(POLL_MS)
            }
        }
        return PackOutcome.SCHEDULED
    }

    override suspend fun installedPacks(): List<PackInfo> =
        capabilities()
            .filter { it.state == TranslationCapability.STATE_ON_DEVICE }
            .map { tagOf(it.sourceSpec.locale) }
            .distinct()
            .map { PackInfo(it, null, forTranslation = true, forTranscription = false, managedBySystem = true) }

    private companion object {
        const val TAG = "LakkoTranslation"
        const val TIMEOUT_MS = 20_000L
        const val DOWNLOAD_WAIT_MS = 120_000L
        const val POLL_MS = 2_000L
    }
}

/**
 * The translation engine for this device: the platform's on-device service
 * when the system has one (API 31+), ML Kit otherwise (ADR-0082 amended
 * 2026-10-27).
 */
internal fun translationEngineFor(context: Context, antiCensorshipArmed: () -> Boolean): TranslationEngine =
    SystemFirstTranslationEngine(
        system = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) PlatformTranslationEngine(context) else null,
        fallback = MlKitTranslationEngine(antiCensorshipArmed),
    )
