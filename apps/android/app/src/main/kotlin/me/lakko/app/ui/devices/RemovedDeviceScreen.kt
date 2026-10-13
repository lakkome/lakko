// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.devices

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import me.lakko.app.auth.KeyResetSignOut
import me.lakko.app.devices.RemovalCopy
import me.lakko.app.l10n.t
import me.lakko.app.l10n.text
import me.lakko.app.ui.auth.AuthScaffold
import me.lakko.app.ui.auth.BusyButton
import me.lakko.app.ui.auth.Lead

/**
 * "Your account keys were reset" (docs/ux/identity-recovery.md section
 * 15.2; ADR-0073 section 3.5 and 4.2) — the same state as the removed
 * screen above with its own copy, shown in EVERY auth state instead of the
 * app, at launch too: this device's keys are not the account's keys any
 * more.
 *
 *   * [KeyResetSignOut.Kind.Reset]: the account was reset on another device.
 *     The deletion runs by itself ("Removing..." with a progress indicator,
 *     [KeyResetSignOut.Phase.Removing]); once done, the one button is
 *     **Sign in** ([onSignIn]), which goes to the anonymous sign-in screen.
 *   * [KeyResetSignOut.Kind.Removed] (ADR-0074, UX 18.2): another device of
 *     the account removed this one. "This device was removed", the same
 *     self-starting deletion, then **Sign in**.
 *   * [KeyResetSignOut.Kind.Mismatch]: the directory publishes other keys of
 *     the same generation (an anomaly, not a known reset). The second line
 *     says so and the one button is the removed screen's **Sign out**
 *     ([onSignOut]), which runs the deletion.
 *
 * No dismiss, no retry. Accessibility: the title and the body are one
 * announcement; the progress state is announced once, not on every tick;
 * the single button is the only focusable element.
 */
@Composable
internal fun KeysResetSignedOutScreen(
    signOut: KeyResetSignOut,
    onSignIn: () -> Unit,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var busy by remember { mutableStateOf(false) }
    val name = if (signOut.username.isBlank()) t("devices.signed_out.this_account") else "@${signOut.username}"
    val title = when (signOut.kind) {
        KeyResetSignOut.Kind.Removed -> t(RemovalCopy.REMOVED_TITLE)
        KeyResetSignOut.Kind.Reset, KeyResetSignOut.Kind.Mismatch -> t("devices.signed_out.title")
    }
    AuthScaffold(title = title, onBack = null, modifier = modifier) {
        when (signOut.kind) {
            // ADR-0074 / UX 18.2 - another device removed this one. The
            // deletion starts by itself; there is no button to tap first,
            // no "Was it you?" and no way back (a removal changes no keys).
            KeyResetSignOut.Kind.Removed -> when (signOut.phase) {
                KeyResetSignOut.Phase.Removing, KeyResetSignOut.Phase.AwaitingTap -> {
                    Lead(t(RemovalCopy.REMOVED_BODY_REMOVING, mapOf("name" to name)))
                    Spacer(Modifier.height(24.dp))
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(12.dp))
                        Text(t(RemovalCopy.REMOVED_PROGRESS), style = MaterialTheme.typography.bodyMedium)
                    }
                }
                KeyResetSignOut.Phase.Removed -> {
                    Lead(t(RemovalCopy.REMOVED_BODY_DONE, mapOf("name" to name)))
                    Spacer(Modifier.height(24.dp))
                    BusyButton(
                        label = t(RemovalCopy.REMOVED_SIGN_IN),
                        busy = false,
                        onClick = onSignIn,
                    )
                }
            }
            KeyResetSignOut.Kind.Reset -> when (signOut.phase) {
                KeyResetSignOut.Phase.Removing, KeyResetSignOut.Phase.AwaitingTap -> {
                    Lead(t("devices.signed_out.body_removing", mapOf("username" to name)))
                    Spacer(Modifier.height(24.dp))
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                        Spacer(Modifier.width(12.dp))
                        Text(t("devices.signed_out.removing"), style = MaterialTheme.typography.bodyMedium)
                    }
                }
                KeyResetSignOut.Phase.Removed -> {
                    Lead(t("devices.signed_out.body_removed", mapOf("username" to name)))
                    // ADR-0074 / UX 18.4 - why the account could not be
                    // taken back, when the form was open as that was learned.
                    signOut.notice?.let { notice ->
                        Text(
                            notice.text(),
                            color = MaterialTheme.colorScheme.error,
                            style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier
                                .padding(top = 12.dp)
                                .semantics { liveRegion = LiveRegionMode.Polite },
                        )
                    }
                    Spacer(Modifier.height(24.dp))
                    BusyButton(
                        label = t("devices.signed_out.sign_in"),
                        busy = false,
                        onClick = onSignIn,
                    )
                }
            }
            KeyResetSignOut.Kind.Mismatch -> {
                Lead(t("devices.signed_out.body_mismatch", mapOf("username" to name)))
                Spacer(Modifier.height(24.dp))
                when (signOut.phase) {
                    KeyResetSignOut.Phase.Removed -> BusyButton(
                        label = t("devices.signed_out.sign_in"),
                        busy = false,
                        onClick = onSignIn,
                    )
                    KeyResetSignOut.Phase.Removing, KeyResetSignOut.Phase.AwaitingTap -> BusyButton(
                        label = t("devices.removed.sign_out"),
                        busy = busy || signOut.phase == KeyResetSignOut.Phase.Removing,
                        onClick = {
                            if (!busy) {
                                busy = true
                                onSignOut()
                            }
                        },
                    )
                }
            }
        }
    }
}
