// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
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
    var nameInput by remember { mutableStateOf(current?.displayName ?: "") }
    var saved by remember { mutableStateOf(current) }
    var confirmation by remember { mutableStateOf<String?>(null) }
    val check = checkDisplayName(nameInput)
    val error = (check as? DisplayNameCheck.Invalid)?.problem?.copy

    Column(
        modifier = Modifier
            .fillMaxSize()
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Text("Profile", style = MaterialTheme.typography.headlineSmall)

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
            text = "Username: " + (username?.let { "@$it" } ?: "none (guest account)"),
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
                }
                confirmation = PROFILE_SAVED_COPY
            },
            modifier = Modifier.fillMaxWidth(),
        ) {
            Text("Save")
        }

        confirmation?.let { Text(it, style = MaterialTheme.typography.bodySmall) }

        OutlinedButton(onClick = onBack, modifier = Modifier.fillMaxWidth()) {
            Text("Back")
        }
    }
}
