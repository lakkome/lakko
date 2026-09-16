// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import me.lakko.app.recovery.ShamirBridge

/**
 * The guided recovery-phrase step (docs/ux/client-redesign.md section 5.6):
 * the 24 numbered words on the sunken surface with a Copy button and a short
 * explanation, then a confirmation step that asks for three words at
 * positions chosen at random on the device, then [onDone]. The optional
 * social-recovery share generator sits behind "More options".
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
    title: String = "Save your recovery phrase",
    lead: String = "These 24 words are the only way back into your account if you lose " +
        "this phone and your password. Write them down in order and keep them " +
        "somewhere safe. Lakko cannot show them again.",
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
            Lead(
                "Anyone who has these words can read your messages and act as you. " +
                    "Do not share them and do not keep them in a screenshot.",
            )
            Spacer(Modifier.height(16.dp))
            SocialRecoveryOptions(phrase)
            Spacer(Modifier.height(24.dp))
            BusyButton(label = "Continue", busy = false, onClick = { confirming = true })
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
        Text(if (copied) "Copied" else "Copy")
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

    AuthScaffold(title = "Check your notes", onBack = onBack, modifier = modifier) {
        Lead("Type the words at these positions to make sure you wrote the phrase down correctly.")
        Spacer(Modifier.height(16.dp))
        positions.forEachIndexed { i, position ->
            val last = i == positions.lastIndex
            OutlinedTextField(
                value = answers[position].orEmpty(),
                onValueChange = { typed ->
                    answers = answers + (position to typed)
                    if (mismatch.isNotEmpty()) mismatch = emptyList()
                },
                label = { Text("Word $position") },
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
                1 -> "Word ${mismatch[0]} does not match. Check your notes and try again."
                else -> "Words ${mismatch.joinToString(", ")} do not match. Check your notes and try again."
            },
        )
        Spacer(Modifier.height(16.dp))
        BusyButton(
            label = "Confirm",
            busy = false,
            enabled = positions.all { answers[it].orEmpty().isNotBlank() },
            onClick = ::submit,
        )
        TextButton(onClick = onBack, modifier = Modifier.padding(top = 4.dp)) { Text("Show the words again") }
    }
}

/**
 * "More options": the optional split of the phrase into five shares of which
 * any three rebuild it, for people the user trusts. Collapsed by default so
 * writing the words down stays the one path everyone takes.
 */
@Composable
private fun SocialRecoveryOptions(phrase: String) {
    var expanded by remember { mutableStateOf(false) }
    var shares by remember { mutableStateOf<List<String>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    var error by remember { mutableStateOf<String?>(null) }
    var copiedIndex by remember { mutableStateOf<Int?>(null) }
    val clipboard = LocalClipboardManager.current
    val scope = rememberCoroutineScope()

    if (!expanded) {
        TextButton(onClick = { expanded = true }) { Text("More options") }
        return
    }

    Surface(
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text("Share the phrase with people you trust", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            Lead(
                "Split the phrase into five pieces and give one to each of five people. " +
                    "Any three pieces together rebuild it; fewer reveal nothing. You can " +
                    "lose touch with two of them and still recover.",
            )
            Spacer(Modifier.height(12.dp))
            if (shares.isEmpty()) {
                OutlinedButton(
                    onClick = {
                        busy = true
                        error = null
                        scope.launch {
                            try {
                                shares = withContext(Dispatchers.Default) {
                                    ShamirBridge.splitMnemonic(phrase)
                                }
                            } catch (cause: Throwable) {
                                error = "Could not create the pieces. Try again."
                            } finally {
                                busy = false
                            }
                        }
                    },
                    enabled = !busy,
                    modifier = Modifier.fillMaxWidth(),
                ) {
                    if (busy) {
                        CircularProgressIndicator(modifier = Modifier.size(16.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(8.dp))
                        Text("Loading...")
                    } else {
                        Text("Create five pieces")
                    }
                }
            } else {
                shares.forEachIndexed { i, share ->
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(
                            "${i + 1}.",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.width(28.dp),
                        )
                        Text(
                            share,
                            style = MaterialTheme.typography.bodySmall,
                            fontFamily = FontFamily.Monospace,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.weight(1f),
                        )
                        TextButton(
                            onClick = {
                                clipboard.setText(AnnotatedString(share))
                                copiedIndex = i
                                scope.launch {
                                    delay(1500L)
                                    if (copiedIndex == i) copiedIndex = null
                                }
                            },
                        ) { Text(if (copiedIndex == i) "Copied" else "Copy") }
                    }
                }
                Spacer(Modifier.height(4.dp))
                Lead("Give each piece to a different person. To recover, collect any three and choose Recovery pieces on the Recover screen.")
            }
            FormError(error)
        }
    }
}
