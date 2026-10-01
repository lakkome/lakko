// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import me.lakko.app.l10n.t

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
    /** The account's username, or `null` / blank for a guest account made
     *  from a call link (it has no password, so the copy differs). */
    username: String? = null,
) {
    val guest = username.isNullOrBlank()
    RecoveryPhraseOnboarding(
        phrase = phrase,
        onDone = onConfirm,
        modifier = modifier,
        title = if (guest) t("recovery_phrase.save.title") else t("recovery_phrase.fresh.title"),
        lead = if (guest) {
            t("recovery_phrase.guest.lede")
        } else {
            t("recovery_phrase.fresh.lede", mapOf("username" to "@$username")) +
                "\n\n" + t("recovery_phrase.fresh.safety_note")
        },
    )
}
