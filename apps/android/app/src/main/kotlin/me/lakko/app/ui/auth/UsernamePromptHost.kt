// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.launch
import me.lakko.app.auth.AuthRepository
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone

/** Test tag of the prompt's field. */
public const val USERNAME_PROMPT_FIELD_TAG: String = "username-prompt-field"

/**
 * "What is your username?" (`auth/UsernamePrompt.kt`): a dialog over the
 * running app after a sign-in that typed no name and whose backup gave none.
 * Never blocks the app: "I don't remember" closes it with a note, and the
 * next start of the app asks again while the name is still unknown.
 */
@Composable
public fun UsernamePromptHost(repository: AuthRepository) {
    val prompt by repository.usernamePrompt.collectAsStateWithLifecycle()
    val state = prompt ?: return
    val scope = rememberCoroutineScope()

    if (state.forgotNote) {
        LakkoDialog(
            onDismissRequest = repository::closeUsernamePromptNote,
            title = null,
            body = t("auth.username_prompt.forgot_note"),
            confirm = LakkoDialogAction(t("common.ok"), onClick = repository::closeUsernamePromptNote),
            cancel = null,
        )
        return
    }

    var typed by rememberSaveable { mutableStateOf("") }
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { runCatching { focus.requestFocus() } }
    val submit: () -> Unit = {
        if (typed.isNotBlank() && !state.busy) {
            scope.launch { repository.submitUsernamePrompt(typed) }
        }
    }
    LakkoDialog(
        // Answered by a button, never by a tap outside.
        onDismissRequest = {},
        title = t("auth.username_prompt.title"),
        body = t("auth.username_prompt.lede"),
        focusCancelOnOpen = false,
        confirm = LakkoDialogAction(
            t("auth.username_prompt.continue"),
            enabled = typed.isNotBlank() && !state.busy,
            onClick = submit,
        ),
        cancel = LakkoDialogAction(
            t("auth.username_prompt.forgot"),
            LakkoDialogTone.Ghost,
            enabled = !state.busy,
        ) { scope.launch { repository.forgotUsernamePrompt() } },
    ) {
        OutlinedTextField(
            value = typed,
            onValueChange = { typed = it },
            label = { Text(t("auth.username_prompt.field")) },
            singleLine = true,
            enabled = !state.busy,
            isError = state.error != null,
            supportingText = state.error?.let { error ->
                {
                    Text(
                        error.text(),
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    )
                }
            },
            keyboardOptions = KeyboardOptions(
                capitalization = KeyboardCapitalization.None,
                autoCorrectEnabled = false,
                keyboardType = KeyboardType.Ascii,
                imeAction = ImeAction.Done,
            ),
            keyboardActions = KeyboardActions(onDone = { submit() }),
            modifier = Modifier
                .fillMaxWidth()
                .testTag(USERNAME_PROMPT_FIELD_TAG)
                .focusRequester(focus),
        )
    }
}
