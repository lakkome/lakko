// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import me.lakko.app.profile.DisplayNameCheck
import me.lakko.app.profile.OwnProfile
import me.lakko.app.profile.PROFILE_SAVED_COPY
import me.lakko.app.profile.applyOwnNameChange
import me.lakko.app.profile.checkDisplayName
import me.lakko.app.profile.ownProfilePreview

/**
 * ADR-0048 UX §4 — Settings -> Profile.
 *
 * One name field (at most 64 characters, one line), a live preview of how
 * others see this account (`Alice Petrova · @alice`), Save, and the username
 * shown read-only. Saving stamps the profile strictly newer than the last one
 * and hands it to [onSaved], which persists it and republishes it into every
 * group; the confirmation line then reads "Your name will update for your
 * contacts." There is no per-contact name.
 *
 * The name rule is the core's (`normalizeDisplayName`, via [checkDisplayName]),
 * applied as the user types; a name the rule refuses cannot be saved.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
public fun ProfileSettingsScreen(
    /** The stored own profile, or `null` when none exists yet. */
    current: OwnProfile?,
    /** The account's canonical username, or `null` (guest). */
    username: String?,
    /** Persist + republish a changed profile. */
    onSaved: (OwnProfile) -> Unit,
    onBack: () -> Unit,
) {
    // Keyed on [current]: the base a save is computed from is always the
    // stored profile, a name adopted from another device included.
    var saved by remember(current) { mutableStateOf(current) }
    // A name set on another of the account's devices (ADR-0048 section 4a)
    // replaces the field only while the person has not edited it.
    var mirroredName by remember { mutableStateOf(current?.displayName ?: "") }
    var nameInput by remember { mutableStateOf(current?.displayName ?: "") }
    LaunchedEffect(current?.displayName) {
        val incoming = current?.displayName ?: ""
        nameInput = me.lakko.app.profile.followSavedName(nameInput, mirroredName, incoming)
        mirroredName = incoming
    }
    var confirmation by remember { mutableStateOf<String?>(null) }
    val check = checkDisplayName(nameInput)
    val error = (check as? DisplayNameCheck.Invalid)?.problem?.copy

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Profile") },
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
            .imePadding()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        OutlinedTextField(
            value = nameInput,
            onValueChange = {
                nameInput = it
                confirmation = null
            },
            label = { Text("Your name") },
            placeholder = { Text(username ?: "Your name") },
            singleLine = true,
            isError = error != null,
            supportingText = { Text(error ?: "Up to 64 characters, one line.") },
            modifier = Modifier.fillMaxWidth(),
        )

        Text(
            text = "Others see you as",
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = ownProfilePreview(check, username),
            style = MaterialTheme.typography.titleMedium,
        )

        Text(
            text = username?.let { "@$it" } ?: "No username yet",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Button(
            enabled = check is DisplayNameCheck.Valid,
            onClick = {
                val valid = check as? DisplayNameCheck.Valid ?: return@Button
                val updated = applyOwnNameChange(
                    current = saved,
                    newName = valid.normalized,
                    username = username,
                    nowMs = System.currentTimeMillis(),
                )
                if (updated != null) {
                    onSaved(updated)
                    saved = updated
                    mirroredName = updated.displayName ?: ""
                    nameInput = mirroredName
                }
                confirmation = PROFILE_SAVED_COPY
            },
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Save")
        }

        confirmation?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
    }
    }
}
