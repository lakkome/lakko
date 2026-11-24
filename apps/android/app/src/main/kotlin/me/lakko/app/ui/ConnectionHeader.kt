// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import me.lakko.app.l10n.t
import me.lakko.app.transport.ConnectionState
import me.lakko.app.transport.connectionAnnouncementKey
import me.lakko.app.transport.connectionHeaderKey

/**
 * `docs/ux/connection-state.md` section 1: the header text while this device
 * cannot reach the Lakko servers ("Waiting for network...", then
 * "Connecting..."), or `null` when the normal title or subtitle stands. Starts
 * listening to the system's network on first use.
 */
@Composable
public fun connectionHeaderText(): String? {
    val context = LocalContext.current
    LaunchedEffect(Unit) { ConnectionState.install(context) }
    val phase by ConnectionState.phase.collectAsStateWithLifecycle()
    return connectionHeaderKey(phase)?.let { t(it) }
}

/**
 * Section 5: a zero-size polite live region that holds the words of the last
 * transition, so TalkBack speaks each change once and nothing while the state
 * stays. Empty until the first transition.
 */
@Composable
public fun ConnectionAnnouncer() {
    val entered by ConnectionState.announcement.collectAsStateWithLifecycle()
    val words = entered?.let { t(connectionAnnouncementKey(it)) }
    Box(
        Modifier.size(0.dp).semantics {
            liveRegion = LiveRegionMode.Polite
            if (words != null) contentDescription = words
        },
    )
}
