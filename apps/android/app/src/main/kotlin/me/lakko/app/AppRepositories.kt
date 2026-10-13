// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import android.content.Context
import me.lakko.app.auth.AuthRepository
import me.lakko.app.chat.ChatRepository
import me.lakko.app.contacts.ContactsRepository
import me.lakko.app.devices.sync.DeviceGroupSyncRepository

/**
 * The repositories of the process: built once, handed to every `MainActivity`
 * the process creates.
 *
 * They used to be built in `MainActivity.onCreate`, and the Activity is
 * recreated on every configuration change it does not declare (rotation, dark
 * mode, font scale, a resized window). Each recreation built a second set
 * while the first went on living: nothing cleared it, its scopes kept running,
 * and the session's ViewModels, which survive a configuration change, kept
 * calling it. The new set was then bootstrapped for the same account by the
 * new composition. From there one device ran TWO chat repositories for one
 * account, each with its own copy of the MLS provider restored from the same
 * snapshot, each polling, sending and persisting: two ratchets for one leaf,
 * and a snapshot that is whichever of them wrote last. A sign-out cleared only
 * the newest set; the older ones kept the account's identity and its seeds in
 * memory for the rest of the process.
 *
 * All four take the application context alone, so nothing here holds an
 * Activity.
 */
internal class AppRepositories private constructor(applicationContext: Context) {
    val auth: AuthRepository = AuthRepository(applicationContext)

    // ChatRepository before ContactsRepository: the contacts side bridges to
    // chat (a Welcome from the mailbox joins a group).
    val chat: ChatRepository = ChatRepository(applicationContext)

    val contacts: ContactsRepository = ContactsRepository(
        authRepo = auth,
        // G-4 (ADR-0016) — the app context backs the persistent PeerPinStore
        // so the fetch-path supersession gate is active on every real send.
        appContext = applicationContext,
        chatRepo = chat,
    )

    /** The cross-device sync runtime. Bootstrapped from the session root once
     *  the identity secrets land; cleared on logout / forget device there. */
    val deviceGroupSync: DeviceGroupSyncRepository = DeviceGroupSyncRepository(applicationContext)

    companion object {
        @Volatile
        private var instance: AppRepositories? = null

        fun get(context: Context): AppRepositories =
            instance ?: synchronized(this) {
                instance ?: AppRepositories(context.applicationContext).also { instance = it }
            }
    }
}
