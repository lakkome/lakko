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
 * Translation through ML Kit Translate (ADR-0082 notes of 2026-10-27 and
 * 2026-11-15): for a device whose system has no on-device translation
 * service, and for every pair whose pack the system does not already have.
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
        val canonical = canonicalLanguageTag(PlatformLanguageTags.fromPlatform(tag)) ?: return null
        // ML Kit's Chinese is Simplified; it has no Traditional model.
        if (canonical.endsWith("-Hant")) return null
        // The core's `nb` is ML Kit's `no` (PlatformLanguageTags).
        return TranslateLanguage.fromLanguageTag(PlatformLanguageTags.toMlKit(canonical))
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
        return TranslateLanguage.getAllLanguages().filter { it != t }.map { PlatformLanguageTags.fromPlatform(it) }
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
            PackInfo(PlatformLanguageTags.fromPlatform(it), null, forTranslation = true, forTranscription = false, managedBySystem = false)
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
 * The system's on-device service where it has the pack, ML Kit for every
 * other pair (ADR-0082 notes of 2026-10-27 and 2026-11-15).
 *
 * The choice is per language pair. The system is used when it answers READY
 * for the pair. When it answers NEEDS_PACK, Lakko has no documented way to
 * make it download the pack (creating a translator does not: seen on a
 * Pixel 9, API 35, the pack never came), so the pair goes to ML Kit: its
 * state, its pack download and its translation. When the system answers
 * UNAVAILABLE, ML Kit answers for the pair (Ukrainian on a system without
 * it). A pair ML Kit cannot take, which includes every pair while
 * anti-censorship is on, is UNAVAILABLE unless the system has it on the
 * device. Packs of both are listed; only ML Kit's are Lakko's to remove.
 */
internal class SystemFirstTranslationEngine(
    private val system: TranslationEngine?,
    private val fallback: TranslationEngine,
) : TranslationEngine {
    /** The engine that handles a pair and its state for it. */
    private suspend fun route(source: String, target: String): Pair<TranslationEngine, TranslationEngineState> {
        if (system != null && system.state(source, target) == TranslationEngineState.READY) {
            return system to TranslationEngineState.READY
        }
        return fallback to fallback.state(source, target)
    }

    override suspend fun available(): Boolean =
        (system != null && system.available()) || fallback.available()

    override suspend fun state(source: String, target: String): TranslationEngineState = route(source, target).second

    /**
     * The languages ML Kit translates from, and those the system has on the
     * device for [target] (a system language it would only have after a
     * download Lakko cannot start is not listed unless ML Kit has it).
     */
    override suspend fun sources(target: String): List<String> {
        val out = fallback.sources(target).toMutableList()
        for (language in system?.sources(target).orEmpty()) {
            if (language in out) continue
            if (system?.state(language, target) == TranslationEngineState.READY) out += language
        }
        return out
    }

    override suspend fun translate(text: String, source: String, target: String): String? =
        route(source, target).first.translate(text, source, target)

    override suspend fun download(source: String, target: String, progress: (Int?) -> Unit): PackOutcome {
        val (engine, state) = route(source, target)
        return when (state) {
            TranslationEngineState.READY -> PackOutcome.DONE
            TranslationEngineState.UNAVAILABLE -> PackOutcome.FAILED
            TranslationEngineState.NEEDS_PACK -> engine.download(source, target, progress)
        }
    }

    override suspend fun packSize(source: String, target: String): Long? =
        route(source, target).first.packSize(source, target)

    override suspend fun installedPacks(): List<PackInfo> =
        (system?.installedPacks().orEmpty()) + fallback.installedPacks()

    override suspend fun removePack(language: String): Boolean = fallback.removePack(language)
}
