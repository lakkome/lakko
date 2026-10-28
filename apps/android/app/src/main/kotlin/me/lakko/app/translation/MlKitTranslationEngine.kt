// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.translation

import android.util.Log
import com.google.mlkit.common.model.DownloadConditions
import com.google.mlkit.common.model.RemoteModelManager
import com.google.mlkit.nl.translate.TranslateLanguage
import com.google.mlkit.nl.translate.TranslateRemoteModel
import com.google.mlkit.nl.translate.Translation
import com.google.mlkit.nl.translate.TranslatorOptions
import kotlinx.coroutines.tasks.await
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.lakko_ffi_uniffi.TranslationEngineState
import uniffi.lakko_ffi_uniffi.canonicalLanguageTag
import java.util.concurrent.ConcurrentHashMap

/**
 * Translation through ML Kit Translate, the fallback of ADR-0082 (amended
 * 2026-10-27) for a device whose system has no on-device translation service.
 *
 * The text is translated on the device. What ML Kit sends Google, by its data
 * disclosure, is the configured source and target languages, for diagnostics
 * and usage analytics; the maintainer accepted that for this fallback only.
 * While anti-censorship is on the engine answers nothing at all, so no ML Kit
 * traffic leaves the device outside the protected connection.
 *
 * Its models are downloaded by ML Kit for this app, so they are Lakko's to
 * remove ([PackInfo.managedBySystem] false).
 */
internal class MlKitTranslationEngine(
    private val antiCensorshipArmed: () -> Boolean,
) : TranslationEngine {
    /**
     * Pairs whose models ML Kit confirmed present this session. ML Kit does
     * not say whether a model it keeps by default is listed among the
     * downloaded ones, so a pair it has just confirmed counts as ready even
     * if one side is missing from that list.
     */
    private val confirmed = ConcurrentHashMap.newKeySet<Pair<String, String>>()

    /** ML Kit's code for a tag, or `null` when it has no model for it. */
    private fun code(tag: String): String? {
        val canonical = canonicalLanguageTag(tag) ?: return null
        // ML Kit's Chinese is Simplified; it has no Traditional model.
        if (canonical.endsWith("-Hant")) return null
        return TranslateLanguage.fromLanguageTag(canonical.substringBefore('-'))
    }

    private suspend fun downloaded(): Set<String> =
        try {
            withTimeoutOrNull(TIMEOUT_MS) {
                RemoteModelManager.getInstance()
                    .getDownloadedModels(TranslateRemoteModel::class.java)
                    .await()
                    .map { it.language }
                    .toSet()
            } ?: emptySet()
        } catch (_: Throwable) {
            emptySet()
        }

    override suspend fun available(): Boolean = !antiCensorshipArmed()

    override suspend fun state(source: String, target: String): TranslationEngineState {
        if (antiCensorshipArmed()) return TranslationEngineState.UNAVAILABLE
        val s = code(source) ?: return TranslationEngineState.UNAVAILABLE
        val t = code(target) ?: return TranslationEngineState.UNAVAILABLE
        if (s == t) return TranslationEngineState.UNAVAILABLE
        if (s to t in confirmed) return TranslationEngineState.READY
        val have = downloaded()
        return if (s in have && t in have) TranslationEngineState.READY else TranslationEngineState.NEEDS_PACK
    }

    override suspend fun sources(target: String): List<String> {
        if (antiCensorshipArmed()) return emptyList()
        val t = code(target) ?: return emptyList()
        return TranslateLanguage.getAllLanguages().filter { it != t }
    }

    override suspend fun translate(text: String, source: String, target: String): String? {
        if (state(source, target) != TranslationEngineState.READY) return null
        val s = code(source) ?: return null
        val t = code(target) ?: return null
        val translator = Translation.getClient(
            TranslatorOptions.Builder().setSourceLanguage(s).setTargetLanguage(t).build(),
        )
        return try {
            withTimeoutOrNull(TIMEOUT_MS) { translator.translate(text).await() }
        } catch (_: Throwable) {
            Log.w(TAG, "translate failed code=exception")
            null
        } finally {
            translator.close()
        }
    }

    /**
     * Fetches the models of the pair. The person's Wi-Fi choice is enforced
     * before this is called (UX 3.5), so no condition is set here. ML Kit
     * reports no progress.
     */
    override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome {
        if (antiCensorshipArmed()) return PackOutcome.FAILED
        val s = code(source) ?: return PackOutcome.FAILED
        val t = code(target) ?: return PackOutcome.FAILED
        progress(null)
        val translator = Translation.getClient(
            TranslatorOptions.Builder().setSourceLanguage(s).setTargetLanguage(t).build(),
        )
        return try {
            val done = withTimeoutOrNull(DOWNLOAD_WAIT_MS) {
                translator.downloadModelIfNeeded(DownloadConditions.Builder().build()).await()
                true
            }
            if (done == true) {
                confirmed.add(s to t)
                PackOutcome.DONE
            } else {
                PackOutcome.SCHEDULED
            }
        } catch (_: Throwable) {
            Log.w(TAG, "download failed code=exception")
            PackOutcome.FAILED
        } finally {
            translator.close()
        }
    }

    override suspend fun installedPacks(): List<PackInfo> =
        downloaded().map {
            PackInfo(it, null, forTranslation = true, forTranscription = false, managedBySystem = false)
        }

    override suspend fun removePack(language: String): Boolean {
        val c = code(language) ?: return false
        return try {
            withTimeoutOrNull(TIMEOUT_MS) {
                RemoteModelManager.getInstance()
                    .deleteDownloadedModel(TranslateRemoteModel.Builder(c).build())
                    .await()
                true
            } == true
        } catch (_: Throwable) {
            false
        }.also { removed -> if (removed) confirmed.removeAll { it.first == c || it.second == c } }
    }

    private companion object {
        const val TAG = "LakkoTranslation"
        const val TIMEOUT_MS = 20_000L
        const val DOWNLOAD_WAIT_MS = 120_000L
    }
}

/**
 * The system's on-device service when this device has one, ML Kit otherwise
 * (ADR-0082 amended 2026-10-27, option "fallback only").
 *
 * The choice is per device, not per language pair: a device whose system
 * service exists but lacks a pair does not fall through to ML Kit for it.
 * Packs of both are listed, so ML Kit models left from before the system
 * service appeared can still be removed.
 */
internal class SystemFirstTranslationEngine(
    private val system: TranslationEngine?,
    private val fallback: TranslationEngine,
) : TranslationEngine {
    private suspend fun active(): TranslationEngine =
        if (system != null && system.available()) system else fallback

    override suspend fun available(): Boolean = active().available()

    override suspend fun state(source: String, target: String): TranslationEngineState =
        active().state(source, target)

    override suspend fun sources(target: String): List<String> = active().sources(target)

    override suspend fun translate(text: String, source: String, target: String): String? =
        active().translate(text, source, target)

    override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome =
        active().download(source, target, progress)

    override suspend fun packSize(source: String, target: String): Long? = active().packSize(source, target)

    override suspend fun installedPacks(): List<PackInfo> =
        (system?.installedPacks().orEmpty()) + fallback.installedPacks()

    override suspend fun removePack(language: String): Boolean = fallback.removePack(language)
}
