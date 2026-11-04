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
 *
 *   1. the 12 numbered words on the sunken surface (3 columns x 4 rows on a
 *      phone, 4 x 3 on a wide screen, read across the rows) with a Copy
 *      button and a short explanation;
 *   2. a check that asks for three words at positions chosen at random on
 *      the device;
 *   3. optionally [thirdStep] (sign-up: the offer to add a passkey), which
 *      is handed the function that ends the flow;
 *
 * then [onDone].
 *
 * Used after sign-up (the phrase the account was created with) and after a
 * lost-everything restore (the fresh identity's phrase); the two differ in
 * [title], [lead] and the third step. The confirmation compares locally and
 * sends nothing.
 */
@Composable
public fun RecoveryPhraseOnboarding(
    phrase: String,
    onDone: () -> Unit,
    modifier: Modifier = Modifier,
    title: String = t("recovery_phrase.save.title"),
    lead: String = t("recovery_phrase.save.lede"),
    thirdStep: (@Composable (finish: () -> Unit) -> Unit)? = null,
    /** The way out of the grid step, for a surface that can be left; `null`
     *  has none. */
    onLeave: (() -> Unit)? = null,
    /** The three-word step of a surface whose [onDone] asks a server: it
     *  stays on screen until the answer. `null` when [onDone] just closes. */
    finish: PhraseFinish? = null,
) {
    val words = remember(phrase) { phraseWords(phrase) }
    var confirming by remember { mutableStateOf(false) }
    var third by remember { mutableStateOf(false) }
    val positions = remember(words) { pickConfirmationPositions(words.size.coerceAtLeast(1)) }
    val totalSteps = if (thirdStep != null) 3 else 2

    BackHandler(enabled = confirming && !third) { confirming = false }

    if (third && thirdStep != null) {
        thirdStep(onDone)
        return
    }

    if (!confirming) {
        AuthScaffold(title = title, onBack = onLeave, modifier = modifier) {
            StepLabel(current = 1, total = totalSteps)
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
            onConfirmed = { if (thirdStep != null) third = true else onDone() },
            totalSteps = totalSteps,
            lastStep = thirdStep == null,
            finish = finish,
            modifier = modifier,
        )
    }
}

/** What the three-word step shows while its surface finishes with a server. */
public class PhraseFinish(
    /** The server is being asked. */
    public val busy: Boolean,
    /** The line of a failed attempt; the step stays and its button retries. */
    public val failure: String?,
    /** The button can be pressed (a proof block above may have to be filled). */
    public val enabled: Boolean,
    /** Drawn above the button: the proof block of a proof that ran out. */
    public val above: @Composable () -> Unit = {},
)

/** "Step 2 of 3" above a step's text. */
@Composable
public fun StepLabel(current: Int, total: Int) {
    Text(
        t("recovery_phrase.step", mapOf("current" to current.toString(), "total" to total.toString())),
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(8.dp))
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
    totalSteps: Int,
    lastStep: Boolean,
    finish: PhraseFinish?,
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
        StepLabel(current = 2, total = totalSteps)
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
        finish?.above?.invoke()
        FormError(finish?.failure)
        Spacer(Modifier.height(16.dp))
        BusyButton(
            label = when {
                finish?.failure != null -> t("common.try_again")
                lastStep -> t("common.confirm")
                else -> t("common.continue")
            },
            busy = finish?.busy == true,
            enabled = positions.all { answers[it].orEmpty().isNotBlank() } && finish?.enabled != false,
            onClick = ::submit,
        )
        TextButton(
            onClick = onBack,
            enabled = finish?.busy != true,
            modifier = Modifier.padding(top = 4.dp),
        ) { Text(t("recovery_phrase.check.back")) }
    }
}
