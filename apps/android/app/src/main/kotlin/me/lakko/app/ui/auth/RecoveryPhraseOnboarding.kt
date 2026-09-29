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
import me.lakko.app.l10n.listPositions
import me.lakko.app.l10n.t
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
            Spacer(Modifier.height(16.dp))
            SocialRecoveryOptions(phrase)
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
    var error by remember { mutableStateOf<me.lakko.app.l10n.UiText?>(null) }
    var copiedIndex by remember { mutableStateOf<Int?>(null) }
    val clipboard = LocalClipboardManager.current
    val scope = rememberCoroutineScope()

    if (!expanded) {
        TextButton(onClick = { expanded = true }) { Text(t("common.more_options")) }
        return
    }

    Surface(
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = Modifier.fillMaxWidth(),
    ) {
        Column(modifier = Modifier.padding(16.dp)) {
            Text(t("recovery.method.shares.title"), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(8.dp))
            Lead(
                t(
                    "recovery.shares.panel_body",
                    mapOf("threshold" to SHARE_THRESHOLD.toString()),
                    mapOf("count" to SHARE_COUNT.toULong()),
                ),
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
                                error = me.lakko.app.l10n.UiText("recovery.shares.error.generic")
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
                        Text(t("recovery.shares.generating"))
                    } else {
                        Text(t("recovery.shares.generate", emptyMap(), mapOf("count" to SHARE_COUNT.toULong())))
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
                        ) { Text(if (copiedIndex == i) t("common.copied") else t("common.copy")) }
                    }
                }
                Spacer(Modifier.height(4.dp))
                Lead(
                    t(
                        "recovery.shares.usage",
                        mapOf(
                            "method" to t("recovery.method.shares.title"),
                            "threshold" to SHARE_THRESHOLD.toString(),
                        ),
                    ),
                )
            }
            FormError(error)
        }
    }
}

/** How many shares the split makes, and how many rebuild the phrase
 *  (`ShamirBridge.splitMnemonic`'s defaults, stated for the copy). */
private val SHARE_COUNT: Int = ShamirBridge.DEFAULT_SHARE_COUNT.toInt()
private val SHARE_THRESHOLD: Int = ShamirBridge.DEFAULT_THRESHOLD.toInt()
