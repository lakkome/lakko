// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.transport

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.transport.AntiCensorshipManager
import me.lakko.app.transport.AntiCensorshipState

/**
 * ADR-0026 Phase 4 — the anti-censorship settings surface (the Kotlin twin of
 * the iOS `AntiCensorshipSettingsView`).
 *
 * One switch that arms / disarms the obfuscated tunnel, plus a live status line
 * driven by [AntiCensorshipManager.state]. Flipping the switch persists the
 * preference and re-runs the boot-arm routine (fetch + verify the signed
 * bootstrap config), so the user sees the outcome immediately.
 */
@Composable
public fun AntiCensorshipSettingsScreen(
    manager: AntiCensorshipManager,
    onBack: () -> Unit,
) {
    val state by manager.state.collectAsStateWithLifecycle()
    var enabled by remember { mutableStateOf(manager.isEnabled) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Text("Anti-censorship", style = MaterialTheme.typography.headlineSmall)

        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Route through obfuscated tunnel")
            Switch(
                checked = enabled,
                onCheckedChange = { on ->
                    enabled = on
                    manager.setEnabled(on)
                },
            )
        }

        Text(
            "When on, Lakko fetches a signed network-bootstrap config and routes "
                + "its API calls through a censorship-resistant transport. Your "
                + "messages stay end-to-end encrypted either way; this only hides the "
                + "connection metadata from a network that blocks Lakko.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(modifier = Modifier.height(8.dp))

        StatusRow(state)

        Spacer(modifier = Modifier.weight(1f))

        OutlinedButton(onClick = onBack, modifier = Modifier.fillMaxWidth()) {
            Text("Back")
        }
    }
}

@Composable
private fun StatusRow(state: AntiCensorshipState) {
    val (title, subtitle, color) = when (state) {
        AntiCensorshipState.Disabled ->
            Triple("Off", "Direct connection", MaterialTheme.colorScheme.onSurfaceVariant)
        AntiCensorshipState.Unconfigured ->
            Triple(
                "Not available in this build",
                "No bootstrap key was pinned at build time",
                Color(0xFFB26A00),
            )
        AntiCensorshipState.Arming ->
            Triple("Connecting…", "Fetching bootstrap config", MaterialTheme.colorScheme.onSurfaceVariant)
        AntiCensorshipState.Armed ->
            Triple("Active", "Routing through the obfuscated tunnel", Color(0xFF1B7A3D))
        is AntiCensorshipState.Unavailable ->
            Triple("Unavailable", state.reason, MaterialTheme.colorScheme.error)
    }
    Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
        Text("Status", style = MaterialTheme.typography.labelMedium)
        Text(title, color = color)
        Text(
            subtitle,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}
