// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier

/**
 * ADR-0017 lost-everything fallback: the freshly created identity's 24-word
 * phrase, shown once through the same guided phrase step as sign-up
 * ([RecoveryPhraseOnboarding], grid, copy, three-word confirmation), then
 * [onConfirm] lands the session. The new identity is already published; this
 * screen only makes sure the phrase is saved.
 */
@Composable
public fun SaveFreshPhraseScreen(
    phrase: String,
    onConfirm: () -> Unit,
    modifier: Modifier = Modifier,
) {
    RecoveryPhraseOnboarding(
        phrase = phrase,
        onDone = onConfirm,
        modifier = modifier,
        title = "Your new recovery phrase",
        lead = "Your account has a new identity. These 24 words are the only way to " +
            "restore it on another phone, so write them down in order and keep them " +
            "somewhere safe. People you already talk to will be asked to verify you again.",
    )
}
