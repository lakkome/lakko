// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp

/**
 * ADR-0017 lost-everything fallback — show the freshly-minted 24-word
 * recovery phrase for the user to save, gated behind an acknowledge
 * checkbox, then finish the session.
 *
 * Mirror of the web `SaveFreshPhrasePrompt`. The new identity is already
 * published (without a supersession proof); this screen only displays the
 * phrase + lands the session on confirm.
 */
@Composable
public fun SaveFreshPhraseScreen(
    phrase: String,
    onConfirm: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var acknowledged by remember { mutableStateOf(false) }
    val words = remember(phrase) { phrase.trim().split(Regex("\\s+")).filter { it.isNotEmpty() } }

    Column(
        modifier = modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text(
            text = "Save your new recovery phrase",
            style = MaterialTheme.typography.headlineMedium,
        )
        Text(
            text = "You created a brand-new secure identity. Write these 24 " +
                "words down and keep them safe — they are the only way to " +
                "restore this identity on another device. Your existing " +
                "contacts will be asked to verify you again.",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        // Two-column numbered grid.
        Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            words.chunked(2).forEachIndexed { rowIndex, pair ->
                Row(
                    modifier = Modifier.fillMaxWidth(),
                    horizontalArrangement = Arrangement.spacedBy(8.dp),
                ) {
                    pair.forEachIndexed { col, word ->
                        val n = rowIndex * 2 + col + 1
                        Text(
                            text = "$n. $word",
                            style = MaterialTheme.typography.bodyMedium.copy(
                                fontFamily = FontFamily.Monospace,
                            ),
                            modifier = Modifier.fillMaxWidth(if (col == 0) 0.5f else 1f),
                        )
                    }
                }
            }
        }

        Spacer(Modifier.height(8.dp))
        Row(
            verticalAlignment = Alignment.CenterVertically,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Checkbox(
                checked = acknowledged,
                onCheckedChange = { acknowledged = it },
            )
            Text(
                text = "I have written down my 24-word recovery phrase.",
                style = MaterialTheme.typography.bodyMedium,
            )
        }

        Button(
            onClick = onConfirm,
            enabled = acknowledged,
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Done — take me to Lakko")
        }
    }
}
