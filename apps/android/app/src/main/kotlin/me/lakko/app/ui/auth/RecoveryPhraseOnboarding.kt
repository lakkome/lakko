// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import me.lakko.app.l10n.listPositions
import me.lakko.app.l10n.t

/**
 * The guided recovery-phrase step (docs/ux/client-redesign.md section 5.6):
 * the 12 numbered words on the sunken surface with a Copy button and a short
 * explanation, then a confirmation step that asks for three words at
 * positions chosen at random on the device, then [onDone].
 *
 * Used after sign-up (the phrase the account was created with) and after a
 * lost-everything restore (the fresh identity's phrase); the two differ only
 * in [title] and [lead]. The phrase is in memory on this screen only; the
 * confirmation compares locally and sends nothing.
 */
@Composable
public fun RecoveryPhraseOnboarding(
    phrase: String,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
    title: String = t("recovery_phrase.save.title"),
    lead: String = t("recovery_phrase.save.lede"),
) {
    val words = remember(phrase) { phraseWords(phrase) }
    var confirming by remember { mutableStateOf(false) }
    val positions = remember(words) { pickConfirmationPositions(words.size.coerceAtLeast(1)) }

    BackHandler(enabled = confirming) { confirming = false }

    if (!confirming) {
        AuthScaffold(title = title, onBack = null, modifier = modifier) {
            Lead(lead)
            Spacer(Modifier.height(16.dp))
            RecoveryPhraseGrid(words)
            Spacer(Modifier.height(8.dp))
            CopyPhraseButton(phrase)
            Spacer(Modifier.height(8.dp))
            Lead(t("recovery_phrase.save.anyone_with_words"))
            Spacer(Modifier.height(8.dp))
            Lead(t("recovery_phrase.save.no_screenshot"))
            Spacer(Modifier.height(24.dp))
            BusyButton(label = t("common.continue"), busy = false, onClick = { confirming = true })
            Spacer(Modifier.height(16.dp))
        }
    } else {
        ConfirmPhraseStep(
            words = words,
            positions = positions,
            onBack = { confirming = false },
            onConfirmed = onDone,
            modifier = modifier,
        )
    }
}

@Composable
private fun CopyPhraseButton(phrase: String) {
    val clipboard = LocalClipboardManager.current
    val scope = rememberCoroutineScope()
    var copied by remember { mutableStateOf(false) }
    OutlinedButton(
        onClick = {
            clipboard.setText(AnnotatedString(phrase))
            copied = true
            scope.launch {
                delay(1500L)
                copied = false
            }
        },
        modifier = Modifier.fillMaxWidth(),
    ) {
        Icon(Icons.Filled.ContentCopy, contentDescription = null, modifier = Modifier.size(18.dp))
        Spacer(Modifier.width(8.dp))
        Text(if (copied) t("common.copied") else t("common.copy"))
    }
}

/**
 * The confirmation step: three fields for the words at [positions], compared
 * on the device. A mismatch names the positions and offers the grid again.
 */
@Composable
private fun ConfirmPhraseStep(
    words: List<String>,
    positions: List<Int>,
    onBack: () -> Unit,
    onConfirmed: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var answers by remember { mutableStateOf(positions.associateWith { "" }) }
    var mismatch by remember { mutableStateOf<List<Int>>(emptyList()) }

    fun submit() {
        when (val verdict = checkConfirmation(words, positions, answers)) {
            ConfirmationVerdict.Confirmed -> onConfirmed()
            is ConfirmationVerdict.Mismatch -> mismatch = verdict.positions
        }
    }

    AuthScaffold(title = t("recovery_phrase.check.title"), onBack = onBack, modifier = modifier) {
        Lead(t("recovery_phrase.check.lede", mapOf("positions" to listPositions(positions))))
        Spacer(Modifier.height(16.dp))
        positions.forEachIndexed { i, position ->
            val last = i == positions.lastIndex
            OutlinedTextField(
                value = answers[position].orEmpty(),
                onValueChange = { typed ->
                    answers = answers + (position to typed)
                    if (mismatch.isNotEmpty()) mismatch = emptyList()
                },
                label = { Text(t("recovery_phrase.word_label", mapOf("position" to position.toString()))) },
                singleLine = true,
                isError = position in mismatch,
                textStyle = MaterialTheme.typography.bodyLarge.copy(fontFamily = FontFamily.Monospace),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = if (last) ImeAction.Done else ImeAction.Next,
                ),
                keyboardActions = androidx.compose.foundation.text.KeyboardActions(onDone = { submit() }),
                modifier = Modifier.fillMaxWidth(),
            )
            Spacer(Modifier.height(12.dp))
        }
        FormError(
            when (mismatch.size) {
                0 -> null
                1 -> t("recovery_phrase.check.mismatch_one", mapOf("position" to mismatch[0].toString()))
                else -> t("recovery_phrase.check.mismatch_many", mapOf("positions" to listPositions(mismatch)))
            },
        )
        Spacer(Modifier.height(16.dp))
        BusyButton(
            label = t("common.confirm"),
            busy = false,
            enabled = positions.all { answers[it].orEmpty().isNotBlank() },
            onClick = ::submit,
        )
        TextButton(onClick = onBack, modifier = Modifier.padding(top = 4.dp)) { Text(t("recovery_phrase.check.back")) }
    }
}
