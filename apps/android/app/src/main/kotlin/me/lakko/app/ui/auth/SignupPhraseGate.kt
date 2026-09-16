// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Holds the recovery phrase a sign-up just produced until the user has
 * saved and confirmed it. Sign-up lands the session the moment the account
 * exists, which replaces the signed-out screens with the app; without this
 * gate the phrase step would never be shown. The app root renders the guided
 * phrase screen instead of the session while a phrase is parked here.
 *
 * In memory only, never persisted: the phrase is shown exactly once.
 */
public object SignupPhraseGate {
    private val parked = MutableStateFlow<String?>(null)

    /** The phrase awaiting confirmation, or `null`. */
    public val phrase: StateFlow<String?> = parked.asStateFlow()

    public fun park(phrase: String) {
        parked.value = phrase
    }

    /** The user confirmed the words (or the session ended): drop the phrase. */
    public fun clear() {
        parked.value = null
    }
}
