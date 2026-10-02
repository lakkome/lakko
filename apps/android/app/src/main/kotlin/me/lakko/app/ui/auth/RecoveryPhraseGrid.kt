// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import me.lakko.app.l10n.t

/** The grid's column count for the current window (3 on a phone, 4 from
 *  [PHRASE_GRID_WIDE_MIN_DP] up). */
@Composable
private fun phraseGridColumns(): Int =
    phraseGridShape(LocalConfiguration.current.screenWidthDp).columns

/**
 * The 12 numbered words on the sunken surface, 3 columns x 4 rows on a phone
 * and 4 x 3 on a wide screen, read across the rows ([phraseGridRows]).
 * Read-only; the sign-up phrase step, the fresh-identity screen and "Show
 * recovery phrase" in Settings all render through it.
 */
@Composable
public fun RecoveryPhraseGrid(words: List<String>, modifier: Modifier = Modifier) {
    val columns = phraseGridColumns()
    Surface(
        shape = MaterialTheme.shapes.medium,
        color = MaterialTheme.colorScheme.surfaceVariant,
        modifier = modifier.fillMaxWidth(),
    ) {
        Column(
            modifier = Modifier.padding(horizontal = 10.dp, vertical = 12.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            phraseGridRows(words, columns).forEach { row ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
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
                                modifier = Modifier.width(22.dp),
                            )
                            Text(
                                text = entry.word,
                                style = MaterialTheme.typography.bodyMedium,
                                fontFamily = FontFamily.Monospace,
                                color = MaterialTheme.colorScheme.onSurface,
                                maxLines = 1,
                            )
                        }
                    }
                    // Keep a short last row aligned to the columns above it.
                    repeat(columns - row.size) { Row(Modifier.weight(1f)) {} }
                }
            }
        }
    }
}

/**
 * The same grid as an input: 12 numbered fields in the same shape and order,
 * with a Paste button that spreads the clipboard over them. [phrase] is the
 * joined non-empty slots; the grid owns the per-slot text and reports the
 * phrase on every change. When the caller wipes [phrase] to empty (after a
 * successful submit) the slots clear too.
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
    val columns = phraseGridColumns()

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
                modifier = Modifier.padding(horizontal = 8.dp, vertical = 10.dp),
                verticalArrangement = Arrangement.spacedBy(6.dp),
            ) {
                phraseGridRows(slots, columns).forEach { row ->
                    Row(
                        modifier = Modifier.fillMaxWidth(),
                        horizontalArrangement = Arrangement.spacedBy(6.dp),
                    ) {
                        row.forEach { entry ->
                            val index = entry.position - 1
                            PhraseSlotField(
                                position = entry.position,
                                value = slots[index],
                                enabled = enabled,
                                last = entry.position == RECOVERY_PHRASE_WORDS,
                                onDone = onDone,
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
                                modifier = Modifier.weight(1f),
                            )
                        }
                        repeat(columns - row.size) { Row(Modifier.weight(1f)) {} }
                    }
                }
            }
        }
    }
}

/**
 * One numbered word field of the entry grid. A plain text field on the
 * surface colour rather than an outlined Material field: three of those do
 * not fit side by side at phone width with an eight-letter word in them.
 */
@Composable
private fun PhraseSlotField(
    position: Int,
    value: String,
    enabled: Boolean,
    last: Boolean,
    onDone: (() -> Unit)?,
    onValueChange: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val label = t("recovery_phrase.word_label", mapOf("position" to position.toString()))
    Surface(
        shape = MaterialTheme.shapes.small,
        color = MaterialTheme.colorScheme.surface,
        modifier = modifier,
    ) {
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.padding(horizontal = 6.dp, vertical = 12.dp),
        ) {
            Text(
                "$position.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.width(22.dp),
            )
            BasicTextField(
                value = value,
                onValueChange = onValueChange,
                enabled = enabled,
                singleLine = true,
                textStyle = MaterialTheme.typography.bodyMedium.copy(
                    fontFamily = FontFamily.Monospace,
                    // A filled field that is not a word of the list is a
                    // typo: marked, as the web grid does.
                    color = if (phraseSlotInvalid(value)) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurface
                    },
                ),
                cursorBrush = SolidColor(MaterialTheme.colorScheme.primary),
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.None,
                    autoCorrectEnabled = false,
                    keyboardType = KeyboardType.Ascii,
                    imeAction = if (last) ImeAction.Done else ImeAction.Next,
                ),
                keyboardActions = KeyboardActions(onDone = { onDone?.invoke() }),
                modifier = Modifier
                    .weight(1f)
                    .semantics { contentDescription = label },
            )
        }
    }
}

/** Whether an entry-grid field is marked invalid: it holds something that is
 *  not a word of the BIP39 English list. An empty field is not invalid. */
public fun phraseSlotInvalid(value: String): Boolean =
    value.isNotBlank() && !me.lakko.app.auth.Bip39Words.contains(value)
