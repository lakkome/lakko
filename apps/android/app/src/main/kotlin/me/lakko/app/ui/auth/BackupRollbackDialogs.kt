// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.auth

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import me.lakko.app.backup.RestoreReason
import me.lakko.app.l10n.t
import me.lakko.app.ui.components.LakkoDialog
import me.lakko.app.ui.components.LakkoDialogAction
import me.lakko.app.ui.components.LakkoDialogTone

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
    LakkoDialog(
        // Dismiss-outside / back = Cancel — never a silent accept.
        onDismissRequest = onCancel,
        title = t("backup.stale_restore.title"),
        body = t(
            "backup.stale_restore.body",
            mapOf(
                "reason" to t(reason.staleKey ?: "common.unknown"),
                "consequence" to t("backup.stale_restore.consequence"),
            ),
        ),
        confirm = LakkoDialogAction(t("backup.stale_restore.confirm"), LakkoDialogTone.Danger, onClick = onConfirm),
        cancel = LakkoDialogAction(t("common.cancel"), LakkoDialogTone.Ghost, onClick = onCancel),
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
public fun BackupRollbackRefusedDialog(
    onDismiss: () -> Unit,
) {
    LakkoDialog(
        onDismissRequest = onDismiss,
        // The reason is diagnostic; AuthRepository has logged it. The person
        // reads what happened and that the account is safe.
        title = t("backup.rollback_refused.title"),
        body = t("backup.rollback_refused.body"),
        confirm = LakkoDialogAction(t("common.dismiss"), onClick = onDismiss),
        cancel = null,
    )
}
