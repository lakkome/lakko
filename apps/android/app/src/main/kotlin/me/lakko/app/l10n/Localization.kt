// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import android.app.LocaleManager
import android.content.Context
import android.content.SharedPreferences
import android.content.res.Resources
import android.os.Build
import android.os.LocaleList
import android.util.Log
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.unit.LayoutDirection
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import org.json.JSONObject
import uniffi.lakko_ffi_uniffi.L10nBundle
import uniffi.lakko_ffi_uniffi.L10nCatalog
import uniffi.lakko_ffi_uniffi.L10nSource
import uniffi.lakko_ffi_uniffi.TextDirection
import uniffi.lakko_ffi_uniffi.l10nResolveLanguage
import uniffi.lakko_ffi_uniffi.l10nSupportedLanguages

private const val TAG = "LakkoL10n"
private const val ASSET_DIR = "l10n"

/**
 * The interface strings of one language. Plain messages are rendered once per
 * switch ([plain]); messages with arguments go through the `lakko-l10n`
 * formatter on each call. A key is never missing at runtime: the core falls
 * back to English, and an unknown key renders as itself so a typo is visible.
 */
public class Strings internal constructor(
    public val language: String,
    public val rtl: Boolean,
    private val plain: Map<String, String>,
    private val catalog: L10nCatalog?,
) {
    internal val hasCatalog: Boolean get() = catalog != null

    public operator fun get(key: String): String = plain[key] ?: key

    public fun format(
        key: String,
        texts: Map<String, String> = emptyMap(),
        counts: Map<String, ULong> = emptyMap(),
    ): String {
        val rendered = try {
            catalog?.format(key, texts, counts)
        } catch (e: Exception) {
            Log.w(TAG, "WARN format failed for $key: ${e.javaClass.simpleName}")
            null
        }
        if (rendered != null) return rendered
        // Without the core (it failed to load, or a host JVM test): render the
        // English text with the English plural rule so the screen still reads.
        val text = plain[key] ?: return key
        return try {
            fallbackRender(text, texts, counts, null)
        } catch (e: RuntimeException) {
            Log.w(TAG, "WARN fallback format failed for $key: ${e.javaClass.simpleName}")
            text
        }
    }
}

/**
 * The English-only rendering of the catalog's message syntax (arguments and
 * `plural` with `=N`, `one`, `other`, `#`), used only when the core cannot
 * format. Quoting is not handled; a message that needs it renders raw.
 */
private fun fallbackRender(
    text: String,
    texts: Map<String, String>,
    counts: Map<String, ULong>,
    hash: ULong?,
): String {
    val out = StringBuilder()
    var i = 0
    while (i < text.length) {
        val c = text[i]
        when {
            c == '#' && hash != null -> {
                out.append(hash.toString())
                i++
            }
            c == '{' -> {
                val end = matchingBrace(text, i)
                val inner = text.substring(i + 1, end)
                val comma = inner.indexOf(',')
                if (comma < 0) {
                    val name = inner.trim()
                    out.append(texts[name] ?: counts[name]?.toString() ?: "{$name}")
                } else {
                    val name = inner.substring(0, comma).trim()
                    val rest = inner.substring(comma + 1)
                    val second = rest.indexOf(',')
                    val value = counts[name] ?: 0uL
                    val branches = if (second < 0) "" else rest.substring(second + 1)
                    out.append(fallbackRender(pluralBranch(branches, value), texts, counts, value))
                }
                i = end + 1
            }
            else -> {
                out.append(c)
                i++
            }
        }
    }
    return out.toString()
}

private fun matchingBrace(text: String, open: Int): Int {
    var depth = 0
    for (j in open until text.length) {
        when (text[j]) {
            '{' -> depth++
            '}' -> {
                depth--
                if (depth == 0) return j
            }
        }
    }
    throw IllegalArgumentException("unbalanced braces")
}

private fun pluralBranch(branches: String, value: ULong): String {
    val found = HashMap<String, String>()
    var i = 0
    while (i < branches.length) {
        val open = branches.indexOf('{', i)
        if (open < 0) break
        val selector = branches.substring(i, open).trim()
        val end = matchingBrace(branches, open)
        found[selector] = branches.substring(open + 1, end)
        i = end + 1
    }
    return found["=$value"] ?: (if (value == 1uL) found["one"] else null) ?: found["other"].orEmpty()
}

/** The strings the current composition renders; see [ProvideLocalization]. */
public val LocalStrings: androidx.compose.runtime.ProvidableCompositionLocal<Strings> =
    staticCompositionLocalOf { Localization.strings.value }

/** A plain interface string in the current language. */
@Composable
public fun t(key: String): String = LocalStrings.current[key]

/** A message with arguments in the current language. */
@Composable
public fun t(key: String, texts: Map<String, String>, counts: Map<String, ULong> = emptyMap()): String =
    LocalStrings.current.format(key, texts, counts)

/**
 * Provide the current [Strings] and the language's layout direction to
 * [content]. Recomposes the whole tree on a switch, with no Activity
 * recreation (MainActivity declares `locale|layoutDirection` config changes).
 */
@Composable
public fun ProvideLocalization(content: @Composable () -> Unit) {
    val strings by Localization.strings.collectAsStateWithLifecycle()
    CompositionLocalProvider(
        LocalStrings provides strings,
        LocalLayoutDirection provides if (strings.rtl) LayoutDirection.Rtl else LayoutDirection.Ltr,
        content = content,
    )
}

/**
 * Process-wide holder of the interface language (ADR-0061 section 4).
 *
 * The choice is kept in plain `SharedPreferences` on every API level and,
 * on Android 13+, also as the system per-app language, which is then the
 * authority (a change in system Settings is picked up). It is never sent to
 * a server.
 */
public object Localization {
    private const val PREFS = "lakko-language"
    private const val KEY = "selected"

    private var controller: LanguageController? = null
    private var source: L10nSource? = null
    private var sourceTexts: Map<String, String> = emptyMap()
    private var appContext: Context? = null

    /** ADR-0061 S6: the accepted signed translation bundle, if any. */
    @Volatile private var bundle: AcceptedBundle<L10nBundle>? = null
    @Volatile private var storage: TranslationBundleStorage? = null
    private val bundleSignedAtFlow = MutableStateFlow<Long?>(null)

    /** When the active translation update was signed (Settings > About), `null` when none. */
    public val translationsUpdatedAt: StateFlow<Long?> = bundleSignedAtFlow.asStateFlow()

    private val stateFlow = MutableStateFlow(LanguageState(null, "en", "en"))
    private val stringsFlow = MutableStateFlow(Strings("en", false, emptyMap(), null))

    public val state: StateFlow<LanguageState> = stateFlow.asStateFlow()
    public val strings: StateFlow<Strings> = stringsFlow.asStateFlow()

    /** The supported languages, in table order; the list screen sorts them. */
    public var languages: List<LanguageEntry> = emptyList()
        private set

    /** Load once per process. Safe to call repeatedly. */
    public fun init(context: Context) {
        if (controller != null) return
        val ctx = context.applicationContext
        appContext = ctx
        sourceTexts = readSourceTexts(ctx)
        languages = try {
            l10nSupportedLanguages().map { LanguageEntry(it.code, it.ownName, it.direction == TextDirection.RTL) }
        } catch (e: Throwable) {
            Log.w(TAG, "WARN supported languages unavailable: ${e.javaClass.simpleName}")
            listOf(LanguageEntry("en", "English", false))
        }
        source = try {
            readAsset(ctx, "source.json")?.let { L10nSource(it) }
        } catch (e: Throwable) {
            Log.w(TAG, "WARN source catalog failed to load: ${e.javaClass.simpleName}")
            null
        }
        loadStoredBundle(FileTranslationBundleStorage(java.io.File(ctx.noBackupFilesDir, "l10n")))
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val c = LanguageController(
            store = PrefsChoiceStore(prefs, KEY),
            platform = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                LocaleManagerAppLanguage(ctx)
            } else {
                null
            },
            systemPreferred = { systemLocaleTags(ctx) },
            resolve = ::resolveSafely,
            matchTag = ::matchTag,
        )
        controller = c
        apply(c.state.value)
    }

    /** Pick a language (`null` = same as system); the UI switches at once. */
    public fun select(code: String?) {
        controller?.select(code)?.let(::apply)
    }

    /** Configuration changed: the system language or the per-app one may have. */
    public fun refresh() {
        controller?.refresh()?.let(::apply)
    }

    /** The built-in catalog version a bundle must exceed; `null` when the core did not load. */
    internal fun builtInCatalogVersion(): ULong? = try {
        source?.version()
    } catch (e: Throwable) {
        null
    }

    /** The version of the bundle in effect (the rollback floor for a download). */
    internal fun activeBundleVersion(): ULong? = bundle?.version

    internal fun bundleStorage(): TranslationBundleStorage? = storage

    /** Switch the live strings to an accepted bundle (call on the main thread). */
    internal fun installBundle(accepted: AcceptedBundle<L10nBundle>) {
        bundle = accepted
        bundleSignedAtFlow.value = accepted.signedAtMillis
        stringsFlow.value = buildStrings(stateFlow.value.resolved)
    }

    /**
     * At launch: re-verify the stored bundle with no rollback floor and keep
     * it only if the core still accepts it at the recorded version; delete it
     * otherwise (an app update raised the built-in version or the release).
     * A file read and a signature check, no network.
     */
    private fun loadStoredBundle(store: TranslationBundleStorage) {
        storage = store
        val key = TranslationPin.verifyingKey ?: return
        val version = builtInCatalogVersion() ?: return
        try {
            when (val outcome = decideOnLaunch(store.read(), coreBundleVerifier(key, version))) {
                is LaunchOutcome.Use -> {
                    bundle = outcome.accepted
                    bundleSignedAtFlow.value = outcome.accepted.signedAtMillis
                }
                LaunchOutcome.Discard -> {
                    Log.i(TAG, "stored translation bundle no longer applies; deleted")
                    store.delete()
                }
                LaunchOutcome.None -> Unit
            }
        } catch (e: Throwable) {
            Log.w(TAG, "WARN stored translation bundle unreadable: ${e.javaClass.simpleName}")
        }
    }

    /**
     * Tests only: serve [strings] as the current language, with no core loaded
     * (a host JVM has no native library). Unit tests that read rendered text
     * install the English source this way.
     */
    @androidx.annotation.VisibleForTesting
    internal fun installForTests(strings: Strings) {
        stringsFlow.value = strings
    }

    /** The own name of [code] ("Deutsch"), or the code when unknown. */
    public fun ownName(code: String): String = languages.firstOrNull { it.code == code }?.ownName ?: code

    private fun apply(next: LanguageState) {
        stateFlow.value = next
        val current = stringsFlow.value
        if (next.resolved != current.language || !current.hasCatalog) {
            stringsFlow.value = buildStrings(next.resolved)
        }
    }

    private fun buildStrings(code: String): Strings {
        val ctx = appContext
        val src = source
        if (ctx != null && src != null) {
            val builtIn = if (code == "en") null else readAsset(ctx, "$code.json")
            // With the accepted bundle first; if that fails, the built-in
            // catalog alone (a bundle must never cost the user their language).
            val bundles: List<L10nBundle?> = listOf(bundle?.bundle, null).distinct()
            for (b in bundles) {
                try {
                    val catalog = L10nCatalog(src, code, builtIn, b)
                    return Strings(
                        language = catalog.language(),
                        rtl = catalog.direction() == TextDirection.RTL,
                        plain = catalog.plainStrings(),
                        catalog = catalog,
                    )
                } catch (e: Throwable) {
                    Log.w(TAG, "WARN catalog for $code failed to load: ${e.javaClass.simpleName}")
                }
            }
        }
        return Strings("en", false, sourceTexts, null)
    }

    private fun resolveSafely(selected: String?, system: List<String>): String = try {
        l10nResolveLanguage(selected, system)
    } catch (e: Throwable) {
        Log.w(TAG, "WARN resolve failed: ${e.javaClass.simpleName}")
        "en"
    }

    /** A platform tag to a Lakko code; the core falls back to English, which is a miss unless the tag is English. */
    private fun matchTag(tag: String): String? {
        val code = resolveSafely(null, listOf(tag))
        return if (code == "en" && !tag.lowercase().startsWith("en")) null else code
    }

    private fun systemLocaleTags(ctx: Context): List<String> {
        val list: LocaleList = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            ctx.getSystemService(LocaleManager::class.java)?.systemLocales
                ?: Resources.getSystem().configuration.locales
        } else {
            Resources.getSystem().configuration.locales
        }
        return (0 until list.size()).map { list[it].toLanguageTag() }
    }

    private fun readAsset(ctx: Context, name: String): ByteArray? = try {
        ctx.assets.open("$ASSET_DIR/$name").use { it.readBytes() }
    } catch (e: java.io.IOException) {
        null
    }

    /** English texts straight from the source JSON, for when the core cannot load. */
    private fun readSourceTexts(ctx: Context): Map<String, String> = try {
        val json = readAsset(ctx, "source.json")?.decodeToString() ?: return emptyMap()
        val messages = JSONObject(json).getJSONObject("messages")
        messages.keys().asSequence().associateWith { messages.getJSONObject(it).getString("text") }
    } catch (e: Exception) {
        emptyMap()
    }
}

private class PrefsChoiceStore(
    private val prefs: SharedPreferences,
    private val key: String,
) : LanguageChoiceStore {
    override fun read(): String? = prefs.getString(key, null)
    override fun write(code: String?) {
        prefs.edit().apply { if (code == null) remove(key) else putString(key, code) }.apply()
    }
}

@androidx.annotation.RequiresApi(Build.VERSION_CODES.TIRAMISU)
private class LocaleManagerAppLanguage(context: Context) : PlatformAppLanguage {
    private val manager: LocaleManager? = context.getSystemService(LocaleManager::class.java)

    override fun read(): String? {
        val list = manager?.applicationLocales ?: return null
        return if (list.isEmpty) null else list[0].toLanguageTag()
    }

    override fun write(code: String?) {
        manager?.applicationLocales = if (code == null) LocaleList.getEmptyLocaleList() else LocaleList.forLanguageTags(code)
    }
}
