// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app

import android.app.Application
import me.lakko.app.account.AccountStoreGate

/**
 * The application every Robolectric test runs in (`robolectric.properties`).
 *
 * Robolectric builds a new one for each test method, and the JVM is shared by
 * the whole suite. [AccountStoreGate] is process-wide on purpose: a test that
 * signs an account out, or forgets a device, leaves that account sealed, and
 * a later test that writes the same account's stores would be refused,
 * depending on nothing but the order the classes happened to run in. Reset
 * here, a test starts from a new process's state whatever ran before it.
 *
 * (Sealing needs a repository, and a repository needs a Context, so every
 * test that can seal an account is a Robolectric test.)
 */
class LakkoTestApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        AccountStoreGate.resetForTest()
    }
}
