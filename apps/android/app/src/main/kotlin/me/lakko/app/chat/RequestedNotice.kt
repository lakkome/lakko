// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.UiText

/**
 * `docs/ux/proposal-based-membership.md` section 2, "What 'tap, done' shows"
 * (seen live 2026-10-29): after a membership attempt the GROUP is read. When
 * the change is already in effect (this device swept it, or another
 * committer, a background sweep included, already did) there is no notice
 * at all: the member row changes in place and the timeline gets its named
 * row. Only a change that is not in effect shows its `... requested` notice,
 * and that notice goes by itself the moment the change lands.
 *
 * Decided from the group state through [landed], never from which code path
 * committed the change.
 *
 * Amended the same day: add, remove, the admin toggle and the ownership
 * transfer (owner or admin actions) show no notice in any case; only the
 * member row's annotation shows until they land. The one `... requested`
 * notice left is a plain member's own leave (`chat.leave.requested`).
 */
public class RequestedNotice {
    private var shown: UiText? = null
    private var landed: (() -> Boolean)? = null

    /** The `... requested` notice currently owed, or `null`. */
    public val notice: UiText? get() = shown

    /**
     * After the attempt returned: `null` (no notice) when [landed] already
     * holds, else [requested], which is then watched by [onRefresh].
     */
    public fun afterAttempt(requested: UiText, landed: () -> Boolean): UiText? {
        if (holds(landed)) {
            clear()
            return null
        }
        shown = requested
        this.landed = landed
        return requested
    }

    /**
     * The view re-read the group (each poll, each roster refresh). Returns the
     * notice that just stopped being owed because its change landed, so the
     * caller clears exactly that notice and no other; `null` otherwise.
     */
    public fun onRefresh(): UiText? {
        val check = landed ?: return null
        if (!holds(check)) return null
        val gone = shown
        clear()
        return gone
    }

    /** Forget the owed notice (a new attempt, or the panel cleared it). */
    public fun clear() {
        shown = null
        landed = null
    }

    /** A read that throws is not a landing: the notice stays. */
    private fun holds(check: () -> Boolean): Boolean = try {
        check()
    } catch (_: Throwable) {
        false
    }
}

/** This device's own leave is in effect: its membership in the group ended. */
public fun leaveApplied(isActive: Boolean?): Boolean = isActive == false
