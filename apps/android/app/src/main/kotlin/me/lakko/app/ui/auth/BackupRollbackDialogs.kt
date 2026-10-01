// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import me.lakko.app.backup.RestoreReason
import me.lakko.app.l10n.t

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
    reason: RestoreReason,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    AlertDialog(
        // Dismiss-outside / back = Cancel — never a silent accept.
        onDismissRequest = onCancel,
        title = { Text(t("backup.stale_restore.title")) },
        text = {
            Text(
                t(
                    "backup.stale_restore.body",
                    mapOf(
                        "reason" to t(reason.staleKey ?: "common.unknown"),
                        "consequence" to t("backup.stale_restore.consequence"),
                    ),
                ),
            )
        },
        confirmButton = {
            TextButton(onClick = onConfirm) {
                Text(t("backup.stale_restore.confirm"), color = MaterialTheme.colorScheme.error)
            }
        },
        dismissButton = {
            TextButton(onClick = onCancel) {
                Text(t("common.cancel"))
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
    onDismiss: () -> Unit,
) {
    AlertDialog(
        onDismissRequest = onDismiss,
        // The reason is diagnostic; AuthRepository has logged it. The person
        // reads what happened and that the account is safe.
        title = { Text(t("backup.rollback_refused.title")) },
        text = { Text(t("backup.rollback_refused.body")) },
        confirmButton = {
            TextButton(onClick = onDismiss) {
                Text(t("common.dismiss"))
            }
        },
    )
}
