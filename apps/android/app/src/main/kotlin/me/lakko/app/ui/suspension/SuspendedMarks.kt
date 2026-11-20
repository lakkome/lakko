// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.suspension

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.produceState
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import me.lakko.app.l10n.t
import me.lakko.app.moderation.SuspensionMarks

/*
 * ADR-0099 step 4, UX `reporting-and-moderation.md` section 3 — the marks a
 * screen shows for ANOTHER person's suspended account. Each asks
 * [SuspensionMarks] when the screen shows the person; an answer it cannot
 * confirm in the moderation log reads as "not suspended".
 */

/** Whether [accountKeyB64] is confirmed suspended; `false` until the check answers. */
@Composable
public fun rememberAccountSuspended(accountKeyB64: String?): State<Boolean> =
    produceState(initialValue = false, accountKeyB64) {
        value = accountKeyB64 != null && SuspensionMarks.isSuspended(accountKeyB64)
    }

/**
 * The keys of [accountKeysB64] confirmed suspended so far: the set grows as
 * answers arrive, so nothing waits on the check.
 */
@Composable
public fun rememberSuspendedAmong(accountKeysB64: Set<String>): State<Set<String>> =
    produceState(initialValue = emptySet(), accountKeysB64) {
        val found = HashSet<String>()
        coroutineScope {
            accountKeysB64.map { key ->
                async {
                    if (SuspensionMarks.isSuspended(key)) {
                        synchronized(found) {
                            found += key
                            value = HashSet(found)
                        }
                    }
                }
            }.awaitAll()
        }
        // Every key answered: a key that is no longer suspended drops out.
        value = HashSet(found)
    }

/** UX section 3 — the banner at the top of a one-to-one with a suspended person. No action. */
@Composable
public fun SuspendedPeerBanner(name: String) {
    Text(
        t("suspended.banner", mapOf("name" to name)),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.surfaceVariant)
            .padding(horizontal = 16.dp, vertical = 8.dp),
    )
}
