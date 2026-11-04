// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.l10n.UiText

/**
 * ADR-0045 UX §6.2 — the end of your own membership.
 *
 * Android mirror of `apps/web/src/chat/membership-ended.ts`. Copy and rules
 * come from `docs/ux/proposal-based-membership.md` §6.2, thirteen subsections.
 *
 * ## What this still covers (ADR-0087 section 9)
 *
 * A person who is out of a group loses it: `MembershipEnd.kt` holds that. What
 * is below is for what does not end that way: a device whose own leaf was
 * retired while the account stays in the group, which keeps its history and
 * this presentation, and a one-to-one, which no commit can take a person out
 * of. Everything below that argues the transcript stays argues it for those.
 *
 * ## What this is for
 *
 * The commit that removes your own leaf arrives on an ordinary poll, merges,
 * and the group goes permanently dead in the MLS core. Before this module the
 * client carried on as though nothing had happened: the chat stayed in the
 * list, the composer stayed enabled, the members panel kept offering
 * `Leave group` on a group already left, and the first thing the user learned
 * was that sending failed.
 *
 * ## Why a plain module and not a handful of `if`s in the composable
 *
 * The host unit suite renders no composable and cannot load the JNI library, so
 * every rule lives here where it is executable — the same split
 * [PendingRequests.kt], [OwnershipTransfer.kt], [CommitterBeacon.kt] and
 * [SplitPoll.kt] already use. `ChatScreen` and `ChatViewModel` render what this
 * decides.
 *
 * ## The one asymmetry the whole design falls out of
 *
 * After the removal the client can still **read** everything about the group
 * and can no longer **do** anything with it. `members()`, `epoch`,
 * `memberCount`, `ownLeafIndex` and `groupRoles()` are plain reads of the
 * public group with no state gate, while `createApplicationMessage`,
 * `exportSecret` and every further decrypt raise `UseAfterEviction`. That is
 * not a limitation to work around — it is precisely the shape of the correct
 * UI, and it is why the answer to *keep the history?* is yes and the answer to
 * *keep the composer?* is no.
 *
 * Two consequences worth stating because they are easy to get wrong:
 *
 *   * **The state is terminal and survives a restart.** OpenMLS calls
 *     `Inactive` "a terminal state that the group can not exit from", and
 *     `write_group_state` runs before the merge, so a snapshot taken afterwards
 *     restores an inactive group.
 *   * **The local view is frozen for good.** `unprotect_message` refuses on
 *     `!is_active()` before it looks at the message at all, and the refusal is
 *     NOT epoch-scoped — so rows still sitting on the DS are unreadable
 *     forever, even ones sealed at an epoch this device still holds keys for.
 *     The roster, the epoch and the role table stop at the values that commit
 *     produced and never move again.
 *
 * ## Two orderings that must not be reordered
 *
 * 1. [membershipState] consults `isActive` FIRST and alone; the leave record
 *    only ever narrows the copy afterwards.
 * 2. An unknown / torn `isActive` reads as **active**, never as ended.
 *
 * Both are argued at [membershipState]. The measured discrimination evidence —
 * 32 deliberate regressions, each RUN and each recorded with its observed
 * failure — lives in `MembershipEndedTest`'s own header.
 */

// ---------------------------------------------------------------------------
// Copy (§6.2) — one definition each, so the composable cannot drift from it
// ---------------------------------------------------------------------------

/**
 * §6.2.3 — the narrowing, applied only on positive evidence.
 *
 * True of exactly one of the two cases, which is why it needs the durable
 * record and why [ENDED_REMOVED] is the fallback rather than this.
 */
internal val ENDED_LEFT: UiText = UiText("chat.ended.left")

/**
 * §6.2.3 — the neutral string, and the FALLBACK.
 *
 * This inverts the usual reflex to treat the neutral case as the special one.
 * "You are no longer a member of this group" is true of someone who left as
 * well as of someone who was removed; "you left this group" is true of only
 * one. So a device that lost the record — the ADR-0040 crash window, a
 * reinstall, a restore — degrades to a sentence that is still correct rather
 * than to one that is wrong.
 *
 * And nothing here says **who**. Not because the information is unobtainable
 * (the removing proposal's author leaf is in hand inside the core's own gate)
 * but because a client that names a remover it inferred is one bad inference
 * away from a serious accusation, and because §5.1's discipline — state
 * inaction, name nobody, blame nobody — is the same discipline whether the news
 * is a wait or an ending.
 */
internal val ENDED_REMOVED: UiText = UiText("chat.ended.no_longer_member")

/**
 * §6.2.5 line 2 — the standing consequence, identical in both cases.
 *
 * Written to claim exactly what is claimed and no more. A device that was
 * offline across its own removal loses the tail of its history silently and
 * permanently (§6.2.6): the decrypt refusal is not epoch-scoped, so rows still
 * on the DS at the moment the commit is applied are unreachable forever. The
 * design deliberately does not mention that — the user cannot tell how much
 * they are missing and no client can tell them — and *the messages you already
 * received are still here* stays true either way.
 */
internal val ENDED_CONSEQUENCE: UiText = UiText("chat.ended.consequence")

/** §6.2.4 — the chat-list row's one muted secondary line. Deliberately neutral
 *  between left and removed: the list is a glance surface and the distinction
 *  belongs in the transcript. */
internal val CHAT_LIST_ENDED_LINE: UiText = UiText("chats.row.ended")

/** §6.2.5 — the static bar that REPLACES the composer, in the same slot so the
 *  layout does not jump. */
internal val COMPOSER_WITHDRAWN: UiText = UiText("chat.ended.composer")

/** §6.2.5 — the muted label above the preserved draft. */
internal val UNSENT_DRAFT_LABEL: UiText = UiText("chat.ended.unsent_draft")

/**
 * §6.2.7 — one line at the top of the frozen roster.
 *
 * A statement about the **list**, not about any row in it, and that is
 * load-bearing: the role table is untouched by a `Remove` — a removal rewrites
 * no `GroupContext` — so a removed admin's frozen roster still names them an
 * admin, and nothing in the UI may present that as a live role.
 * *"Stopped updating"* is the literal truth of the frozen local view rather
 * than a euphemism for it.
 */
internal val FROZEN_ROSTER_HEADER: UiText = UiText("chat.members.frozen")

/** §6.2.4 — the one control still honest once you are out. */
internal val DELETE_CHAT_CONTROL: UiText = UiText("chat.delete.control")

/** §6.2.4 — confirmation title. */
internal val DELETE_CHAT_TITLE: UiText = UiText("chat.delete.title")

/**
 * §6.2.4 — confirmation body.
 */
internal val DELETE_CHAT_BODY: UiText = UiText("chat.delete.body")

// ---------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------

/** Whether this device is still in the group, and — if not — how that happened,
 *  as far as this device can honestly tell. */
internal sealed interface MembershipState {
    /** Still a member, as far as this device can tell. Also the answer for a
     *  torn read — see [membershipState]. */
    data object Active : MembershipState

    /**
     * Terminal. [left] is `true` only on positive local evidence that this
     * device asked to go (§6.2.3); it narrows the COPY and never decides the
     * state.
     */
    data class Ended(val left: Boolean) : MembershipState
}

/**
 * Read the core's own membership predicate, or `null` when the handle cannot be
 * read at all.
 *
 * `Group.isActive()` is the variable `createApplicationMessage`, `exportSecret`
 * and `unprotect_message` each consult before refusing, so a composer gated on
 * it is withdrawn for the reason the send would have failed. The available
 * alternative — *no member record whose signature key is ours* — is sound today
 * and would work, but it rests on a leaf's signature key being the account's
 * signature key, which ADR-0022 names as an open follow-up (the Phase-2b
 * per-device-leaf resolution). The day that lands, the inference breaks
 * silently, in the dangerous direction, in three clients at once. `isActive` is
 * about *this handle* and is immune to it.
 *
 * Taken as a lambda rather than a `Group` so the rule stays free of the FFI
 * type and host-JVM testable, exactly as [GroupPushRouting.RotationRole] and
 * `beaconEmitVerdict`'s injected throttle already are. The generated binding is
 * `@Throws(FfiException)`, so a torn handle is a reachable case rather than a
 * hypothetical — and it reads as `null` rather than as `false`, see
 * [membershipState].
 */
internal fun readIsActive(read: () -> Boolean): Boolean? =
    try {
        read()
    } catch (_: Throwable) {
        null
    }

/**
 * Decide the membership state.
 *
 * **The order of the two reads is the rule.** [isActive] is consulted first and
 * alone decides whether the membership has ended; [leaveRequestedAtMs] only
 * ever *narrows the copy* afterwards. Reading the record first would make
 * *asked to leave* mean *left*, which is §6.2.11's forbidden optimistic
 * transition: between publishing the request and the commit merging you are
 * still fully in the group — still receiving, still decrypting — and §3's
 * argument against an optimistic `Remove` is the same argument pointed at
 * yourself.
 *
 * **An unknown [isActive] fails toward [MembershipState.Active], and that is
 * deliberate even though it is the un-reflexive direction.** Fail-closed is the
 * house rule for a claim about *cryptographic capability*; this is a claim
 * about *fact*. A torn handle is not evidence of an eviction, and asserting
 * "You are no longer a member of this group." on one is a false statement the
 * user has no way to check. Getting it wrong the other way costs only the
 * status quo: the composer stays and a send fails, which is exactly what
 * happened before this module existed.
 */
internal fun membershipState(
    isActive: Boolean?,
    leaveRequestedAtMs: Long?,
): MembershipState {
    if (isActive != false) return MembershipState.Active
    return MembershipState.Ended(left = leaveRequestedAtMs != null)
}

/**
 * The standing gate every background discipline consults (§6.2.8).
 *
 * A gate rather than a one-shot, because three of the five disciplines are
 * things that must *keep not happening*: the poll is re-entered from several
 * call sites, the rotation decision is re-evaluated per commit batch, and the
 * call buttons are re-composed on every roster change.
 */
internal fun membershipEnded(state: MembershipState): Boolean =
    state is MembershipState.Ended

// ---------------------------------------------------------------------------
// §6.2.5 — the terminal row and the composer
// ---------------------------------------------------------------------------

/** The two lines of the terminal timeline row. */
internal data class TerminalRowCopy(
    /** [ENDED_LEFT] or [ENDED_REMOVED]. */
    val headline: UiText,
    /** [ENDED_CONSEQUENCE]. */
    val consequence: UiText,
)

/**
 * The terminal row, or `null` while the membership holds.
 *
 * It goes in the **timeline**, not in a banner, and §10 already says why:
 * *only outcomes belong in the timeline* — and this is an outcome, one that
 * happened at a definite point in the transcript. A row marks *where* the
 * history stops, survives scrollback and needs no dismissal state.
 */
internal fun terminalRow(state: MembershipState): TerminalRowCopy? =
    when (state) {
        MembershipState.Active -> null
        is MembershipState.Ended -> TerminalRowCopy(
            headline = if (state.left) ENDED_LEFT else ENDED_REMOVED,
            consequence = ENDED_CONSEQUENCE,
        )
    }

/** The chat-list row's secondary line, or `null` while the membership holds. */
internal fun chatListLine(state: MembershipState): UiText? =
    if (state is MembershipState.Ended) CHAT_LIST_ENDED_LINE else null

/** What occupies the composer's slot. */
internal sealed interface ComposerSlot {
    /** The ordinary composer. */
    data object Composer : ComposerSlot

    /**
     * The composer is GONE and this bar stands in its place.
     *
     * [draft] is the unsent text, verbatim, or `null` when there was nothing
     * worth keeping; [draftLabel] is [UNSENT_DRAFT_LABEL] exactly when [draft]
     * is non-null.
     */
    data class Withdrawn(
        val notice: UiText,
        val draft: String?,
        val draftLabel: UiText?,
    ) : ComposerSlot
}

/**
 * Decide the composer's slot (§6.2.5).
 *
 * **Replaced, not disabled, and the distinction is principled.** §5.3 argues a
 * pending row's `Remove` must stay *visible but disabled*, because hiding it
 * would read as the permission having vanished when it is merely suspended. The
 * permission here has vanished, permanently and by design. So the rule is:
 * **disable while the state is transient, remove when it is terminal** — and a
 * disabled Send button on a group you will never be in again is a control
 * promising a future it does not have.
 *
 * **The draft is preserved, and NOT in an input.** The commit lands during a
 * poll, which can be mid-sentence, and silently discarding what the user typed
 * is a small data loss with no upside. But the obvious fix — leave the text
 * field there, disabled — is wrong on this platform in particular: a disabled
 * `OutlinedTextField` is not selectable, so the user would see their words and
 * be unable to copy them out. The cross-platform shape is therefore
 * **selectable, non-editable text**; on Compose that is a `SelectionContainer`
 * around a `Text`, never a disabled field.
 *
 * A blank-or-whitespace draft yields no block at all: an `Unsent draft` heading
 * over three spaces is furniture pretending to be data. The draft that IS kept
 * is preserved **verbatim**, leading and trailing whitespace included — it is
 * the user's text, and this decides whether to show it, not what it should have
 * been.
 */
internal fun composerState(state: MembershipState, draft: String): ComposerSlot {
    if (!membershipEnded(state)) return ComposerSlot.Composer
    val worthKeeping = draft.isNotBlank()
    return ComposerSlot.Withdrawn(
        notice = COMPOSER_WITHDRAWN,
        draft = if (worthKeeping) draft else null,
        draftLabel = if (worthKeeping) UNSENT_DRAFT_LABEL else null,
    )
}

/** Whether the per-message action menu is offered at all. */
internal enum class MessageActions { AVAILABLE, WITHDRAWN }

/**
 * Decide the per-message action menu — Edit, Delete, React (§6.2.5's rule,
 * reached from the other end).
 *
 * **Withdrawn, not disabled, and it is the composer's own question.** All three
 * actions compose an MLS application message, so all three are refused by
 * `createApplicationMessage`'s `is_operational()` gate — the *same* gate, at
 * the *same* instant, for the *same* permanent reason as Send. §5.3's
 * visible-but-disabled rule is for a permission that is merely *suspended*; a
 * greyed React item on a group that can never become live again is a control
 * promising a future it does not have, which is exactly the shape §6.2.5
 * rejects for the composer.
 *
 * **Why a function here and not a `&&` at the call site.** Two gates over one
 * predicate drift when somebody later adds a condition to one of them, and the
 * drift is silent in both directions: an ungated menu offers three actions that
 * cannot work, and an over-gated one hides working controls. Both this and
 * [composerState] therefore route through [membershipEnded], and the test
 * enumerates every state and asserts they agree.
 *
 * **What this does NOT withdraw is the information.** A reaction *chip* is
 * rendered content — part of the frozen transcript §6.2.6 keeps whole — so the
 * counts stay. What goes is the affordance that would toggle one.
 */
internal fun messageActionsState(state: MembershipState): MessageActions =
    if (membershipEnded(state)) MessageActions.WITHDRAWN else MessageActions.AVAILABLE

// ---------------------------------------------------------------------------
// §6.2.8 — the one-shot half of the teardown
// ---------------------------------------------------------------------------

/**
 * The teardown steps that are **effects** rather than gates.
 *
 * §6.2.8 lists five disciplines and they are not all the same shape, so they
 * are not modelled as though they were. Three of the five — the poll loop, the
 * shrink rotation and the exporter-backed call buttons — are pure **gates**:
 * there is nothing to dispose, only something that must keep not happening, and
 * [membershipEnded] is what they consult. These two are the ones with an effect
 * to fire, and firing an effect more than once is what [EndedTeardownLatch]
 * exists to prevent.
 */
internal enum class EndedTeardownStep {
    /**
     * The beacon emitter runs on its own `repeatOnLifecycle` ticker and
     * composes an application message, which now throws. A candidate that
     * cannot commit must not claim it can: a beacon means *able to sweep*, and
     * an evicted device can never sweep again. (The same rule that keeps a
     * backgrounded device from emitting, reached from the other direction —
     * there the device could sweep and is not running; here it is running and
     * never can.) The emit path is gated too ([BeaconEmitVerdict.MEMBERSHIP_ENDED]),
     * so stopping the ticker is what stops the wasted wake while the gate is
     * what makes a surviving ticker harmless.
     */
    STOP_BEACON_TICKER,

    /**
     * The interesting one. ADR-0043 recorded that an evicted member's push-gw
     * row *"is never deleted — nobody else can delete it and no client has 'you
     * were removed' handling"*, so it goes **inert** rather than away. This is
     * that handling: the evicted device can delete its own row, and
     * [me.lakko.app.push.GroupPushRouting.retireOldPushGid] already does
     * exactly the right three things. It closes the other half of G-36 residual
     * 2 — rotation stops the group notifying the old id; this stops the
     * ex-member being subscribed to anything at all.
     *
     * **The honest bound:** it needs the evicted device to come online and run
     * it, so it *complements* rotation and replaces nothing.
     */
    RETIRE_PUSH_SUBSCRIPTION,
}

/** The canonical set, so a caller cannot quietly implement one of two. */
internal val ENDED_TEARDOWN_STEPS: List<EndedTeardownStep> =
    listOf(EndedTeardownStep.STOP_BEACON_TICKER, EndedTeardownStep.RETIRE_PUSH_SUBSCRIPTION)

/**
 * Which one-shot teardown steps are owed right now.
 *
 * Returns `[]` while the membership holds, and `[]` again once every step has
 * been run — so the caller can drive it from a flow collector that re-runs for
 * unrelated reasons without re-firing a network call each time. A
 * partially-completed teardown resumes rather than restarting, which matters
 * because the two steps fail independently: the ticker stop is local and cannot
 * fail, while the push retire is bounded network IO that can.
 */
internal fun pendingTeardownSteps(
    state: MembershipState,
    alreadyRun: Set<EndedTeardownStep>,
): List<EndedTeardownStep> {
    if (state !is MembershipState.Ended) return emptyList()
    return ENDED_TEARDOWN_STEPS.filter { it !in alreadyRun }
}

/**
 * [pendingTeardownSteps], but it **claims** what it returns in the same atomic
 * step.
 *
 * This is what the driver must use, and the reason is a defect that
 * [pendingTeardownSteps] alone cannot prevent. One of the two steps is
 * suspending (the push retire is bounded network IO), so marking it done only
 * after the await leaves a window in which the driver re-runs — and it re-runs
 * on every repository emission, for reasons that have nothing to do with the
 * eviction — reads the step as still owed, and fires a second concurrent
 * unsubscribe. "Exactly once" has to be decided *before* the first suspension
 * point, not after the last one.
 *
 * **On this platform that costs a real lock.** The web reference can use a bare
 * `Set` because its event loop is single-threaded; the iOS mirror gets it
 * nearly free from `@MainActor`. Android's push work runs on `Dispatchers.IO`,
 * which is genuinely parallel, so the read-modify-write is `synchronized` — and
 * the test that proves it fans out on `Dispatchers.IO` rather than on a
 * confined dispatcher, which on this codebase has previously MASKED exactly
 * this class of defect.
 *
 * A step that FAILS is handed back with [release], so the next pass retries it.
 * Claim-then-release rather than mark-on-success is deliberate: the two
 * orderings differ only under concurrency, and that is precisely the case this
 * exists for.
 */
internal class EndedTeardownLatch {

    private val done: MutableSet<EndedTeardownStep> = mutableSetOf()

    /** Atomically take every step still owed. Returns `[]` while the membership
     *  holds, and `[]` for any step another caller already took. */
    fun claim(state: MembershipState): List<EndedTeardownStep> = synchronized(done) {
        val owed = pendingTeardownSteps(state, done)
        done.addAll(owed)
        owed
    }

    /** Hand a claimed step back after it failed, so a later pass retries it. */
    fun release(step: EndedTeardownStep): Unit = synchronized(done) {
        done.remove(step)
        Unit
    }

    /** ADR-0087 section 9 — the membership came back (a retired leaf was
     *  re-admitted): what ran for the old handle is owed for the new one. */
    fun reset(): Unit = synchronized(done) {
        done.clear()
        Unit
    }

    /** Snapshot of what has been claimed — diagnostics and tests only. */
    fun completed(): Set<EndedTeardownStep> = synchronized(done) { done.toSet() }
}

/**
 * Run the §6.2.8 one-shot teardown once.
 *
 * The production seam: [ChatViewModel] binds [stopBeaconTicker] to the ticker's
 * own run flag and [retirePushSubscription] to the repository, and the host
 * suite binds fakes. Every claim is taken from [latch] **before** the first
 * suspension point; a step whose effect throws or reports failure is released
 * so the next pass retries it.
 *
 * Never throws: a teardown that propagated would take out whatever collector is
 * driving it, and the whole point is that it keeps being re-offered until it
 * lands.
 */
internal suspend fun runEndedTeardown(
    state: MembershipState,
    latch: EndedTeardownLatch,
    stopBeaconTicker: () -> Unit,
    retirePushSubscription: suspend () -> Boolean,
) {
    // Claimed synchronously, in one atomic step, for the whole batch — so a
    // concurrent re-entry gets nothing even though the retire below suspends.
    for (step in latch.claim(state)) {
        when (step) {
            EndedTeardownStep.STOP_BEACON_TICKER ->
                try {
                    stopBeaconTicker()
                } catch (_: Throwable) {
                    latch.release(step)
                }

            EndedTeardownStep.RETIRE_PUSH_SUBSCRIPTION ->
                try {
                    if (!retirePushSubscription()) latch.release(step)
                } catch (_: Throwable) {
                    latch.release(step)
                }
        }
    }
}

// ---------------------------------------------------------------------------
// §6.2.3 / §9 — the durable `leaveRequestedAt` record
// ---------------------------------------------------------------------------

/**
 * Where "this device published a self-targeted leave" lives, per
 * `(account, group)`.
 *
 * **It has to be its own durable field, and the reason is that the failure is
 * silent.** The client already knows locally whether it asked — a device that
 * requested a leave wrote an observation under `remove:{ownLeaf}` with
 * `own = true` — but that record is pruned by the same pass that makes the
 * question askable. [prunePendingRequestObservations]' whole discipline is to
 * drop an observation once the core no longer holds the request, and the sweep
 * that removes you is exactly what empties the core's pending set. So the poll
 * batch that carries your eviction also destroys the only evidence that you
 * asked for it, and the order in which those two happen is an implementation
 * detail no test pins.
 *
 * Hence: written at **publish**, never pruned with the pending sweep, and
 * discarded only with the group record itself (`Delete this chat`) or the
 * account (`forgetDevice`).
 *
 * Not encrypted, same posture as [PendingRequestStore] and
 * [BeaconObservationStore]: a millisecond timestamp of an action the user took
 * on this device, and no key material.
 */
internal interface LeaveRecordStore {
    fun load(accountIdB64: String, groupIdHex: String): Long?

    fun save(accountIdB64: String, groupIdHex: String, atMs: Long)

    fun clear(accountIdB64: String, groupIdHex: String)

    fun clearForAccount(accountIdB64: String)
}

/**
 * Record that this device published a self-targeted leave request.
 *
 * A timestamp rather than a boolean costs nothing and is strictly more useful.
 * The FIRST stamp wins, for the same reason [recordPendingRequestObservation]'s
 * does: a re-request is the same intent, not a new one.
 *
 * Never throws — a store failure costs the *narrowing*, and the fallback string
 * ([ENDED_REMOVED]) is the one that is true either way.
 */
internal fun recordLeaveRequested(
    store: LeaveRecordStore,
    accountIdB64: String,
    groupIdHex: String,
    atMs: Long,
) {
    try {
        if (store.load(accountIdB64, groupIdHex) != null) return
        store.save(accountIdB64, groupIdHex, atMs)
    } catch (_: Throwable) {
        // the narrowing is best-effort; the fallback string is still true
    }
}

/** Read the stamp, or `null` when absent or unreadable. A corrupt row degrades
 *  to the removed copy, which is still true. */
internal fun loadLeaveRequestedAt(
    store: LeaveRecordStore,
    accountIdB64: String,
    groupIdHex: String,
): Long? =
    try {
        store.load(accountIdB64, groupIdHex)
    } catch (_: Throwable) {
        null
    }

/**
 * The production [LeaveRecordStore] — one plain `SharedPreferences` file.
 *
 * Row key: `"leave-requested-v1:" + accountIdB64 + NUL + groupIdHex` (the NUL
 * written as the Kotlin escape `\u0000`; a RAW NUL in the source would make git
 * classify this file as binary — ADR-0042 MEDIUM-3).
 *
 * Row value: the millisecond stamp as a decimal string. Never throws: a corrupt
 * or absent row reads as absent, which degrades the copy to [ENDED_REMOVED] —
 * still true of someone who left.
 */
internal class SharedPrefsLeaveRecordStore(context: Context) : LeaveRecordStore {

    private val appContext: Context = context.applicationContext

    private fun prefs(): SharedPreferences =
        appContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)

    override fun load(accountIdB64: String, groupIdHex: String): Long? =
        try {
            decodeStamp(prefs().getString(rowKey(accountIdB64, groupIdHex), null))
        } catch (_: Throwable) {
            null
        }

    override fun save(accountIdB64: String, groupIdHex: String, atMs: Long) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                prefs().edit().putString(rowKey(accountIdB64, groupIdHex), atMs.toString()).apply()
            } catch (_: Throwable) {
                // best-effort — see [recordLeaveRequested]
            }
        }
    }

    override fun clear(accountIdB64: String, groupIdHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                prefs().edit().remove(rowKey(accountIdB64, groupIdHex)).apply()
            } catch (_: Throwable) {
                // best-effort
            }
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        try {
            val prefix = KEY_PREFIX + accountIdB64 + ROW_SEPARATOR
            val p = prefs()
            val victims = p.all.keys.filter { it.startsWith(prefix) }
            if (victims.isEmpty()) return
            val editor = p.edit()
            for (k in victims) editor.remove(k)
            editor.apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }

    internal companion object {
        private const val PREFS_NAME = "lakko-leave-requested"
        private const val KEY_PREFIX = "leave-requested-v1:"
        private const val ROW_SEPARATOR = "\u0000"

        private fun rowKey(accountIdB64: String, groupIdHex: String): String =
            KEY_PREFIX + accountIdB64 + ROW_SEPARATOR + groupIdHex

        /** Split out so the one rule that can actually be wrong — what a
         *  corrupt row reads as — is host-testable without `SharedPreferences`. */
        internal fun decodeStamp(raw: String?): Long? = raw?.toLongOrNull()
    }
}
