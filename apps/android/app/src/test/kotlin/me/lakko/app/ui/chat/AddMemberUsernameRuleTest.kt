// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.ContextWrapper
import android.content.SharedPreferences
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatRepository
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * ADR-0048 §4 — the add-member search field never sends a lookup for a
 * username that fails the rule, and shows the reason instead; a passing name
 * is looked up in its canonical form.
 *
 * Drives the real [ChatViewModel] over the real [ChatRepository] with an
 * in-memory `Context` (no session, no native core — the same harness as
 * `ChatViewModelConstructionTest`). The username rule is a stand-in throwing
 * the generated binding's typed refusals; the resolver counts every lookup.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class AddMemberUsernameRuleTest {

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    private fun fakeValidate(input: String): String {
        val n = input.trim().removePrefix("@").lowercase()
        if (n.isEmpty()) throw FfiException.UsernameEmpty("empty")
        if (!n.all { it in 'a'..'z' || it in '0'..'9' || it == '_' }) {
            throw FfiException.UsernameInvalidCharacters("chars")
        }
        if (n.length < 3) throw FfiException.UsernameTooShort("short")
        return n
    }

    @Test
    fun `an invalid username shows the reason and is never looked up`() {
        val lookups = mutableListOf<String>()
        val repo = ChatRepository(applicationContext = RuleTestContext())
        try {
            val vm = ChatViewModel(
                repo = repo,
                groupIdHex = "00".repeat(16),
                resolveRecipient = { name ->
                    lookups += name
                    awaitCancellation()
                },
                validateUsername = ::fakeValidate,
            )
            vm.setAddMemberInput("pa-vel")
            vm.onAddMember()
            assertEquals(emptyList<String>(), lookups)
            // ADR-0058: the reason is the catalog message, held as its key
            // so a language switch re-renders it.
            assertEquals(
                me.lakko.app.profile.UsernameProblem.INVALID_CHARACTERS.text,
                vm.membershipError.value,
            )

            vm.setAddMemberInput("al")
            vm.onAddMember()
            assertEquals(emptyList<String>(), lookups)
            assertEquals(
                me.lakko.app.profile.UsernameProblem.TOO_SHORT.text,
                vm.membershipError.value,
            )

            vm.setAddMemberInput(" @Alice ")
            vm.onAddMember()
            assertEquals(listOf("alice"), lookups)
        } finally {
            repo.close()
        }
    }
}

/** In-memory `Context`: itself as the application context, in-memory prefs. */
private class RuleTestContext : ContextWrapper(null) {
    private val prefs = HashMap<String, RuleTestPreferences>()

    override fun getApplicationContext(): android.content.Context = this

    override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
        synchronized(prefs) { prefs.getOrPut(name) { RuleTestPreferences() } }
}

private class RuleTestPreferences : SharedPreferences {
    private val values = HashMap<String, Any?>()

    override fun getAll(): MutableMap<String, *> = synchronized(values) { HashMap(values) }
    override fun getString(key: String, defValue: String?): String? =
        synchronized(values) { values[key] as String? ?: defValue }

    @Suppress("UNCHECKED_CAST")
    override fun getStringSet(key: String, defValues: MutableSet<String>?): MutableSet<String>? =
        synchronized(values) { (values[key] as Set<String>?)?.toMutableSet() ?: defValues }
    override fun getInt(key: String, defValue: Int): Int = synchronized(values) { values[key] as Int? ?: defValue }
    override fun getLong(key: String, defValue: Long): Long = synchronized(values) { values[key] as Long? ?: defValue }
    override fun getFloat(key: String, defValue: Float): Float =
        synchronized(values) { values[key] as Float? ?: defValue }
    override fun getBoolean(key: String, defValue: Boolean): Boolean =
        synchronized(values) { values[key] as Boolean? ?: defValue }
    override fun contains(key: String): Boolean = synchronized(values) { key in values }
    override fun edit(): SharedPreferences.Editor = Editor()
    override fun registerOnSharedPreferenceChangeListener(
        listener: SharedPreferences.OnSharedPreferenceChangeListener,
    ) = Unit
    override fun unregisterOnSharedPreferenceChangeListener(
        listener: SharedPreferences.OnSharedPreferenceChangeListener,
    ) = Unit

    private inner class Editor : SharedPreferences.Editor {
        private val staged = HashMap<String, Any?>()
        private val removed = HashSet<String>()
        private var clear = false

        override fun putString(key: String, value: String?) = apply { staged[key] = value }
        override fun putStringSet(key: String, values: MutableSet<String>?) = apply { staged[key] = values?.toSet() }
        override fun putInt(key: String, value: Int) = apply { staged[key] = value }
        override fun putLong(key: String, value: Long) = apply { staged[key] = value }
        override fun putFloat(key: String, value: Float) = apply { staged[key] = value }
        override fun putBoolean(key: String, value: Boolean) = apply { staged[key] = value }
        override fun remove(key: String) = apply { removed += key }
        override fun clear() = apply { clear = true }
        override fun commit(): Boolean {
            synchronized(values) {
                if (clear) values.clear()
                removed.forEach { values.remove(it) }
                values.putAll(staged)
            }
            return true
        }
        override fun apply() {
            commit()
        }
    }
}
