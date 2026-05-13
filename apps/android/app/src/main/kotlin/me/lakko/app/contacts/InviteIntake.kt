// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Tiny state holder for a pending invite-link slug an external
 * deep-link just brought into the app.
 *
 * `MainActivity` reads the launching `Intent`'s `data` field on
 * `onCreate` + `onNewIntent`, extracts the slug from
 * `https://lakko.me/i/<slug>` URLs, and hands it to [publish].
 * The Compose router observes [pendingInviteSlug] — when it
 * becomes non-null it navigates to the [InviteLandingScreen]
 * for that slug (1.15e11b's surface).
 *
 * [consume] clears the slot. The router calls it once it has
 * actually mounted the landing screen so a subsequent
 * `onNewIntent` for a different invite can land cleanly.
 *
 * **Process-scoped state.** A cold-start deep-link launches
 * MainActivity, which constructs a fresh InviteIntake — no
 * persistence across process death. If the user receives an
 * invite URL, taps it, the app crashes before the landing
 * screen mounts, then they re-tap from their messaging app,
 * the deep-link lands cleanly the second time.
 *
 * **Pre-auth queue is intentional**: if the user is in
 * `AuthState.Anon` when the deep link arrives we still queue
 * the slug. The router can either surface a "log in to accept
 * this invite" affordance OR auto-launch the landing screen
 * once auth flips to Session. For 1.15e11a/b the surface lands
 * pre-Session by routing through Login first; 1.15e11b's
 * `MainActivity` integration wires the post-Session pickup.
 */
public class InviteIntake {

    private val mutableSlug: MutableStateFlow<String?> = MutableStateFlow(null)

    /** Slug from the most recent unconsumed deep-link, or
     *  `null`. The Compose router collects this and navigates
     *  to the [InviteLandingScreen] when non-null. */
    public val pendingInviteSlug: StateFlow<String?> = mutableSlug.asStateFlow()

    /** Publish a new slug — called from
     *  [me.lakko.app.MainActivity.handleInviteIntent]. Replaces
     *  any prior unconsumed value (the user has clearly chosen
     *  to act on the newer link). */
    public fun publish(slug: String) {
        mutableSlug.value = slug
    }

    /** Clear the slot. Called by the router once the landing
     *  screen has mounted + read the slug. */
    public fun consume() {
        mutableSlug.value = null
    }
}
