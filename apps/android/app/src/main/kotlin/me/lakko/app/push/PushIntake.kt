// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * Tiny state holder for a pending push-wake hint a notification
 * tap just brought into the app (Phase 1.20d2 — mirror of
 * [me.lakko.app.contacts.InviteIntake]).
 *
 * `MainActivity` reads the launching `Intent`'s wake extras on
 * `onCreate` + `onNewIntent` (see
 * [LakkoMessagingService.EXTRA_WAKE_GROUP] /
 * [LakkoMessagingService.EXTRA_WAKE_POINTER]) and hands the
 * group-pseudonym hex to [publish]. The Compose router observes
 * [pendingWake]; when it becomes non-null the
 * `SessionRoot` `LaunchedEffect` checks the local
 * `ChatRepository.threads` list for that group, then routes to
 * the `Chat(groupIdHex)` destination if the group is known
 * (otherwise it falls back to the chat list — a stale wake is
 * benign).
 *
 * [consume] clears the slot. The router calls it once it has
 * acted on the wake so a subsequent `onNewIntent` for a
 * different push can land cleanly.
 *
 * ## Process-scoped state
 *
 * A cold-start notification tap launches MainActivity, which
 * constructs a fresh `PushIntake` — no persistence across
 * process death. If the wake arrives, the user taps, the app
 * crashes before SessionRoot mounts, then they re-tap from the
 * notification shade, the deep-link lands cleanly the second
 * time.
 *
 * ## Pre-auth queue is intentional
 *
 * If the user is in `AuthState.Anon` when the wake tap lands we
 * still queue the group hex. The router can either surface a
 * "log in to view this thread" affordance OR auto-route once
 * auth flips to Session. The current `SessionRoot` consumer
 * only fires post-Session — pre-auth wakes are dropped on the
 * floor when the user logs in (the slot's last value lingers
 * but is consumed-and-discarded by the next mount).
 *
 * ## Why a separate intake from `InviteIntake`?
 *
 * The two carry different payload shapes (slug vs group-hex +
 * optional pointer) and have different consumer surfaces
 * (`InviteLandingScreen` vs `ChatScreen`). Sharing one intake
 * would force the consumer to dispatch on the variant, which is
 * worse than two parallel holders for two parallel deep-link
 * sources.
 */
public class PushIntake {

    /** Discriminated wake payload — only the group hex is
     *  load-bearing; the pointer is optional and used purely as
     *  a cursor hint for the next mailbox poll. */
    public data class Wake(
        /** Group routing pseudonym hex (matches
         *  [me.lakko.app.chat.ChatThread.groupIdHex]). The push
         *  wake's `g` field carries this verbatim. */
        val groupHex: String,
        /** Optional DS journal pointer the wake hinted at —
         *  Phase-1 unused on the consume side (the chat poll
         *  loop maintains its own cursor); preserved here so a
         *  future "jump to specific message id" surface can
         *  consume it without re-plumbing the Intent extras. */
        val pointer: Long?,
    )

    private val mutableWake: MutableStateFlow<Wake?> = MutableStateFlow(null)

    /** Wake from the most recent unconsumed notification tap, or
     *  `null`. The Compose router collects this and navigates
     *  to the relevant chat thread when non-null. */
    public val pendingWake: StateFlow<Wake?> = mutableWake.asStateFlow()

    /** Publish a new wake hint — called from
     *  [me.lakko.app.MainActivity.handleWakeIntent]. Replaces
     *  any prior unconsumed value (the user has clearly chosen
     *  to act on the newer notification). */
    public fun publish(wake: Wake) {
        mutableWake.value = wake
    }

    /** Clear the slot. Called by the router once it has acted
     *  on the wake (routed to chat / fell back to chat list). */
    public fun consume() {
        mutableWake.value = null
    }
}
