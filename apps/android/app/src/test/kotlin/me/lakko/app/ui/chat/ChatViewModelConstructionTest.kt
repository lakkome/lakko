// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import android.content.ContextWrapper
import android.content.SharedPreferences
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import me.lakko.app.chat.ChatRepository
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import java.util.concurrent.CopyOnWriteArrayList

/**
 * Opening a chat constructs a [ChatViewModel], and the constructor must not
 * crash.
 *
 * ## The defect this pins
 *
 * `viewModelScope` runs on `Dispatchers.Main.immediate`. A ViewModel is built
 * on the main thread, where `Main.immediate` needs no dispatch, so every
 * `viewModelScope.launch` in an `init` block starts executing IN the
 * constructor, and a `StateFlow.collect` delivers its current value
 * synchronously. The collectors therefore run while every property declared
 * textually BELOW the `init` block is still unassigned — a JVM `null` behind a
 * non-null Kotlin type. The first `init` block read `sfuCallUiState` and
 * `mutableActiveSfuCall` (and, once signed in, the membership-retry state) from
 * further down the class: an NPE inside the coroutine, which on Android reaches
 * the thread's uncaught-exception handler and kills the process. Every chat
 * open crashed.
 *
 * ## How the test reproduces production
 *
 * [UnconfinedTestDispatcher] as `Main` is the faithful stand-in: like
 * `Main.immediate` on the main thread it reports no dispatch needed, so the
 * launches run in the constructor exactly as they do on a device. A coroutine
 * exception outside `runTest` goes to the current thread's uncaught-exception
 * handler — the same route that crashes the app — so the test installs one and
 * asserts nothing reached it.
 *
 * The repository is the real [ChatRepository] over an in-memory `Context`; no
 * session is bootstrapped, so nothing here touches the native core. That also
 * means the signed-in-only reads (the retry evaluation) are not reached by this
 * test — the fix does not rely on it, because it moves the collectors after
 * EVERY property rather than after the ones this test happens to hit.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ChatViewModelConstructionTest {

    @Before
    fun setUp() {
        Dispatchers.setMain(UnconfinedTestDispatcher())
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `constructing the view model starts its collectors without touching unassigned properties`() {
        val uncaught = CopyOnWriteArrayList<Throwable>()
        val thread = Thread.currentThread()
        val previousHandler = thread.uncaughtExceptionHandler
        thread.uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { _, e -> uncaught += e }
        val repo = ChatRepository(applicationContext = InMemoryContext())
        try {
            val vm = ChatViewModel(repo = repo, groupIdHex = "00".repeat(16))

            assertEquals(
                "a collector started in the constructor threw: " +
                    uncaught.joinToString { it.toString() },
                emptyList<Throwable>(),
                uncaught.toList(),
            )
            // The collectors are live, not merely absent: the call surfaces the
            // first init block reads are the ones the screen then observes.
            assertNull(vm.activeSfuCall.value)
            assertNull(vm.joinableGroupCall.value)
        } finally {
            thread.uncaughtExceptionHandler = previousHandler
            repo.close()
        }
    }
}

/**
 * The narrowest `Context` the repository's stores need: themselves as the
 * application context, and an in-memory `SharedPreferences` per name.
 * `ContextWrapper`'s constructor is a no-op in the unit-test android.jar; every
 * method not overridden here would throw, so a store reaching for anything else
 * fails the test loudly rather than silently.
 */
internal class InMemoryContext : ContextWrapper(null) {
    private val prefs = HashMap<String, InMemoryPreferences>()

    override fun getApplicationContext(): android.content.Context = this

    override fun getSharedPreferences(name: String, mode: Int): SharedPreferences =
        synchronized(prefs) { prefs.getOrPut(name) { InMemoryPreferences() } }
}

internal class InMemoryPreferences : SharedPreferences {
    private val values = HashMap<String, Any?>()

    override fun getAll(): MutableMap<String, *> = synchronized(values) { HashMap(values) }

    override fun getString(key: String, defValue: String?): String? =
        synchronized(values) { values[key] as String? ?: defValue }

    @Suppress("UNCHECKED_CAST")
    override fun getStringSet(key: String, defValues: MutableSet<String>?): MutableSet<String>? =
        synchronized(values) { (values[key] as Set<String>?)?.toMutableSet() ?: defValues }

    override fun getInt(key: String, defValue: Int): Int =
        synchronized(values) { values[key] as Int? ?: defValue }

    override fun getLong(key: String, defValue: Long): Long =
        synchronized(values) { values[key] as Long? ?: defValue }

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
        override fun putStringSet(key: String, values: MutableSet<String>?) =
            apply { staged[key] = values?.toSet() }
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
