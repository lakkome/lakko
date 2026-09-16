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
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
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
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun AntiCensorshipSettingsScreen(
    manager: AntiCensorshipManager,
    onBack: () -> Unit,
) {
    val state by manager.state.collectAsStateWithLifecycle()
    var enabled by remember { mutableStateOf(manager.isEnabled) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Anti-censorship") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { padding ->
    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(padding)
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text("Hide Lakko traffic from network blocking")
            Switch(
                checked = enabled,
                onCheckedChange = { on ->
                    enabled = on
                    manager.setEnabled(on)
                },
            )
        }

        Text(
            "When on, Lakko connects through a disguised route that a network "
                + "blocking Lakko cannot recognise. Your messages stay end-to-end "
                + "encrypted either way.",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(modifier = Modifier.height(8.dp))

        StatusRow(state)
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
                "This build cannot use the disguised route",
                me.lakko.app.ui.theme.LakkoLight.warning,
            )
        AntiCensorshipState.Arming ->
            Triple("Connecting...", "Checking the route", MaterialTheme.colorScheme.onSurfaceVariant)
        AntiCensorshipState.Armed ->
            Triple("Active", "Connected through the disguised route", me.lakko.app.ui.theme.LakkoLight.success)
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
