// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.profile

/**
 * docs/ux/profile-screen.md §6 — leaving Settings > Profile with unsaved
 * changes by a way the screen does not draw itself (a tapped link, a
 * notification that opens a chat). The screen registers [ask] while it is
 * shown; the app's navigation for those cases goes through [run], which
 * either navigates at once or lets the screen show `Save changes?` and
 * navigates only on `Save` (once it succeeded) or `Don't save`.
 */
public object ProfileLeaveGuard {
    @Volatile
    internal var ask: ((proceed: () -> Unit) -> Unit)? = null

    /** Run [navigate] now, or once the person chose to leave. */
    public fun run(navigate: () -> Unit) {
        val guard = ask
        if (guard == null) navigate() else guard(navigate)
    }
}
