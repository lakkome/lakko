// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t

/**
 * The 24 numbered words in a two-column grid on the sunken surface
 * (docs/ux/client-redesign.md section 5.6). Read-only; the sign-up phrase
 * step and the fresh-identity screen both render through it.
 */
@Composable
public fun RecoveryPhraseGrid(words: List<String>, modifier: Modifier = Modifier) {
    Surface(
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 16.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            phraseGridRows(words).forEach { row ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(12.dp),
                ) {
                    row.forEach { entry ->
                        Row(
                            modifier = Modifier.weight(1f),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Text(
                                text = "${entry.position}.",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                modifier = Modifier.width(28.dp),
                            )
                            Text(
                                text = entry.word,
                                style = MaterialTheme.typography.bodyLarge,
                                fontFamily = FontFamily.Monospace,
                                color = MaterialTheme.colorScheme.onSurface,
                            )
                        }
                    }
                    if (row.size == 1) Row(Modifier.weight(1f)) {}
                }
            }
        }
    }
}

/**
 * The same grid as an input: 24 numbered fields with a Paste button that
 * spreads the clipboard over them. [phrase] is the joined non-empty slots;
 * the grid owns the per-slot text and reports the phrase on every change.
 * When the caller wipes [phrase] to empty (after a successful submit) the
 * slots clear too.
 */
@Composable
public fun RecoveryPhraseEntryGrid(
    phrase: String,
    onPhraseChange: (String) -> Unit,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    onDone: (() -> Unit)? = null,
) {
    var slots by remember { mutableStateOf(List(RECOVERY_PHRASE_WORDS) { "" }) }
    val clipboard = LocalClipboardManager.current

    LaunchedEffect(phrase) {
        if (phrase.isEmpty() && slots.any { it.isNotEmpty() }) {
            slots = List(RECOVERY_PHRASE_WORDS) { "" }
        }
    }

    fun update(next: List<String>) {
        slots = next
        onPhraseChange(phraseFromSlots(next))
    }

    Column(modifier = modifier.fillMaxWidth()) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text(
                t("recovery_phrase.label"),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.weight(1f),
            )
            TextButton(
                enabled = enabled,
                onClick = {
                    val text = clipboard.getText()?.text.orEmpty()
                    if (text.isNotBlank()) update(slotsFromPasted(text))
                },
            ) {
                Icon(Icons.Filled.ContentPaste, contentDescription = null)
                Text("  " + t("common.paste"))
            }
        }
        Surface(
            shape = MaterialTheme.shapes.medium,
            color = MaterialTheme.colorScheme.surfaceVariant,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Column(
                modifier = Modifier.padding(horizontal = 12.dp, vertical = 12.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                val positions = (1..RECOVERY_PHRASE_WORDS).toList()
                phraseGridRows(positions.map { "" }).forEach { row ->
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        row.forEach { entry ->
                            val index = entry.position - 1
                            val last = entry.position == RECOVERY_PHRASE_WORDS
                            OutlinedTextField(
                                value = slots[index],
                                onValueChange = { typed ->
                                    // A multi-word paste into one field spreads out too.
                                    val words = phraseWords(typed)
                                    if (words.size > 1) {
                                        val next = slots.toMutableList()
                                        words.forEachIndexed { k, w ->
                                            if (index + k < RECOVERY_PHRASE_WORDS) next[index + k] = w
                                        }
                                        update(next)
                                    } else {
                                        update(slots.toMutableList().also { it[index] = typed.trim() })
                                    }
                                },
                                enabled = enabled,
                                singleLine = true,
                                prefix = {
                                    Text(
                                        "${entry.position}.",
                                        style = MaterialTheme.typography.bodySmall,
                                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    )
                                },
                                textStyle = MaterialTheme.typography.bodyMedium.copy(
                                    fontFamily = FontFamily.Monospace,
                                ),
                                keyboardOptions = KeyboardOptions(
                                    capitalization = KeyboardCapitalization.None,
                                    autoCorrectEnabled = false,
                                    keyboardType = KeyboardType.Ascii,
                                    imeAction = if (last) ImeAction.Done else ImeAction.Next,
                                ),
                                keyboardActions = androidx.compose.foundation.text.KeyboardActions(
                                    onDone = { onDone?.invoke() },
                                ),
                                colors = OutlinedTextFieldDefaults.colors(
                                    focusedContainerColor = MaterialTheme.colorScheme.surface,
                                    unfocusedContainerColor = MaterialTheme.colorScheme.surface,
                                    disabledContainerColor = MaterialTheme.colorScheme.surface,
                                ),
                                modifier = Modifier.weight(1f),
                            )
                        }
                    }
                }
            }
        }
    }
}
