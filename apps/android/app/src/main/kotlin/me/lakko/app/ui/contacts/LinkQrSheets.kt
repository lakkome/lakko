// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.contacts

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import me.lakko.app.contacts.LakkoLink
import me.lakko.app.contacts.lakkoLinkFromScanned
import me.lakko.app.l10n.t
import me.lakko.app.ui.devices.QrScannerComposable
import me.lakko.app.ui.devices.QrTextImageComposable

/**
 * "Show QR code" on an invite-link row: a Lakko sheet with a large QR of the
 * link's URL, one line saying what scanning it does, and Close. The URL is
 * not shown as text (UX 5.3), and no system share sheet is involved.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun InviteQrSheet(url: String, onDismiss: () -> Unit) {
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(t("invite.links.show_qr"), style = MaterialTheme.typography.titleLarge)
            // White behind the code whatever the theme, so a dark-mode screen
            // still scans.
            Box(
                modifier = Modifier
                    .clip(RoundedCornerShape(16.dp))
                    .background(Color.White)
                    .padding(12.dp),
            ) {
                QrTextImageComposable(text = url, size = 264.dp)
            }
            Text(
                t("invite.links.qr_hint"),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            OutlinedButton(onClick = onDismiss, modifier = Modifier.fillMaxWidth()) {
                Text(t("common.close"))
            }
        }
    }
}

/**
 * "Scan QR code" on the Contacts screen. Asks for the camera when it is not
 * granted yet; a scanned Lakko `/i/`, `/g/` or `/c/` link goes to [onLink]
 * (the same route an opened app link takes); any other code shows a line and
 * the camera keeps scanning.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun ScanLinkSheet(onLink: (LakkoLink) -> Unit, onDismiss: () -> Unit) {
    val context = LocalContext.current
    val sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)
    val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
        PackageManager.PERMISSION_GRANTED
    var cameraAllowed by remember { mutableStateOf(granted) }
    var denied by remember { mutableStateOf(false) }
    var notALink by remember { mutableStateOf(false) }
    var handled by remember { mutableStateOf(false) }
    val launcher = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { ok ->
        cameraAllowed = ok
        denied = !ok
    }
    LaunchedEffect(Unit) {
        if (!granted) launcher.launch(Manifest.permission.CAMERA)
    }
    ModalBottomSheet(onDismissRequest = onDismiss, sheetState = sheetState) {
        Column(
            modifier = Modifier
                .fillMaxWidth()
                .navigationBarsPadding()
                .padding(horizontal = 24.dp)
                .padding(bottom = 24.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(t("contacts.scan_qr"), style = MaterialTheme.typography.titleLarge)
            if (cameraAllowed) {
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .aspectRatio(1f)
                        .clip(RoundedCornerShape(12.dp)),
                ) {
                    QrScannerComposable(
                        onDetected = { raw ->
                            // The analyser fires once per distinct code; a
                            // second link after the first is ignored.
                            if (handled) return@QrScannerComposable
                            val link = lakkoLinkFromScanned(raw)
                            if (link == null) {
                                notALink = true
                            } else {
                                handled = true
                                notALink = false
                                onLink(link)
                            }
                        },
                        onError = { _ ->
                            cameraAllowed = false
                            denied = true
                        },
                    )
                }
                if (notALink) {
                    Text(
                        t("contacts.scan_qr.not_a_link"),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            } else if (denied) {
                Text(
                    t("devices.scan.error.camera_denied"),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.error,
                )
                OutlinedButton(onClick = { launcher.launch(Manifest.permission.CAMERA) }) {
                    Text(t("common.try_again"))
                }
            }
            OutlinedButton(onClick = onDismiss, modifier = Modifier.fillMaxWidth()) {
                Text(t("common.cancel"))
            }
        }
    }
}
