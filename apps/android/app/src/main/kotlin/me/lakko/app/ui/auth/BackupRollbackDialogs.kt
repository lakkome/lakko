// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable

/**
 * ADR-0037 §6 (G-11) — the destructive stale-backup override dialog.
 *
 * A restore hit a backup head that is authentically signed but OLDER than
 * expected — indistinguishable from a rollback replay (a returning client's head
 * below its floor, or a fresh install's head past the freshness window). The
 * user MUST explicitly acknowledge the downgrade; there is NO silent accept
 * (invariant #4). Tapping outside / back is treated as Cancel (fail-closed),
 * mirroring the web `defaultStaleConfirm` posture.
 */
@Composable
public fun StaleBackupOverrideDialog(
    reason: String,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    AlertDialog(
        // Dismiss-outside / back = Cancel — never a silent accept.
        onDismissRequest = onCancel,
        title = { Text("Restore an older backup?") },
        text = {
            Text(
                "This backup is older than expected ($reason).\n\n" +
                    "Restoring it will roll your account back to older state — " +
                    "older messages, and devices you removed may reappear.\n\n" +
                    "Restore anyway?",
            )
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text("Restore anyway", color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = {
            TextButton(onClick = onCancel) {
                Text("Cancel")
            }
        },
    )
}

/**
 * ADR-0037 §6 (G-11) — the hard-refusal alarm dialog.
 *
 * A restore was HARD-refused by the rollback gate: the served backup head was
 * unverifiable / tampered / corrupt, or its attest key could not be obtained.
 * No backup was applied; there is no override (invariant #4). Acknowledgement
 * only.
 */
@Composable
public fun BackupRollbackAlertDialog(
    reason: String,
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Backup restore blocked") },
        text = {
            Text(
                "The backup on the server could not be verified and may have " +
                    "been tampered with ($reason). No backup was restored — your " +
                    "account is unchanged. If you did not expect this, verify " +
                    "your safety number with your contacts out of band.",
            )
        },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text("OK")
            }
        },
    )
}
