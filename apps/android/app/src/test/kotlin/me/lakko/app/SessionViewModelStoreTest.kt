// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotSame
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The session's ViewModels live in [SessionViewModelStore], so the end of a
 * session clears them. Under the Activity's own store they outlived the
 * session (navigation keeps a back-stack entry's models until the entry is
 * popped, and signing out pops nothing) and went on collecting repositories
 * that by then held the next account.
 */
class SessionViewModelStoreTest {

    private class Recording : ViewModel() {
        var cleared = false
        override fun onCleared() {
            cleared = true
        }
    }

    private val factory = object : ViewModelProvider.Factory {
        @Suppress("UNCHECKED_CAST")
        override fun <T : ViewModel> create(modelClass: Class<T>): T = Recording() as T
    }

    private fun SessionViewModelStore.model(key: String): Recording =
        ViewModelProvider(store, factory)[key, Recording::class.java]

    @Test
    fun `the end of a session clears its view models`() {
        val holder = SessionViewModelStore()
        val chat = holder.model("chat-aa")
        val contacts = holder.model("contacts")
        assertFalse(chat.cleared)

        holder.endSession()

        assertTrue(chat.cleared)
        assertTrue(contacts.cleared)
    }

    @Test
    fun `the next session starts with none of the previous session's models`() {
        val holder = SessionViewModelStore()
        val first = holder.model("chat-aa")
        holder.endSession()

        // The same chat id under the next account is a new model.
        val second = holder.model("chat-aa")
        assertNotSame(first, second)
        assertFalse(second.cleared)
    }

    @Test
    fun `within one session a model is kept`() {
        val holder = SessionViewModelStore()
        assertSame(holder.model("chat-aa"), holder.model("chat-aa"))
    }
}
