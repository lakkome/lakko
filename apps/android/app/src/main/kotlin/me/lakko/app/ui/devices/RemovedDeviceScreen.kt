// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import me.lakko.app.devices.sync.RemovedDevice
import me.lakko.app.ui.auth.AuthScaffold
import me.lakko.app.ui.auth.BusyButton
import me.lakko.app.ui.auth.Lead

/**
 * "This device was removed" (docs/ux/second-device.md section 3.1).
 *
 * Replaces the whole signed-in app once another device of the account has
 * removed this one: no chat list behind it, no dismiss, no retry. The one
 * button is Settings' "Forget this device" ([onSignOut]): this device's keys
 * and local history for the account go, then the login screen. Drawn in the
 * auth screens' frame, since where it leads is sign-in.
 */
@Composable
internal fun RemovedDeviceScreen(
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var busy by remember { mutableStateOf(false) }
    AuthScaffold(title = RemovedDevice.TITLE, onBack = null, modifier = modifier) {
        Lead(RemovedDevice.BODY)
        Spacer(Modifier.height(24.dp))
        BusyButton(
            label = RemovedDevice.SIGN_OUT,
            busy = busy,
            onClick = {
                if (!busy) {
                    busy = true
                    onSignOut()
                }
            },
        )
    }
}
