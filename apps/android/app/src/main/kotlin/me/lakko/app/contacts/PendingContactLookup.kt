// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * A username the conversation list found, on its way to Contacts.
 *
 * `docs/ux/navigation.md` section 4.3. The one lookup row asks both namespaces
 * and routes what it finds: a channel opens its subscribe page, and a person
 * "hands the name to Contacts, which is where the contact request lives because
 * that flow asks for an introduction this field has no room for".
 *
 * A one-shot slot rather than a navigation argument, for the same reason
 * [me.lakko.app.contacts.InviteIntake] is one: the Contacts destination is a
 * TAB, and giving it an argument would make its route a pattern that the tab
 * bar's own "which tab am I on" comparison no longer matches. The name is
 * published, the Contacts screen opens the add sheet with it filled in, and
 * [consume] empties the slot so a later lookup lands cleanly instead of
 * re-opening the sheet on every visit.
 *
 * Pure Kotlin — no `Context`, no Android — so the publish / consume handshake
 * runs in a host JVM unit test.
 */
public object PendingContactLookup {
    private val slot = MutableStateFlow<String?>(null)

    /** The username waiting to be offered, or `null` when there is none. */
    public val pendingUsername: StateFlow<String?> = slot.asStateFlow()

    /** Hand a found username to Contacts. A blank name is not a name. */
    public fun publish(username: String) {
        val trimmed = username.trim()
        if (trimmed.isEmpty()) return
        slot.value = trimmed
    }

    /** Empty the slot once Contacts has taken the name. */
    public fun consume() {
        slot.value = null
    }
}
