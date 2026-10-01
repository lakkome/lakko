// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Host-side tests for the Android language holder (ADR-0061). The real
 * resolution rule is `l10nResolveLanguage` in `lakko-l10n`, which needs the
 * native library the JVM suite does not have; here it is a fake that records
 * WHAT it was asked, so these tests pin the order of the inputs the client
 * hands it and where the choice is kept, not the matching rule itself
 * (that is tested in Rust).
 */
class LanguageLogicTest {

    private class MemoryStore(var value: String? = null) : LanguageChoiceStore {
        var writes = 0
        override fun read(): String? = value
        override fun write(code: String?) { value = code; writes++ }
    }

    private class FakePlatform(var tag: String? = null) : PlatformAppLanguage {
        override fun read(): String? = tag
        override fun write(code: String?) { tag = code }
    }

    private val supported = setOf("en", "ar", "ru", "pt-BR", "zh-Hans")

    /** A stand-in for the core rule: selected, else first supported system tag, else English. */
    private val fakeResolve: (String?, List<String>) -> String = { selected, system ->
        selected?.takeIf { it in supported } ?: system.firstOrNull { it in supported } ?: "en"
    }

    private fun controller(
        store: LanguageChoiceStore,
        platform: PlatformAppLanguage? = null,
        system: () -> List<String> = { listOf("de", "ru") },
    ) = LanguageController(
        store = store,
        platform = platform,
        systemPreferred = system,
        resolve = fakeResolve,
        matchTag = { tag -> tag.takeIf { it in supported } },
    )

    @Test
    fun noChoiceFollowsTheSystem() {
        val state = controller(MemoryStore()).state.value
        assertNull(state.selected)
        assertEquals("ru", state.resolved)
        assertEquals("ru", state.systemResolved)
    }

    @Test
    fun storedChoiceWinsOverTheSystem() {
        val state = controller(MemoryStore("ar")).state.value
        assertEquals("ar", state.selected)
        assertEquals("ar", state.resolved)
        assertEquals("ru", state.systemResolved)
    }

    @Test
    fun noSupportedSystemLanguageFallsBackToEnglish() {
        val state = controller(MemoryStore(), system = { listOf("de", "fr") }).state.value
        assertEquals("en", state.resolved)
    }

    @Test
    fun selectPersistsAndSwitches() {
        val store = MemoryStore()
        val c = controller(store)
        c.select("zh-Hans")
        assertEquals("zh-Hans", store.value)
        assertEquals("zh-Hans", c.state.value.resolved)
        c.select(null)
        assertNull(store.value)
        assertEquals("ru", c.state.value.resolved)
    }

    @Test
    fun choiceSurvivesANewController() {
        val store = MemoryStore()
        controller(store).select("pt-BR")
        assertEquals("pt-BR", controller(store).state.value.resolved)
    }

    @Test
    fun followingTheSystemPicksUpASystemChange() {
        var system = listOf("ru")
        val c = controller(MemoryStore(), system = { system })
        assertEquals("ru", c.state.value.resolved)
        system = listOf("ar")
        assertEquals("ar", c.refresh().resolved)
    }

    @Test
    fun selectMirrorsToThePlatformPerAppLanguage() {
        val platform = FakePlatform()
        val c = controller(MemoryStore(), platform)
        c.select("ar")
        assertEquals("ar", platform.tag)
        c.select(null)
        assertNull(platform.tag)
    }

    @Test
    fun aChangeInSystemSettingsWinsAndIsCopiedLocally() {
        val store = MemoryStore("ar")
        val platform = FakePlatform("ru")
        val c = controller(store, platform)
        assertEquals("ru", c.state.value.selected)
        assertEquals("ru", store.value)
        platform.tag = null
        assertNull(c.refresh().selected)
        assertNull(store.value)
    }

    @Test
    fun anUnsupportedPlatformTagReadsAsFollowingTheSystem() {
        val c = controller(MemoryStore(), FakePlatform("de"))
        assertNull(c.state.value.selected)
        assertEquals("ru", c.state.value.resolved)
    }

    @Test
    fun nameKeyIsLowercased() {
        assertEquals("language.name.pt-br", languageNameKey("pt-BR"))
        assertEquals("language.name.zh-hans", languageNameKey("zh-Hans"))
    }

    @Test
    fun searchIsCaseAndDiacriticInsensitive() {
        assertEquals("portugues (brasil)", searchFold("  Português (Brasil) "))
        assertEquals("turkce", searchFold("TÜRKÇE"))
    }

    private val entries = listOf(
        LanguageEntry("ru", "Русский", false),
        LanguageEntry("es", "Español", false),
        LanguageEntry("ar", "العربية", true),
    )
    private val english = mapOf("ru" to "Russian", "es" to "Spanish", "ar" to "Arabic")

    @Test
    fun listIsSortedByTranslatedName() {
        val rows = visibleLanguages(entries, { english.getValue(it.code) }, "en", "")
        assertEquals(listOf("ar", "ru", "es"), rows.map { it.code })
    }

    @Test
    fun searchMatchesOwnOrTranslatedName() {
        assertEquals(listOf("es"), visibleLanguages(entries, { english.getValue(it.code) }, "en", "espanol").map { it.code })
        assertEquals(listOf("ru"), visibleLanguages(entries, { english.getValue(it.code) }, "en", "russ").map { it.code })
        assertEquals(emptyList<String>(), visibleLanguages(entries, { english.getValue(it.code) }, "en", "xyz").map { it.code })
    }
}
